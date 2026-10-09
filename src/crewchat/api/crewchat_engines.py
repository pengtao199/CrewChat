"""Engines: 本机执行引擎的管理（自研 `/api/crewchat/engines`）。

数据 = cc-switch 的工具层扫描（安装分布/版本/来源/冲突）+ 我们执行器注册表
（kind/名称/被哪些 Agent 引用）。安装/更新**会真正执行系统命令**。见 `AGENTS/gateway.md`。

性能（产品要求：设置页秒开）：
- **本地扫描**便宜但会 spawn 进程 → 缓存 5 分钟。
- **版本探测**要联网（慢且易抖动）→ 缓存 6 小时，**后台刷新**，主请求绝不等它。
- 进入页面即时返回上次结果；只有显式 `?refresh=true` 才同步重扫。装/更新后失效缓存。
"""

from __future__ import annotations

import re
import threading
import time
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..core.config import get_settings
from ..core.engines import get_engine_tools
from ..executors import list_executors
from ..orchestration import session_for

router = APIRouter()

_SCAN_TTL = 300.0  # 本地扫描缓存
_VERSIONS_TTL = 6 * 3600.0  # 联网版本缓存

_SCAN: dict[str, Any] = {"ts": 0.0, "data": None}
# data=None 表示"尚未探测过"（哨兵）：time.monotonic() 是开机时长，不能拿 0 当"很久以前"。
_VERSIONS: dict[str, Any] = {"ts": 0.0, "data": None}
_versions_lock = threading.Lock()
_versions_busy = False


def _do_refresh_versions() -> None:
    global _versions_busy
    try:
        data = {str(v.get("name")): v for v in get_engine_tools(get_settings()).versions()}
        with _versions_lock:
            _VERSIONS["data"] = data
            _VERSIONS["ts"] = time.monotonic()
    except Exception:  # noqa: BLE001
        pass
    finally:
        _versions_busy = False


def _refresh_versions(blocking: bool) -> None:
    global _versions_busy
    if blocking:
        _do_refresh_versions()
        return
    if _versions_busy:
        return
    _versions_busy = True
    threading.Thread(target=_do_refresh_versions, daemon=True).start()


def _scan(force: bool) -> list[dict[str, Any]]:
    now = time.monotonic()
    if not force and _SCAN["data"] is not None and (now - _SCAN["ts"]) < _SCAN_TTL:
        return _SCAN["data"]
    try:
        data = get_engine_tools(get_settings()).scan()
    except Exception:  # noqa: BLE001 — 核心不可用时沿用上次结果
        data = _SCAN["data"] or []
    _SCAN["data"] = data
    _SCAN["ts"] = now
    return data


def _versions(force: bool) -> dict[str, Any]:
    never = _VERSIONS["data"] is None
    stale = never or (time.monotonic() - _VERSIONS["ts"]) > _VERSIONS_TTL
    if force:
        _refresh_versions(blocking=True)
    elif stale:
        _refresh_versions(blocking=False)  # 后台，主请求不等
    with _versions_lock:
        return dict(_VERSIONS["data"] or {})


def _version_key(value: str) -> list[int]:
    parts: list[int] = []
    for seg in re.split(r"[.\-+_]", value):
        match = re.match(r"\d+", seg)
        parts.append(int(match.group()) if match else 0)
    return parts


def _is_newer(candidate: str, current: str) -> bool:
    """candidate 是否比 current 新（按数字段比较，对非数值段容错）。"""
    left, right = _version_key(candidate), _version_key(current)
    for i in range(max(len(left), len(right))):
        a = left[i] if i < len(left) else 0
        b = right[i] if i < len(right) else 0
        if a != b:
            return a > b
    return False


def _agent_refs() -> dict[str, list[str]]:
    refs: dict[str, list[str]] = {}
    for spec in session_for("__engines__").specs.values():
        refs.setdefault(spec.body, []).append(spec.id)
    return refs


def _compute_view(force: bool = False) -> list[dict[str, Any]]:
    scan = _scan(force=force)
    versions = _versions(force=force)
    bodies = {e["id"]: e for e in list_executors()}
    refs = _agent_refs()

    out: list[dict[str, Any]] = []
    for report in scan:
        tool = str(report.get("tool") or "")
        installs = report.get("installs") or []
        default = installs[0] if installs else {}
        body = bodies.get(tool)
        meta = versions.get(tool, {})
        installed = bool(installs)
        version = default.get("version") or meta.get("version")
        latest = meta.get("latest_version")
        broken = bool(meta.get("installed_but_broken")) or (installed and not default.get("runnable"))
        conflict = bool(report.get("is_conflict"))
        # 业务判断在后端（AGENTS/ui.md）：前端拿到的 status 可直接渲染。
        status = (
            "not_installed" if not installed else "broken" if broken else "conflict" if conflict else "ready"
        )
        has_update = bool(latest and version and _is_newer(str(latest), str(version)))
        out.append(
            {
                "id": tool,
                "name": body["label"] if body else tool,
                "kind": (body or {}).get("kind", "cli"),
                "is_body": body is not None,
                "agent_ids": refs.get(tool, []),
                "status": status,
                "installed": installed,
                "version": version,
                "latest_version": latest,
                "has_update": has_update,
                "broken": broken,
                "conflict": conflict,
                "error": meta.get("error"),
                "path": default.get("path"),
                "source": default.get("source"),
                "runnable": bool(default.get("runnable")),
                "installs": installs,
                "update_command": report.get("command") or "",
                "anchored": bool(report.get("anchored")),
                "unmanaged": bool(report.get("unmanaged")),
            }
        )
    return out


@router.get("/api/crewchat/engines")
async def list_engines(refresh: bool = False) -> list[dict[str, Any]]:
    return _compute_view(force=refresh)


@router.post("/api/crewchat/engines/install")
async def install_engines(request: Request) -> Any:
    return _lifecycle(await request.json(), "install")


@router.post("/api/crewchat/engines/update")
async def update_engines(request: Request) -> Any:
    return _lifecycle(await request.json(), "update")


def _lifecycle(body: dict[str, Any], action: str) -> Any:
    tools = [str(t) for t in (body.get("tools") or []) if t]
    if not tools:
        return JSONResponse({"error": "missing_tools"}, status_code=400)
    try:
        engine_tools = get_engine_tools(get_settings())
        (engine_tools.install if action == "install" else engine_tools.update)(tools)
    except Exception as exc:  # noqa: BLE001
        return JSONResponse(
            {"error": "lifecycle_failed", "message": f"{type(exc).__name__}: {exc}"},
            status_code=502,
        )
    _SCAN["data"] = None  # 装/更新后失效，下次进入重扫
    _VERSIONS["data"] = None
    return {"success": True, "action": action, "tools": tools}
