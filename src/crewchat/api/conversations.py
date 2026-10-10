"""Conversations: list/detail/create/delete plus the message history the panel reads.

The forked UI treats our agents as an ACP-family backend, so a conversation
carries `type: "acp"` with `extra.backend` naming the body and `extra.cli_path`
naming the executable — mapped from the agent that owns the conversation.
"""

from __future__ import annotations

import json
import time
import uuid
from typing import Any

from fastapi import APIRouter, Query, Request
from fastapi.responses import JSONResponse

from .store import store

router = APIRouter()


def _ms(value: str | None) -> int:
    if not value:
        return int(time.time() * 1000)
    try:
        return int(float(value) * 1000) if "." in value else int(value) * 1000
    except ValueError:
        return int(time.time() * 1000)


def _iso(ms: int) -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(ms / 1000))


def idle_runtime() -> dict[str, Any]:
    return {
        "state": "idle",
        "can_send_message": True,
        "has_task": False,
        "is_processing": False,
        "pending_confirmations": 0,
        "turn_id": None,
        "supports_midturn_delivery": True,
    }


def to_api(row: dict[str, Any], agents: dict[str, Any] | None = None) -> dict[str, Any]:
    created = _ms(row.get("created_at"))
    modified = _ms(row.get("updated_at"))
    agent_ids = row.get("agent_ids") or []
    first = agent_ids[0] if agent_ids else None
    spec = (agents or {}).get(first) if first else None

    extra: dict[str, Any] = {
        "backend": getattr(spec, "body", None) or "llm",
        "workspace": _project_path(row.get("project_id")),
        "custom_workspace": bool(_project_path(row.get("project_id"))),
        "agent_name": _roster_label(agent_ids, agents) or getattr(spec, "name", None) or first or "",
        "cli_path": None,
        "team_agent_ids": agent_ids,
        "leader_agent_id": row.get("leader_agent_id") or None,
    }

    model: dict[str, Any] = {}
    if spec is not None and getattr(spec, "brain", ""):
        brain: str = spec.brain
        provider_id, _, model_id = brain.partition("/")
        provider_id = provider_id or "default"
        if not model_id:
            # `brain` 只写了 provider id 时，用池子里的默认模型补全
            try:
                from ..core.config import get_settings
                from ..core.providers import get_provider

                pool = get_provider(get_settings(), provider_id)
                model_id = pool.model if pool else provider_id
            except Exception:
                model_id = provider_id
        model = {"provider_id": provider_id, "model": model_id, "use_model": model_id}

    return {
        "id": row["id"],
        "name": row.get("title") or "新会话",
        "created_at": created,
        "modified_at": modified,
        "type": "acp",
        "extra": extra,
        "model": model or None,
        "status": "finished",
        "project_id": row.get("project_id"),
        "runtime": idle_runtime(),
        "source": "aionui",
    }


def _roster_label(agent_ids: list[str], agents: dict[str, Any] | None) -> str:
    if len(agent_ids) < 2:
        return ""
    names: list[str] = []
    for agent_id in agent_ids:
        spec = (agents or {}).get(agent_id)
        names.append(getattr(spec, "name", None) or agent_id)
    shown = names[:3]
    extra = len(names) - len(shown)
    return " · ".join(shown) + (f" +{extra}" if extra else "")


def _project_path(project_id: str | None) -> str:
    if not project_id:
        return ""
    try:
        from ..core.config import get_settings
        from ..core.projects import get_project

        project = get_project(get_settings(), project_id)
        return str(project.path) if project else ""
    except Exception:
        return ""


def _agents_by_id() -> dict[str, Any]:
    from ..orchestration import session_for

    try:
        session = session_for("__list__")
        return dict(session.specs)
    except Exception:
        return {}


@router.get("/api/conversations")
async def list_conversations(
    cursor: str | None = None,
    limit: int = Query(default=10000),
) -> dict[str, Any]:
    rows = store().list_conversations(include_archived=False)
    agents = _agents_by_id()
    items = [to_api(r, agents) for r in rows]
    offset = int(cursor) if cursor and str(cursor).isdigit() else 0
    window = items[offset : offset + limit]
    return {
        "items": window,
        "total": len(items),
        "has_more": offset + limit < len(items),
    }


@router.get("/api/conversations/active-count")
async def active_count() -> dict[str, Any]:
    return {"count": 0}


@router.get("/api/conversations/{conversation_id}")
async def get_conversation(conversation_id: str) -> Any:
    row = store().get_conversation(conversation_id)
    if row is None:
        return JSONResponse({"error": "not_found"}, status_code=404)
    return to_api(row, _agents_by_id())


@router.delete("/api/conversations/{conversation_id}")
async def delete_conversation(conversation_id: str) -> dict[str, Any]:
    store().delete_conversation(conversation_id)
    return {"success": True}


