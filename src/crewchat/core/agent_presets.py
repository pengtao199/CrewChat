"""Built-in Agent files that the app can restore.

These are ordinary AgentSpec documents in `.data/agents/`. The manager is a
visible member of every multi-agent conversation, not a hidden coordinator
and not a fourth Agent type.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .config import Settings
from .models import AgentSpec, load_specs
from .projects import agents_dir_for

MANAGER_ID = "crewchat-manager"

_MANAGER_TEMPLATE: dict[str, Any] = {
    "name": "管理员",
    "emoji": "🧭",
    "body": "llm",
    "brain": "",
    "role": "你是这个会话的管理员。你负责拆解用户任务、把子任务交给在场成员、最后汇总。",
    "goal": "让在场的专业成员各自完成自己的部分，再给出一份对得上的结论。",
    "backstory": (
        "你不代替成员写代码或改文件。你只规划、委派和汇总。"
        "拆任务时按依赖顺序每个子任务一行写 "
        'plan(owner="成员名字", task="子任务", done="完成标准", after="上一步负责人")。'
        "没有依赖就省略 after。成员做完后，根据他们的产出写汇总。"
    ),
    "enabled": True,
}


def manager_path(settings: Settings) -> Path:
    return agents_dir_for(settings) / f"{MANAGER_ID}.jsonc"


def load_manager(settings: Settings) -> AgentSpec | None:
    for spec in load_specs(agents_dir_for(settings)):
        if spec.id == MANAGER_ID:
            return spec
    return None


def manager_reason(spec: AgentSpec | None) -> str:
    """Why the manager cannot lead a group, or empty when it can."""
    if spec is None:
        return "missing"
    if not spec.enabled:
        return "disabled"
    if not (spec.brain or "").strip():
        return "unconfigured"
    return ""


def manager_status(settings: Settings) -> dict[str, Any]:
    spec = load_manager(settings)
    reason = manager_reason(spec)
    return {
        "id": MANAGER_ID,
        "exists": spec is not None,
        "enabled": bool(spec and spec.enabled),
        "has_brain": bool(spec and (spec.brain or "").strip()),
        "ready": reason == "",
        "reason": reason,
        "path": str(manager_path(settings)),
    }


def restore_manager(settings: Settings) -> dict[str, Any]:
    """Write the template only when the file is absent. Never overwrite edits."""
    settings.ensure_dirs()
    path = manager_path(settings)
    created = False
    if not path.exists() and not path.with_suffix(".json").exists():
        path.write_text(
            json.dumps(_MANAGER_TEMPLATE, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )
        created = True
    status = manager_status(settings)
    status["created"] = created
    return status


def roster_unready(spec: AgentSpec | None, *, require_brain: bool) -> str:
    """Stable error for an agent that cannot join a conversation."""
    if spec is None:
        return "unknown_agent"
    if not spec.enabled:
        return "disabled_agent"
    if require_brain and not (spec.brain or "").strip():
        return "unconfigured_agent"
    return ""
