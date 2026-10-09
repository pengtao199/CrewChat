"""Agent composer: read and write `agents/*.jsonc`, the declarative source of truth.

This is our own endpoint rather than /api/assistants on purpose: the file is the
truth and the UI is an editor for it, so the payload is the raw AgentSpec
(brain/body/skills/mcps/memory/rules) instead of the assistant projection the
rest of the fork consumes.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
from pydantic import ValidationError

from ..core.config import get_settings
from ..core.models import AgentSpec, load_specs
from ..orchestration import session_for
from ..core.projects import agents_dir_for

router = APIRouter()

_SAFE_ID = re.compile(r"^[a-zA-Z0-9_-]{1,64}$")


def _agents_dir(project_id: str | None = None) -> Path:
    """全局 agent 目录；`project_id` 保留兼容但不影响结果（agent 不归工作区）。"""
    settings = get_settings()
    settings.ensure_dirs()
    return agents_dir_for(settings)


def _spec_payload(spec: AgentSpec) -> dict[str, Any]:
    return {
        "id": spec.id,
        "name": spec.name or spec.id,
        "emoji": spec.emoji,
        "brain": spec.brain,
        "body": spec.body,
        "role": spec.role,
        "goal": spec.goal,
        "backstory": spec.backstory,
        "skills": list(spec.skills),
        "mcps": list(spec.mcps),
        "plugins": list(spec.plugins),
        "memory": spec.memory,
        "enabled": spec.enabled,
        "max_iter": spec.max_iter,
        "reasoning": spec.reasoning,
        "cwd": spec.cwd,
    }


def _write_agent(path: Path, payload: dict[str, Any]) -> None:
    body = {
        "name": payload.get("name") or path.stem,
        "emoji": payload.get("emoji") or "🤖",
        "brain": payload.get("brain") or "",
        "body": payload.get("body") or "llm",
        "role": payload.get("role") or "",
        "goal": payload.get("goal") or "",
        "backstory": payload.get("backstory") or "",
        "skills": list(payload.get("skills") or []),
        "mcps": list(payload.get("mcps") or []),
        "memory": bool(payload.get("memory")),
        "max_iter": int(payload.get("max_iter") or 24),
        "reasoning": bool(payload.get("reasoning")),
    }
    if payload.get("plugins"):
        body["plugins"] = list(payload["plugins"])
    if payload.get("cwd"):
        body["cwd"] = payload["cwd"]
    path.write_text(json.dumps(body, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


@router.get("/api/crewchat/agents")
async def list_agents(project_id: str | None = None) -> dict[str, Any]:
    directory = _agents_dir(project_id)
    specs = load_specs(directory)
    return {
        "agents": [_spec_payload(s) for s in specs],
        "agents_dir": str(directory),
    }


@router.get("/api/crewchat/agents/options")
async def agent_options() -> dict[str, Any]:
    """Everything the composer's pickers need, in one round trip."""
    from ..executors import list_executors
    from ..core.providers import available_plugins, available_skills, load_providers, shared_mcp_servers

    settings = get_settings()
    providers = load_providers(settings)
    return {
        # 只回调用方真正要用的字段；模型列表走 /api/providers/fetch-models（按端点实时拉取）。
        "providers": [
            {
                "id": p.id,
                "name": p.name,
                "model": p.model,
                "base_url": p.base_url,
                "has_key": p.has_key(),
            }
            for p in providers
        ],
        "bodies": list_executors(),
        "skills": [s["name"] for s in available_skills()],
        "mcps": sorted(shared_mcp_servers().keys()),
        "plugins": available_plugins(),
    }


@router.get("/api/crewchat/agents/{agent_id}")
async def get_agent(agent_id: str, project_id: str | None = None) -> Any:
    for spec in load_specs(_agents_dir(project_id)):
        if spec.id == agent_id:
            return _spec_payload(spec)
    return JSONResponse({"error": "not_found"}, status_code=404)


@router.put("/api/crewchat/agents/{agent_id}")
async def upsert_agent(agent_id: str, request: Request, project_id: str | None = None) -> Any:
    if not _SAFE_ID.match(agent_id):
        return JSONResponse({"error": "invalid_id"}, status_code=400)
    payload = await request.json()
    directory = _agents_dir(project_id)
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / f"{agent_id}.jsonc"
    try:
        _write_agent(path, payload)
    except OSError as exc:
        return JSONResponse({"error": f"write_failed: {exc}"}, status_code=500)

    _reload_all()
    for spec in load_specs(directory):
        if spec.id == agent_id:
            return _spec_payload(spec)
    return JSONResponse({"error": "written_but_unreadable"}, status_code=500)


@router.delete("/api/crewchat/agents/{agent_id}")
async def delete_agent(agent_id: str, project_id: str | None = None) -> dict[str, Any]:
    if not _SAFE_ID.match(agent_id):
        return JSONResponse({"error": "invalid_id"}, status_code=400)
    directory = _agents_dir(project_id)
    (directory / f"{agent_id}.jsonc").unlink(missing_ok=True)
    (directory / f"{agent_id}.json").unlink(missing_ok=True)
    _reload_all()
    return {"success": True}


@router.post("/api/crewchat/agents/{agent_id}/preview")
async def preview_prompt(agent_id: str, request: Request) -> dict[str, Any]:
    """Resolve the composed agent into the exact prompt the body will receive."""
    body = await request.json()
    for spec in load_specs(_agents_dir(None)):
        if spec.id == agent_id:
            return {
                "prompt": spec.persona(),
                "body": spec.body,
                "brain": spec.brain,
                "skills": spec.skills,
                "mcps": spec.mcps,
            }
    try:
        candidate = AgentSpec(id=agent_id, **{k: v for k, v in body.items() if k != "id"})
    except ValidationError as exc:
        return JSONResponse({"error": str(exc)}, status_code=400)
    return {"prompt": candidate.persona(), "body": candidate.body, "brain": candidate.brain}


def _reload_all() -> None:
    """agent 全局：改一处，所有已缓存 Session 一起刷新。"""
    from ..orchestration.session import reload_all

    get_settings().ensure_dirs()
    reload_all()