@router.get("/api/conversations/{conversation_id}/messages")
async def list_messages(
    conversation_id: str,
    limit: int = Query(default=50),
    before: str | None = None,
    after: str | None = None,
    anchor_message_id: str | None = None,
    content_mode: str | None = None,
) -> dict[str, Any]:
    """Cursor page — the panel reads `items`, not a bare array (shape follows the UI)."""
    rows = store().list_messages(conversation_id)
    ids = [r["id"] for r in rows]
    if before and before in ids:
        rows = rows[: ids.index(before)]
    current = [r["id"] for r in rows]
    if after and after in current:
        rows = rows[current.index(after) + 1 :]
    window = rows[-limit:] if limit and limit > 0 else rows
    return {
        "items": [_message_payload(m) for m in window],
        "oldest_cursor": window[0]["id"] if window else None,
        "newest_cursor": window[-1]["id"] if window else None,
        "has_more_before": len(rows) > len(window),
        "has_more_after": False,
    }


@router.get("/api/conversations/{conversation_id}/messages/latest")
async def latest_message(conversation_id: str, type: str | None = None) -> Any:
    messages = store().list_messages(conversation_id)
    for message in reversed(messages):
        if type is None or message.get("role") == type:
            return _message_payload(message)
    return None


@router.get("/api/conversations/{conversation_id}/messages/{message_id}")
async def get_message(conversation_id: str, message_id: str) -> Any:
    for message in store().list_messages(conversation_id):
        if message["id"] == message_id:
            return _message_payload(message)
    return JSONResponse({"error": "not_found"}, status_code=404)


def _message_payload(row: dict[str, Any]) -> dict[str, Any]:
    """One history row, shaped exactly like the frame that streamed it live.

    `metadata` carries the agent identity and usage written by the chat layer, so
    a reloaded conversation renders the same per-agent bubbles and footers the
    live stream did instead of collapsing them into anonymous assistant text.
    """
    role = row.get("role")
    metadata = row.get("metadata") or {}
    content: dict[str, Any] = {"content": row.get("content") or ""}
    if role != "user":
        author = row.get("author")
        if author:
            content["senderName"] = author
        if metadata.get("agent_id"):
            content["senderAgentId"] = metadata["agent_id"]
        if metadata.get("body"):
            content["senderAgentType"] = metadata["body"]
        if metadata.get("teammate"):
            content["teammateMessage"] = True
        usage = {
            key: metadata[key]
            for key in (
                "model",
                "input_tokens",
                "output_tokens",
                "cache_read_tokens",
                "cache_creation_tokens",
                "reasoning_tokens",
                "cost_usd",
                "duration_ms",
            )
            if metadata.get(key) is not None
        }
        if usage:
            content["usage"] = usage
    return {
        "id": row["id"],
        "msg_id": row["id"],
        "conversation_id": row["conversation_id"],
        "type": "text",
        "role": role,
        "content": content,
        "created_at": _ms(row.get("created_at")),
        "metadata": metadata,
        "author": row.get("author"),
        "position": "right" if role == "user" else "left",
        "status": "finish",
    }


def _bind_create(body: dict[str, Any]) -> tuple[list[str], str | None, str | None] | JSONResponse:
    """Resolve who belongs in a new conversation.

    A group send uses ``extra.team_agent_ids`` plus the built-in manager.
    A 1:1 send uses ``agent.id``. Empty is allowed so a draft row can exist;
    sending later still requires a bound agent.
    """
    from ..core.agent_presets import MANAGER_ID, load_manager, manager_reason, roster_unready
    from ..core.config import get_settings
    from ..core.models import load_specs
    from ..core.projects import agents_dir_for, get_project

    extra = body.get("extra") or {}
    team_ids = [str(item) for item in (extra.get("team_agent_ids") or []) if str(item).strip()]
    leader_id = str(extra.get("leader_agent_id") or "").strip() or None
    single_id = str((body.get("agent") or {}).get("id") or "").strip() or None
    project_id = body.get("project_id") or extra.get("project_id") or None
    if project_id is not None:
        project_id = str(project_id).strip() or None

    settings = get_settings()
    specs = {spec.id: spec for spec in load_specs(agents_dir_for(settings))}

    if team_ids or leader_id:
        if not project_id:
            return JSONResponse({"error": "missing_project"}, status_code=400)
        if get_project(settings, project_id) is None:
            return JSONResponse({"error": "unknown_project"}, status_code=400)
        if leader_id and leader_id != MANAGER_ID:
            return JSONResponse({"error": "leader_not_manager"}, status_code=400)
        workers: list[str] = []
        seen: set[str] = set()
        for agent_id in team_ids:
            if agent_id == MANAGER_ID or agent_id in seen:
                continue
            seen.add(agent_id)
            workers.append(agent_id)
        if not workers:
            return JSONResponse({"error": "no_workers"}, status_code=400)
        manager = load_manager(settings)
        reason = manager_reason(manager)
        if reason:
            return JSONResponse({"error": f"manager_{reason}"}, status_code=400)
        for agent_id in workers:
            unready = roster_unready(specs.get(agent_id), require_brain=False)
            if unready:
                return JSONResponse({"error": unready, "agent_id": agent_id}, status_code=400)
        return [MANAGER_ID, *workers], MANAGER_ID, project_id

    agent_ids = [single_id] if single_id else []
    if single_id:
        unready = roster_unready(specs.get(single_id), require_brain=False)
        if unready:
            return JSONResponse({"error": unready, "agent_id": single_id}, status_code=400)
    return agent_ids, None, project_id


