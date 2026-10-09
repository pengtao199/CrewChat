"""The chat bridge: a POST starts a turn, the reply streams over the socket.

This is where our crewai orchestration meets the UI's contract. The renderer
expects an immediate `{msg_id, turn_id, runtime}` and then watches
`message.stream` frames until a `finish` frame arrives, so the turn runs in a
background task and every executor event is forwarded as it happens.
"""

from __future__ import annotations

import asyncio
import uuid
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..core.models import AgentSpec
from ..orchestration import session_for
from .conversations import ensure_conversation, persist_turn
from .hub import (
    error_frame,
    finish_frame,
    hub,
    text_frame,
    tool_frame,
    turn_completed_frame,
    user_created_frame,
)

router = APIRouter()

_running: dict[str, asyncio.Task[None]] = {}


def _running_runtime(turn_id: str, speaking: str = "", queued: list[str] | None = None) -> dict[str, Any]:
    return {
        "state": "running",
        "can_send_message": False,
        "has_task": True,
        "is_task": True,
        "is_processing": True,
        "pending_confirmations": 0,
        "turn_id": turn_id,
        "supports_midturn_delivery": False,
        "speaking": speaking,
        "queued": list(queued or []),
    }


def _idle_runtime() -> dict[str, Any]:
    return {
        "state": "idle",
        "can_send_message": True,
        "has_task": False,
        "is_processing": False,
        "pending_confirmations": 0,
        "turn_id": None,
        "supports_midturn_delivery": True,
        "speaking": "",
        "queued": [],
    }


#: 当前发言人 / 排队者。UI 轮询 /runtime 时读它显示「谁在说、还有谁排队」。
_speaker_state: dict[str, dict[str, Any]] = {}


def _broadcast_speaker(
    conversation_id: str, turn_id: str, speaking: str, queued: list[str]
) -> None:
    _speaker_state[conversation_id] = {"turn_id": turn_id, "speaking": speaking, "queued": queued}


def _for_conversation(conversation_id: str):  # type: ignore[no-untyped-def]
    from .store import store

    row = store().get_conversation(conversation_id)
    # 按会话所属项目建 Session：项目决定工作目录(cwd)与 agent 集。
    session = session_for(f"conv:{conversation_id}", (row or {}).get("project_id"))
    agent_ids = (row or {}).get("agent_ids") or []
    specs = [session.specs[a] for a in agent_ids if a in session.specs]
    if not specs:
        specs = list(session.specs.values())[:1]
    return session, specs


def _prompt_for(specs: list[AgentSpec], content: str, group: bool) -> list[str]:
    prompts: list[str] = []
    for spec in specs:
        persona = spec.persona()
        if not persona:
            prompts.append(content)
            continue
        head = persona
        if group:
            others = "、".join(s.display for s in specs if s.id != spec.id) or "（无）"
            head += f"\n\n在场 agent（你之外）：{others}\n给出你的判断与产出，别人错了直接指出，不要复述。"
        prompts.append(f"{head}\n\n---\n\n{content}")
    return prompts


def _sender(spec: AgentSpec, group: bool) -> dict[str, Any]:
    """Identity block for one agent's reply.

    Always sent: every reply is attributable, and a 1:1 conversation should still
    show who is answering. `teammateMessage` stays group-only — it drives the
    teammate colour/border treatment meant for multi-agent threads.
    """
    return {
        "senderName": spec.display,
        "senderAgentType": spec.body,
        "senderAgentId": spec.id,
        "teammateMessage": group,
    }


def _usage_payload(spec: AgentSpec, meta: dict[str, Any]) -> dict[str, Any]:
    """Per-agent usage for the bubble footer and the `usage_events` ledger."""
    usage = meta.get("usage") or {}
    payload = {
        "input_tokens": int(usage.get("input_tokens") or 0),
        "output_tokens": int(usage.get("output_tokens") or 0),
    }
    for key in ("cache_read_tokens", "cache_creation_tokens", "reasoning_tokens"):
        value = int(usage.get(key) or 0)
        if value:
            payload[key] = value
    model = str(meta.get("model") or "")
    if model:
        payload["model"] = model
    if meta.get("cost_usd") is not None:
        payload["cost_usd"] = meta["cost_usd"]
    if meta.get("duration_ms") is not None:
        payload["duration_ms"] = meta["duration_ms"]
    return payload


