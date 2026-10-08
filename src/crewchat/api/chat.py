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
from .hub import error_frame, finish_frame, hub, text_frame, tool_frame

router = APIRouter()

_running: dict[str, asyncio.Task[None]] = {}


def _running_runtime(turn_id: str) -> dict[str, Any]:
    return {
        "state": "running",
        "can_send_message": False,
        "has_task": True,
        "is_task": True,
        "is_processing": True,
        "pending_confirmations": 0,
        "turn_id": turn_id,
        "supports_midturn_delivery": False,
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
    }


def _for_conversation(conversation_id: str):  # type: ignore[no-untyped-def]
    from .store import store

    row = store().get_conversation(conversation_id)
    session = session_for(f"conv:{conversation_id}")
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


async def _run_turn(
    conversation_id: str,
    specs: list[AgentSpec],
    content: str,
    turn_id: str,
    reply_msg_id: str,
) -> None:
    session, _ = _for_conversation(conversation_id)
    group = len(specs) > 1
    prompts = _prompt_for(specs, content, group)
    buffer: list[str] = []

    try:
        for spec, prompt in zip(specs, prompts, strict=False):
            head = f"**{spec.display}**\n\n" if group else ""
            if head:
                buffer.append(head)
                await hub.broadcast(
                    "message.stream",
                    text_frame(conversation_id, reply_msg_id, "".join(buffer), turn_id),
                )

            async for event in session.stream_ask(spec, prompt, f"conv:{conversation_id}"):
                if event.kind == "delta" and event.text:
                    buffer.append(event.text)
                    await hub.broadcast(
                        "message.stream",
                        text_frame(conversation_id, reply_msg_id, "".join(buffer), turn_id),
                    )
                elif event.kind == "tool" and event.tool:
                    await hub.broadcast(
                        "message.stream",
                        tool_frame(conversation_id, reply_msg_id, event.tool, event.status, turn_id),
                    )
                elif event.kind == "error" and event.text:
                    await hub.broadcast(
                        "message.stream", error_frame(conversation_id, reply_msg_id, event.text)
                    )

            if group:
                buffer.append("\n\n")

        text = "".join(buffer).strip()
        if text:
            persist_turn(conversation_id, "assistant", text, specs[0].display)
    except asyncio.CancelledError:
        await hub.broadcast(
            "message.stream",
            error_frame(conversation_id, reply_msg_id, "已取消"),
        )
        raise
    except Exception as exc:  # noqa: BLE001
        await hub.broadcast(
            "message.stream",
            error_frame(conversation_id, reply_msg_id, f"{type(exc).__name__}: {exc}"),
        )
    finally:
        _running.pop(conversation_id, None)
        await hub.broadcast(
            "message.stream", finish_frame(conversation_id, reply_msg_id, turn_id)
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
    turn_id = uuid.uuid4().hex
    reply_msg_id = uuid.uuid4().hex

    task = asyncio.create_task(_run_turn(conversation_id, specs, content, turn_id, reply_msg_id))
    _running[conversation_id] = task

    return {
        "msg_id": user_msg_id,
        "turn_id": turn_id,
        "runtime": _running_runtime(turn_id),
    }


@router.post("/api/conversations/{conversation_id}/cancel")
async def cancel_turn(conversation_id: str) -> dict[str, Any]:
    task = _running.pop(conversation_id, None)
    if task and not task.done():
        task.cancel()
    return {"runtime": _idle_runtime()}


@router.post("/api/conversations/{conversation_id}/active-lease")
async def active_lease(conversation_id: str) -> None:
    return None


@router.get("/api/conversations/{conversation_id}/runtime")
async def runtime(conversation_id: str) -> dict[str, Any]:
    task = _running.get(conversation_id)
    if task and not task.done():
        return {"runtime": _running_runtime("")}
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