@router.post("/api/conversations")
async def create_conversation(request: Request) -> Any:
    body = await request.json()
    conv_id = str(body.get("id") or uuid.uuid4())
    name = str(body.get("name") or "新会话")
    bound = _bind_create(body)
    if isinstance(bound, JSONResponse):
        return bound
    agent_ids, leader_agent_id, project_id = bound
    now = _iso(int(time.time() * 1000))
    store().create_conversation(conv_id, name, project_id, list(agent_ids), now, leader_agent_id)
    row = store().get_conversation(conv_id) or {
        "id": conv_id,
        "title": name,
        "agent_ids": agent_ids,
        "leader_agent_id": leader_agent_id,
        "project_id": project_id,
        "created_at": now,
        "updated_at": now,
    }
    return to_api(row, _agents_by_id())


@router.patch("/api/conversations/{conversation_id}")
async def update_conversation(conversation_id: str, request: Request) -> Any:
    body = await request.json()
    row = store().get_conversation(conversation_id)
    if row is None:
        return JSONResponse({"error": "not_found"}, status_code=404)
    if name := body.get("name"):
        with store()._lock, store()._connect() as conn:  # noqa: SLF001
            conn.execute("UPDATE conversations SET title = ? WHERE id = ?", (name, conversation_id))
    if "archived" in body:
        store().set_archived(conversation_id, bool(body["archived"]))
    row = store().get_conversation(conversation_id)
    return to_api(row or {}, _agents_by_id())


@router.get("/api/sidebar/archived")
async def archived() -> dict[str, Any]:
    rows = store().list_conversations(include_archived=True)
    items = [to_api(r, _agents_by_id()) for r in rows if r.get("archived")]
    return {"items": items, "total": len(items)}


@router.post("/api/sidebar/{item_type}/{item_id}/archive")
async def archive_item(item_type: str, item_id: str) -> Any:
    if item_type != "conversation" or store().get_conversation(item_id) is None:
        return JSONResponse({"error": "not_found"}, status_code=404)
    store().set_archived(item_id, True)
    return {"success": True}


@router.post("/api/sidebar/{item_type}/{item_id}/unarchive")
async def unarchive_item(item_type: str, item_id: str) -> Any:
    if item_type != "conversation":
        return JSONResponse({"error": "not_found"}, status_code=404)
    store().set_archived(item_id, False)
    return {"success": True}


@router.delete("/api/sidebar/archived")
async def delete_archived() -> dict[str, int]:
    rows = [r for r in store().list_conversations(include_archived=True) if r.get("archived")]
    for row in rows:
        store().delete_conversation(row["id"])
    return {"teams_deleted": 0, "conversations_deleted": len(rows)}


@router.delete("/api/sidebar/archived/{item_type}/{item_id}")
async def delete_archived_item(item_type: str, item_id: str) -> Any:
    if item_type != "conversation":
        return JSONResponse({"error": "not_found"}, status_code=404)
    store().delete_conversation(item_id)
    return {"success": True}


def persist_turn(
    conv_id: str,
    role: str,
    content: str,
    author: str | None = None,
    metadata: dict[str, Any] | None = None,
    msg_id: str | None = None,
) -> str:
    """Called by the chat layer to record a message."""
    msg_id = msg_id or uuid.uuid4().hex
    now = _iso(int(time.time() * 1000))
    store().add_message(msg_id, conv_id, role, content, author, now, metadata)
    return msg_id


def ensure_conversation(
    conv_id: str,
    name: str,
    agent_ids: list[str],
    project_id: str | None = None,
    leader_agent_id: str | None = None,
) -> None:
    if store().get_conversation(conv_id) is None:
        store().create_conversation(
            conv_id,
            name,
            project_id,
            agent_ids,
            _iso(int(time.time() * 1000)),
            leader_agent_id,
        )