async def _run_turn(
    conversation_id: str,
    specs: list[AgentSpec],
    content: str,
    turn_id: str,
) -> None:
    """Run one turn: every agent answers as its own message.

    Each agent gets a fresh `msg_id`, so the renderer keeps one bubble per
    speaker instead of one blob of concatenated text. That id is also the
    persisted row id, so reloading history reproduces exactly what was streamed.
    """
    session, _ = _for_conversation(conversation_id)
    group = len(specs) > 1
    prompts = _prompt_for(specs, content, group)

    try:
        for index, (spec, prompt) in enumerate(zip(specs, prompts, strict=False)):
            reply_msg_id = uuid.uuid4().hex
            sender = _sender(spec, group)
            buffer: list[str] = []
            usage: dict[str, Any] = {}

            # Tell the UI who is about to speak and who is still queued.
            _broadcast_speaker(conversation_id, turn_id, spec.display, [s.display for s in specs[index + 1 :]])

            async for event in session.stream_ask(spec, prompt, f"conv:{conversation_id}"):
                if event.kind == "delta" and event.text:
                    buffer.append(event.text)
                    await hub.broadcast(
                        "message.stream",
                        text_frame(
                            conversation_id,
                            reply_msg_id,
                            "".join(buffer),
                            turn_id,
                            sender=sender,
                        ),
                    )
                elif event.kind == "tool" and event.tool:
                    await hub.broadcast(
                        "message.stream",
                        tool_frame(
                            conversation_id,
                            reply_msg_id,
                            event.tool,
                            event.status,
                            turn_id,
                            event.call_id,
                        ),
                    )
                elif event.kind == "error" and event.text:
                    await hub.broadcast(
                        "message.stream", error_frame(conversation_id, reply_msg_id, event.text)
                    )
                elif event.kind == "done" and event.meta:
                    meta = event.meta
                    usage = _usage_payload(spec, meta)
                    if usage:
                        from .store import store

                        store().record_usage(
                            conversation_id=conversation_id,
                            turn_id=turn_id,
                            agent_id=spec.id,
                            body=str(meta.get("body") or spec.body),
                            model=str(meta.get("model") or ""),
                            usage=usage,
                            cost_usd=meta.get("cost_usd"),
                            duration_ms=meta.get("duration_ms"),
                        )

            text = "".join(buffer).strip()
            if text:
                persist_turn(
                    conversation_id,
                    "assistant",
                    text,
                    spec.display,
                    metadata={
                        "agent_id": spec.id,
                        "body": spec.body,
                        "teammate": group,
                        **usage,
                    },
                )
            await hub.broadcast(
                "message.stream",
                finish_frame(
                    conversation_id,
                    reply_msg_id,
                    turn_id,
                    usage,
                    turn_complete=index == len(specs) - 1,
                ),
            )
    except asyncio.CancelledError:
        await hub.broadcast(
            "message.stream",
            error_frame(conversation_id, "", "已取消"),
        )
        raise
    except Exception as exc:  # noqa: BLE001
        await hub.broadcast(
            "message.stream",
            error_frame(conversation_id, "", f"{type(exc).__name__}: {exc}"),
        )
    finally:
        _running.pop(conversation_id, None)
        _speaker_state.pop(conversation_id, None)
        # Authoritative end-of-turn: releases the runtime store back to idle and
        # lets the sidebar refresh. Per-agent finish frames only close bubbles.
        await hub.broadcast(
            "turn.completed", turn_completed_frame(conversation_id, turn_id, _idle_runtime())
        )
        await hub.broadcast(
            "conversation.listChanged",
            {"conversation_id": conversation_id, "reason": "turn_finished"},
        )


