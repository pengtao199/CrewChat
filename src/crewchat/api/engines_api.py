"""Agent tooling: CLI detection and agent management under Settings → Agent."""

from __future__ import annotations

import shutil
from typing import Any

from fastapi import APIRouter

from ..core.config import get_settings
from ..core.diagnostics import snapshot
from ..executors import list_executors
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
def check_tool_installed(body: dict[str, Any]) -> bool:
    tool = str(body.get("tool") or "")
    binary = KNOWN_TOOLS.get(tool, tool)
    return shutil.which(binary) is not None


@router.get("/api/engines/management")
def managed_agents() -> list[dict[str, Any]]:
    settings = get_settings()
    settings.ensure_dirs()
    probes = snapshot(settings)
    session = session_for("__agents__")

    by_body: dict[str, list[str]] = {}
    for spec in session.specs.values():
        by_body.setdefault(spec.body, []).append(spec.id)

    kinds = {e["id"]: e.get("kind", "cli") for e in list_executors()}

    # 以注册表为准：引擎页列的是"这台机器有哪些执行能力"，
    # 与是否已有 Agent 使用它无关（codex 装好了就该出现）。
    out: list[dict[str, Any]] = []
    for engine in list_executors():
        body = engine["id"]
        agent_ids = by_body.get(body, [])
        probe = probes.get(body)
        out.append(
            {
                "id": body,
                "type": body,
                "kind": kinds.get(body, "cli"),
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


@router.post("/api/engines/refresh")
def refresh_agents() -> dict[str, Any]:
    session_for("__agents__").reload()
    return {"success": True}


@router.get("/api/engines/custom")
def custom_agents() -> list[dict[str, Any]]:
    return []


@router.get("/api/extensions/acp-adapters")
def acp_adapters() -> list[dict[str, Any]]:
    return []
