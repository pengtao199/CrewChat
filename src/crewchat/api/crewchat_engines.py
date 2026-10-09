"""Engines: 本机执行引擎的管理（自研 `/api/crewchat/engines`）。

数据 = cc-switch 的工具层扫描（安装分布/版本/来源/冲突/更新命令）+ 我们执行器注册表
（kind/名称/被哪些 Agent 引用）。安装/更新**会真正执行系统命令**。见 `AGENTS/gateway.md`。
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..core.config import get_settings
from ..core.engines import get_engine_tools
from ..executors import list_executors
from ..orchestration import session_for

router = APIRouter()


def _agent_refs() -> dict[str, list[str]]:
    refs: dict[str, list[str]] = {}
    for spec in session_for("__engines__").specs.values():
        refs.setdefault(spec.body, []).append(spec.id)
    return refs


def _view() -> list[dict[str, Any]]:
    settings = get_settings()
    try:
        scan = get_engine_tools(settings).scan()
    except Exception:  # noqa: BLE001 — 核心不可用时降级为"只有我们自己的引擎"
        scan = []
    bodies = {e["id"]: e for e in list_executors()}
    refs = _agent_refs()

    out: list[dict[str, Any]] = []
    seen: set[str] = set()
    for report in scan:
        tool = str(report.get("tool") or "")
        seen.add(tool)
        installs = report.get("installs") or []
        default = installs[0] if installs else {}
        body = bodies.get(tool)
        out.append(
            {
                "id": tool,
                "name": body["label"] if body else tool,
                "kind": (body or {}).get("kind", "cli"),
                "is_body": body is not None,
                "agent_ids": refs.get(tool, []),
                "installed": bool(installs),
                "version": default.get("version"),
                "path": default.get("path"),
                "source": default.get("source"),
                "runnable": bool(default.get("runnable")),
                "conflict": bool(report.get("is_conflict")),
                "installs": installs,
                "update_command": report.get("command") or "",
                "anchored": bool(report.get("anchored")),
                "unmanaged": bool(report.get("unmanaged")),
            }
        )
    for eid, body in bodies.items():
        if eid in seen:
            continue
        out.append(
            {
                "id": eid,
                "name": body["label"],
                "kind": body.get("kind", "cli"),
                "is_body": True,
                "agent_ids": refs.get(eid, []),
                "installed": True,
                "version": None,
                "path": None,
                "source": None,
                "runnable": True,
                "conflict": False,
                "installs": [],
                "update_command": "",
                "anchored": False,
                "unmanaged": True,
            }
        )
    return out


@router.get("/api/crewchat/engines")
async def list_engines() -> list[dict[str, Any]]:
    return _view()


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
        fn = get_engine_tools(get_settings()).install if action == "install" else get_engine_tools(get_settings()).update
        fn(tools)
    except Exception as exc:  # noqa: BLE001
        return JSONResponse(
            {"error": "lifecycle_failed", "message": f"{type(exc).__name__}: {exc}"},
            status_code=502,
        )
    return {"success": True, "action": action, "tools": tools}
