"""权限闸门：一个全局开关「自动批准所有权限请求」。

ACP 的 `session/request_permission` 是一轮运行里的一次性询问。这里只回答一件事：
**这次要不要弹卡问用户**。

- 开关打开（默认）：所有询问直接放行，不弹卡。
- 开关关闭：每个询问都挂起，等用户在权限卡上点选。

真相源是 `.data/permissions.jsonc`（本项目私有，不入库）。
"""

from __future__ import annotations

import asyncio
import json
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal

from .cli_overrides import current_settings
from .config import Settings

#: 全局开关键。缺省为 True：保持 fork 自 AionUi 时「不打扰」的行为。
AUTO_APPROVE_KEY = "auto_approve_all"

_LOCK = threading.Lock()

#: 正在等用户点确认的请求：`call_id → asyncio.Future[选中的 option_id]`。
#:
#: 身体（`acp_body._AcpClient._ask`）在这里挂起等答案，HTTP 确认端点
#: （`api/chat.py` 的 `/confirmations/{call_id}/confirm`）来唤醒。跨层传递走这个表，
#: 身体的契约只认 `emit`。
_PENDING: dict[str, asyncio.Future[str]] = {}


def register_pending(call_id: str, future: asyncio.Future[str]) -> None:
    _PENDING[call_id] = future


def resolve_pending(call_id: str, option_id: str) -> bool:
    """唤醒一个待确认的请求。返回是否找到了对应的等待者。"""
    future = _PENDING.pop(call_id, None)
    if future is None or future.done():
        return False
    future.set_result(option_id)
    return True


#: 用户在权限卡上点的选项 → ACP 自己的 option kind（前端两种分类器都认这些值）。
ALLOW_ONCE = "allow_once"
ALLOW_ALWAYS = "allow_always"
REJECT_ONCE = "reject_once"
REJECT_ALWAYS = "reject_always"

_REJECT_KINDS = frozenset({REJECT_ONCE, REJECT_ALWAYS, "cancel", "deny"})


def is_reject(option_id: str) -> bool:
    return option_id in _REJECT_KINDS


@dataclass(frozen=True, slots=True)
class ToolRequest:
    """一次待决的权限请求，只用于弹卡展示。"""

    #: ACP 的工具调用 id，同时当确认端点的 `call_id`。
    call_id: str
    #: 工具名（如 `Bash`、`Edit`）。
    tool: str
    #: 人类可读的一行描述，给权限卡当副标题。
    detail: str = ""
    #: 该工具的入参（原始 dict）。
    args: dict[str, Any] | None = None


def _path() -> Path:
    return current_settings().data_dir / "permissions.jsonc"


def _read(settings: Settings) -> dict[str, Any]:
    path = _path()
    if not path.exists():
        return {}
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError:
        return {}
    try:
        # 容忍注释：优先走 JSONC 加载器，失败再退回标准 JSON。
        from crewai.project.json_loader import load_jsonc_file

        data = load_jsonc_file(str(path))
    except Exception:  # noqa: BLE001 — 开关文件坏了不该拦住对话
        try:
            data = json.loads(raw)
        except ValueError:
            return {}
    return data if isinstance(data, dict) else {}


def _write(settings: Settings, data: dict[str, Any]) -> None:
    path = _path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def auto_approve() -> bool:
    """「自动批准所有权限请求」开关。缺省（文件里没有这个键）为打开。"""
    return bool(_read(current_settings()).get(AUTO_APPROVE_KEY, True))


def set_auto_approve(value: bool) -> None:
    with _LOCK:
        settings = current_settings()
        data = _read(settings)
        data[AUTO_APPROVE_KEY] = bool(value)
        _write(settings, data)


def decision(request: ToolRequest) -> Literal["allow", "ask"]:
    """这一跳要不要问用户。唯一判定点：开关打开就放行，否则弹卡。"""
    del request  # 目前不按工具内容区分；保留参数便于以后按需细化。
    return "allow" if auto_approve() else "ask"
