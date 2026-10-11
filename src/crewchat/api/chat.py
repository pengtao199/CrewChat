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

from fastapi import APIRouter, Body, Request
from fastapi.responses import JSONResponse

from ..core import permissions
from ..core.models import AgentSpec
from ..orchestration import session_for
from ..core.agent_presets import MANAGER_ID
from ..orchestration.group import (
    accept_handoff,
    apply_plan,
    build_handoff_prompt,
    build_prompt,
    lead_steps,
    parse_handoff,
    parse_plan,
    plan_steps,
    resolve_handoff,
    strip_control_calls,
)
from .conversations import ensure_conversation, persist_turn
from .hub import (
    confirmation_remove_frame,
    error_frame,
    finish_frame,
    hub,
    permission_frame,
    text_frame,
    thinking_frame,
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


class ConversationBindError(Exception):
    """The conversation named agents that cannot run this turn."""

    def __init__(self, code: str, **extra: Any) -> None:
        self.code = code
        self.extra = extra
        super().__init__(code)


def _for_conversation(conversation_id: str):  # type: ignore[no-untyped-def]
    from .store import store

    row = store().get_conversation(conversation_id)
    # 按会话所属项目建 Session：项目决定工作目录(cwd)与 agent 集。
    session = session_for(f"conv:{conversation_id}", (row or {}).get("project_id"))
    agent_ids = (row or {}).get("agent_ids") or []
    if not agent_ids:
        raise ConversationBindError("no_agent_in_conversation")
    missing = [agent_id for agent_id in agent_ids if agent_id not in session.specs]
    if missing:
        raise ConversationBindError("unknown_agent", missing=missing)
    return session, [session.specs[agent_id] for agent_id in agent_ids]


def _single_prompt(spec: AgentSpec, content: str) -> str:
    """A 1:1 turn: persona + task, nothing else.

    No peers and no shared context — there is only one speaker, so the pipeline
    machinery of `orchestration.group` would add noise.
    """
    persona = spec.persona()
    return f"{persona}\n\n---\n\n{content}" if persona else content


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

    In a group turn the prompt is built per speaker as the turn progresses,
    carrying what the earlier agents produced — that is the collaboration.
    """
    session, _ = _for_conversation(conversation_id)
    group = len(specs) > 1
    #: (display, output) of the agents that already spoke, fed forward in order.
    prior: list[tuple[str, str]] = []
    outcomes: list[tuple[str, str, str]] = []
    leader = next((spec for spec in specs if spec.id == MANAGER_ID), None)
    # One extra pass per agent is enough for a handoff back; beyond that the
    # queue stops so two agents cannot delegate forever.
    steps = lead_steps(leader, content) if group and leader is not None else plan_steps(specs, content)
    max_steps = max(len(specs) * 2, 1)

    try:
        index = 0
        while index < len(steps):
            current = asyncio.current_task()
            if current is not None and current.cancelling():
                raise asyncio.CancelledError
            step = steps[index]
            spec = step.spec
            if group and step.kind == "handoff":
                prompt = build_handoff_prompt(
                    spec,
                    specs,
                    step.task,
                    prior,
                    step.requester,
                    outcomes=outcomes if spec.id == MANAGER_ID else None,
                )
            elif group:
                prompt = build_prompt(spec, specs, step.task, prior)
            else:
                prompt = _single_prompt(spec, content)
            reply_msg_id = uuid.uuid4().hex
            sender = _sender(spec, group)
            buffer: list[str] = []
            thought: list[str] = []
            usage: dict[str, Any] = {}
            error_text = ""

            # Tell the UI who is about to speak and who is still queued.
            _broadcast_speaker(
                conversation_id,
                turn_id,
                spec.display,
                [pending.spec.display for pending in steps[index + 1 :]],
            )

            async for event in session.stream_ask(spec, prompt, f"conv:{conversation_id}"):
                if event.kind == "thinking" and event.text:
                    thought.append(event.text)
                    await hub.broadcast(
                        "message.stream",
                        thinking_frame(
                            conversation_id,
                            reply_msg_id,
                            "".join(thought),
                            turn_id,
                            sender=sender,
                        ),
                    )
                elif event.kind == "delta" and event.text:
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
                            event.args,
                            event.result,
                        ),
                    )
                elif event.kind == "permission" and event.call_id:
                    await hub.broadcast(
                        "message.stream",
                        permission_frame(
                            conversation_id,
                            reply_msg_id,
                            event.call_id,
                            event.text or event.tool,
                            list(event.meta.get("options") or []),
                            turn_id,
                            action="exec" if event.tool.lower() in ("bash", "shell", "exec") else "tool",
                            command_type=event.tool,
                            sender=sender,
                        ),
                    )
                elif event.kind == "error" and event.text:
                    error_text = event.text
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
            visible = strip_control_calls(text)
            if visible:
                persist_turn(
                    conversation_id,
                    "assistant",
                    visible,
                    spec.display,
                    metadata={
                        "agent_id": spec.id,
                        "body": spec.body,
                        "teammate": group,
                        **usage,
                    },
                    msg_id=reply_msg_id,
                )
                # Feed this agent's output to the next speaker. Done only after the
                # reply is complete (and only when non-empty) so a silent or failed
                # agent does not inject an empty block downstream.
                prior.append((spec.display, visible))
                await hub.broadcast(
                    "message.stream",
                    text_frame(
                        conversation_id,
                        reply_msg_id,
                        visible,
                        turn_id,
                        sender=sender,
                    ),
                )
            if visible:
                outcomes.append((spec.display, "ok", visible))
            elif error_text:
                outcomes.append((spec.display, "error", error_text))
            else:
                outcomes.append((spec.display, "empty", ""))
            if group and text:
                planned = apply_plan(
                    steps,
                    index,
                    parse_plan(text),
                    specs,
                    spec,
                    max_steps=max_steps,
                )
                if planned:
                    for number, (owner, task) in enumerate(planned, 1):
                        label = f"{number}. {owner.display}：{task.description}"
                        await hub.broadcast(
                            "message.stream",
                            tool_frame(
                                conversation_id,
                                reply_msg_id,
                                "plan",
                                "completed",
                                turn_id,
                                f"plan-{reply_msg_id}-{number}",
                                {"to": owner.display, "task": label},
                                task.expected_output or "已排入",
                            ),
                        )
                elif step.kind != "lead":
                    requested = parse_handoff(text)
                    target = (
                        resolve_handoff(requested[0], specs, spec.id)
                        if requested is not None
                        else None
                    )
                    if (
                        requested is not None
                        and target is not None
                        and accept_handoff(
                            steps,
                            index,
                            target,
                            requested[1],
                            spec.display,
                            max_steps=max_steps,
                        )
                    ):
                        await hub.broadcast(
                            "message.stream",
                            tool_frame(
                                conversation_id,
                                reply_msg_id,
                                "handoff",
                                "completed",
                                turn_id,
                                f"handoff-{reply_msg_id}",
                                {"to": target.display, "task": requested[1]},
                                f"交给 {target.display}",
                            ),
                        )
                _broadcast_speaker(
                    conversation_id,
                    turn_id,
                    spec.display,
                    [pending.spec.display for pending in steps[index + 1 :]],
                )
            await hub.broadcast(
                "message.stream",
                finish_frame(
                    conversation_id,
                    reply_msg_id,
                    turn_id,
                    usage,
                    turn_complete=index == len(steps) - 1,
                ),
            )
            index += 1
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

    try:
        store_conv, specs = _for_conversation(conversation_id)
    except ConversationBindError as exc:
        return JSONResponse({"error": exc.code, **exc.extra}, status_code=400)
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


@router.post("/api/conversations/{conversation_id}/confirmations/{call_id}/confirm")
async def confirm(conversation_id: str, call_id: str, body: Any = Body(default=None)) -> dict[str, Any]:
    """用户在权限卡上点了某个选项，唤醒挂起中的身体。

    `body` 的形状跟前端 `ipcBridge.conversation.confirmation.confirm` 一致：
    `{data: {value}, always_allow}`。`value` 是 ACP 的 option kind
    （`allow_once` / `allow_always` / `reject_*`），前端两种分类器都认这些值。

    找不到对应的等待者（已超时、已被另一张卡回答、或这一轮根本不问）不算错误——
    界面照样要把卡片收掉，所以这里一律返回 ok。
    """
    payload = body if isinstance(body, dict) else {}
    data = payload.get("data")
    value = str((data or {}).get("value") or payload.get("value") or "")

    resolved = permissions.resolve_pending(call_id, value)
    await hub.broadcast("message.stream", confirmation_remove_frame(conversation_id, call_id))
    return {"ok": True, "call_id": call_id, "value": value, "resolved": resolved}


@router.get("/api/conversations/{conversation_id}/terminals")
async def terminals(conversation_id: str) -> list[Any]:
    return []
