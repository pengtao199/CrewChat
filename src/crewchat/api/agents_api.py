"""Agent tooling: CLI detection and agent management under Settings → Agent."""

from __future__ import annotations

import shutil
from typing import Any

from fastapi import APIRouter, Request

from ..core.config import get_settings
from ..core.diagnostics import snapshot
from ..orchestration import session_for

router = APIRouter()

KNOWN_TOOLS = {
    "claude": "claude",
    "claude-code": "claude",
    "codex": "codex",
    "opencode": "opencode",
    "gemini": "gemini",
    "qwen": "qwen",
    "goose": "goose",
}


@router.post("/api/shell/check-tool-installed")
async def check_tool_installed(request: Request) -> bool:
    body = await request.json()
    tool = str(body.get("tool") or "")
    binary = KNOWN_TOOLS.get(tool, tool)
    return shutil.which(binary) is not None


@router.get("/api/agents/management")
async def managed_agents() -> list[dict[str, Any]]:
    settings = get_settings()
    settings.ensure_dirs()
    probes = snapshot(settings)
    session = session_for("__agents__")

    by_body: dict[str, list[str]] = {}
    for spec in session.specs.values():
        by_body.setdefault(spec.body, []).append(spec.id)

    out: list[dict[str, Any]] = []
    for body, agent_ids in by_body.items():
        probe = probes.get(body)
        out.append(
            {
                "id": body,
                "type": body,
                "name": probe.label if probe else body,
                "installed": bool(probe.installed) if probe else body == "llm",
                "version": probe.version if probe else None,
                "path": probe.path if probe else None,
                "status": "online" if (probe.ready if probe else True) else "offline",
                "authenticated": probe.logged_in if probe else True,
                "detail": probe.detail if probe else None,
                "agent_ids": agent_ids,
            }
        )
    return out


@router.post("/api/agents/refresh")
async def refresh_agents() -> dict[str, Any]:
    session_for("__agents__").reload()
    return {"success": True}


@router.get("/api/agents/custom")
async def custom_agents() -> list[dict[str, Any]]:
    return []


@router.get("/api/extensions/acp-adapters")
async def acp_adapters() -> list[dict[str, Any]]:
    return []