@router.post("/api/conversations/{conversation_id}/messages")
async def send_message(conversation_id: str, request: Request) -> Any:
    body = await request.json()
    content = str(body.get("content") or "").strip()
    if not content:
        return JSONResponse({"error": "empty_message"}, status_code=400)

    store_conv, specs = _for_conversation(conversation_id)
    if not specs:
        return JSONResponse({"error": "no_agent_in_conversation"}, status_code=400)

    from .store import store

    if store().get_conversation(conversation_id) is None:
        ensure_conversation(conversation_id, content[:40] or "新会话", [s.id for s in specs])

    user_msg_id = persist_turn(conversation_id, "user", content, "you")
    # Show the user's own message immediately. The renderer inserts the row from
    # this event; without it the bubble only appears after a history refetch,
    # which reads as a failed send.
    await hub.broadcast(
        "message.userCreated",
        user_created_frame(conversation_id, user_msg_id, content),
    )
    turn_id = uuid.uuid4().hex

    task = asyncio.create_task(_run_turn(conversation_id, specs, content, turn_id))
    _running[conversation_id] = task

    return {
        "msg_id": user_msg_id,
        "turn_id": turn_id,
        "runtime": _running_runtime(turn_id, specs[0].display, [s.display for s in specs[1:]]),
    }


@router.post("/api/conversations/{conversation_id}/cancel")
async def cancel_turn(conversation_id: str) -> dict[str, Any]:
    task = _running.pop(conversation_id, None)
    if task and not task.done():
        task.cancel()
    _speaker_state.pop(conversation_id, None)
    return {"runtime": _idle_runtime()}


@router.post("/api/conversations/{conversation_id}/retry")
async def retry_message(conversation_id: str, request: Request) -> Any:
    """Re-ask about the conversation's latest question.

    With `agent_id`, only that agent answers — retrying one reply instead of
    replaying the whole turn. Without it, every agent in the conversation answers.
    The original question is reused verbatim and not re-persisted.
    """
    from .store import store

    body = await request.json() if await request.body() else {}
    agent_id = str(body.get("agent_id") or "") or None

    _, specs = _for_conversation(conversation_id)
    if agent_id:
        specs = [s for s in specs if s.id == agent_id]
    if not specs:
        return JSONResponse({"error": "no_agent_in_conversation"}, status_code=400)

    rows = [
        row
        for row in store().list_messages(conversation_id)
        if row.get("role") == "user" and (row.get("content") or "").strip()
    ]
    if not rows:
        return JSONResponse({"error": "nothing_to_retry"}, status_code=400)

    turn_id = uuid.uuid4().hex
    task = asyncio.create_task(
        _run_turn(conversation_id, specs, str(rows[-1]["content"]).strip(), turn_id)
    )
    _running[conversation_id] = task

    return {
        "turn_id": turn_id,
        "runtime": _running_runtime(turn_id, specs[0].display, [s.display for s in specs[1:]]),
    }


@router.post("/api/conversations/{conversation_id}/active-lease")
async def active_lease(conversation_id: str) -> None:
    return None


@router.get("/api/conversations/{conversation_id}/runtime")
async def runtime(conversation_id: str) -> dict[str, Any]:
    task = _running.get(conversation_id)
    if task and not task.done():
        speaker = _speaker_state.get(conversation_id) or {}
        return {
            "runtime": _running_runtime(
                str(speaker.get("turn_id") or ""),
                str(speaker.get("speaking") or ""),
                list(speaker.get("queued") or []),
            )
        }
    _speaker_state.pop(conversation_id, None)
    return {"runtime": _idle_runtime()}


@router.get("/api/conversations/{conversation_id}/slash-commands")
async def slash_commands(conversation_id: str) -> list[Any]:
    return []


@router.get("/api/conversations/{conversation_id}/usage")
async def usage(conversation_id: str) -> None:
    return None


@router.get("/api/conversations/{conversation_id}/confirmation")
async def confirmations(conversation_id: str) -> list[Any]:
    return []


@router.get("/api/conversations/{conversation_id}/terminals")
async def terminals(conversation_id: str) -> list[Any]:
    return []
