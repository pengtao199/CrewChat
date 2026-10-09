"""Agents: our agents presented as the UI's agent cards and detail page.

The Guid page reads `detail.defaults.model.mode` unguarded, so the detail
response must always carry the full AgentDetail shape — a missing `defaults`
crashes the page rather than degrading.
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..core.config import get_settings
from ..orchestration import session_for

router = APIRouter()


def _session():  # type: ignore[no-untyped-def]
    settings = get_settings()
    settings.ensure_dirs()
    return session_for("__agents__")


def _scalar(value: str | None = None, mode: str = "auto") -> dict[str, Any]:
    return {"mode": mode, "value": value} if value else {"mode": mode}


def _list_default(values: list[str]) -> dict[str, Any]:
    return {"mode": "default", "values": list(values)}


def status_for(spec) -> tuple[str, str | None]:  # type: ignore[no-untyped-def]
    from ..executors import get_executor

    try:
        executor = get_executor(spec.body)
    except KeyError as exc:
        return "missing", str(exc)
    if not executor.available():
        return "missing", f"未安装 {executor.label}"
    return "online", None


def to_list_item(spec, index: int) -> dict[str, Any]:  # type: ignore[no-untyped-def]
    status, message = status_for(spec)
    return {
        "id": spec.id,
        "source": "user",
        "name": spec.name or spec.id,
        "name_i18n": {},
        "description": spec.role or spec.goal or "",
        "description_i18n": {},
        "avatar": spec.emoji or "🤖",
        # 真实启用状态：它决定 Guid 页的 agent 芯片是否出现
        "enabled": spec.enabled,
        "sort_order": index,
        # agent_id 指引擎（身体），与 /api/agents/management 的 id 对齐，
        # 否则上游的「绑定助手」栈永远匹配不到（它按 agent.agent_id === managedAgent.id 过滤）
        "agent_id": spec.body,
        "agent": {"type": spec.body, "source": "custom", "acp_backend": spec.body},
        "enabled_skills": list(spec.skills),
        "custom_skill_names": [],
        "disabled_builtin_skills": [],
        "context": spec.persona(),
        "context_i18n": {},
        "prompts": [],
        "prompts_i18n": {},
        "models": [spec.brain] if spec.brain else [],
        "agent_status": status,
        "agent_status_message": message,
        "team_selectable": True,
        "deletable": False,
    }


def to_detail(spec, index: int) -> dict[str, Any]:
    base = to_list_item(spec, index)
    provider_id, _, model_id = (spec.brain or "").partition("/")
    if provider_id and not model_id:
        try:
            from ..core.providers import get_provider

            pool = get_provider(get_settings(), provider_id)
            model_id = pool.model if pool else ""
        except Exception:
            model_id = ""
    model_value = f"{provider_id}/{model_id}" if provider_id and model_id else (spec.brain or None)

    return {
        "id": spec.id,
        "source": "user",
        "agent_status": base["agent_status"],
        "agent_status_message": base["agent_status_message"],
        "team_selectable": True,
        "deletable": False,
        "profile": {
            "name": spec.name or spec.id,
            "name_i18n": {},
            "description": spec.role or spec.goal or "",
            "description_i18n": {},
            "avatar": spec.emoji or "🤖",
        },
        "state": {"enabled": spec.enabled, "sort_order": index},
        "engine": {
            "agent_id": spec.body,
            "agent": {"type": spec.body, "source": "custom", "acp_backend": spec.body},
        },
        "rules": {"content": spec.persona(), "storage_mode": "inline"},
        "prompts": {"recommended": [], "recommended_i18n": {}},
        "defaults": {
            "model": _scalar(model_value),
            "permission": _scalar(None, "auto"),
            "thought_level": _scalar(None, "auto"),
            "skills": _list_default(spec.skills),
            "mcps": _list_default(spec.mcps),
        },
        "capabilities": {
            "default_skill_ids": list(spec.skills),
            "custom_skill_names": [],
            "default_disabled_builtin_skill_ids": [],
        },
        "preferences": {
            "last_model_id": model_value,
            "last_skill_ids": list(spec.skills),
            "last_disabled_builtin_skill_ids": [],
            "last_mcp_ids": list(spec.mcps),
        },
    }


def _find(agent_id: str):  # type: ignore[no-untyped-def]
    specs = list(_session().specs.values())
    for index, spec in enumerate(specs):
        if spec.id == agent_id:
            return spec, index
    return None, -1


@router.get("/api/agents")
async def list_agents() -> list[dict[str, Any]]:
    specs = list(_session().specs.values())
    return [to_list_item(s, i) for i, s in enumerate(specs)]


@router.get("/api/agents/{agent_id}")
async def get_agent(agent_id: str) -> Any:
    spec, index = _find(agent_id)
    if spec is None:
        return JSONResponse({"error": "not_found"}, status_code=404)
    return to_detail(spec, index)


@router.patch("/api/agents/{agent_id}/state")
async def set_state(agent_id: str, request: Request) -> Any:
    """启停一个 Agent。

    只改文件里的 enabled 字段，其余键原样保留（用户可能手写过额外字段）。
    """
    import json

    body = await request.json()
    if "enabled" not in body:
        return JSONResponse({"error": "missing_enabled"}, status_code=400)

    spec, index = _find(agent_id)
    if spec is None:
        return JSONResponse({"error": "not_found"}, status_code=404)

    from ..core.config import get_settings
    from ..core.projects import agents_dir_for

    settings = get_settings()
    settings.ensure_dirs()
    directory = agents_dir_for(settings)
    for candidate in (directory / f"{agent_id}.jsonc", directory / f"{agent_id}.json"):
        if not candidate.exists():
            continue
        try:
            raw = json.loads(candidate.read_text(encoding="utf-8"))
        except ValueError:
            raw = {}
        raw["enabled"] = bool(body["enabled"])
        candidate.write_text(json.dumps(raw, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        break

    session_for("__agents__").reload()
    session_for("__agents__").reload()
    spec, index = _find(agent_id)
    if spec is None:
        return JSONResponse({"error": "reload_failed"}, status_code=500)
    return to_list_item(spec, index)


@router.get("/api/agents/{agent_id}/rules")
async def get_rules(agent_id: str) -> dict[str, Any]:
    spec, _ = _find(agent_id)
    return {"content": spec.persona() if spec else "", "storage_mode": "inline"}


@router.get("/api/agents/{agent_id}/prompts")
async def get_prompts(agent_id: str) -> dict[str, Any]:
    return {"recommended": [], "recommended_i18n": {}}


@router.post("/api/agents/import")
async def import_agents() -> dict[str, Any]:
    return {"success": True, "imported": 0, "skipped": 0}
