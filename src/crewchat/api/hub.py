"""WebSocket hub: the single place that pushes frames to connected UI clients.

Frames use the fork's envelope `{"name": <event>, "data": <payload>}`, and the
event names come from the UI's bridge (`message.stream`, `turn.completed`, ...)
so we never invent a channel the renderer isn't listening on.
"""

from __future__ import annotations

import asyncio
import json
import time
from typing import Any

from fastapi import WebSocket


class Hub:
    def __init__(self) -> None:
        self._clients: set[WebSocket] = set()
        self._lock = asyncio.Lock()

    async def add(self, ws: WebSocket) -> None:
        async with self._lock:
            self._clients.add(ws)

    async def remove(self, ws: WebSocket) -> None:
        async with self._lock:
            self._clients.discard(ws)

    @property
    def size(self) -> int:
        return len(self._clients)

    async def broadcast(self, name: str, data: Any) -> None:
        if not self._clients:
            return
        frame = json.dumps({"name": name, "data": data}, ensure_ascii=False)
        async with self._lock:
            targets = list(self._clients)
        dead: list[WebSocket] = []
        for ws in targets:
            try:
                await ws.send_text(frame)
            except Exception:  # noqa: BLE001
                dead.append(ws)
        if dead:
            async with self._lock:
                for ws in dead:
                    self._clients.discard(ws)


hub = Hub()


def text_frame(
    conversation_id: str,
    msg_id: str,
    content: str,
    turn_id: str | None = None,
    *,
    replace: bool = True,
    status: str | None = "work",
    sender: dict[str, Any] | None = None,
) -> dict[str, Any]:
    data: dict[str, Any] = {"content": content}
    if sender:
        # Identity travels with the text so the renderer can draw one bubble per
        # agent instead of a single concatenated blob.
        data.update(sender)
    frame: dict[str, Any] = {
        "type": "text",
        "data": data,
        "msg_id": msg_id,
        "conversation_id": conversation_id,
        "created_at": int(time.time() * 1000),
        "status": status,
    }
    if turn_id:
        frame["turn_id"] = turn_id
    if replace:
        frame["replace"] = True
    return frame


def user_created_frame(
    conversation_id: str,
    msg_id: str,
    content: str,
    *,
    status: str = "finish",
    position: str = "right",
    hidden: bool = False,
) -> dict[str, Any]:
    """A message the user just sent.

    Broadcast immediately after persisting so the bubble appears the moment the
    request is accepted. Without it the renderer has no row to show until the
    history is refetched, which reads as "my message did not send".
    """
    return {
        "conversation_id": conversation_id,
        "msg_id": msg_id,
        "content": content,
        "position": position,
        "status": status,
        "hidden": hidden,
        "created_at": int(time.time() * 1000),
    }


def turn_completed_frame(
    conversation_id: str,
    turn_id: str,
    runtime: dict[str, Any],
    *,
    last_message: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """The turn is over.

    The runtime store treats this as the authoritative "back to idle" signal; the
    per-agent `finish` stream frames only close individual bubbles, so without
    this the conversation stays stuck in its running state after the last reply.
    """
    return {
        "session_id": conversation_id,
        "turn_id": turn_id,
        "status": "finished",
        "state": "ai_waiting_input",
        "detail": "",
        "can_send_message": True,
        "runtime": runtime,
        "last_message": last_message or {"content": None, "created_at": int(time.time() * 1000)},
    }


def finish_frame(
    conversation_id: str,
    msg_id: str,
    turn_id: str | None = None,
    usage: dict[str, Any] | None = None,
    *,
    turn_complete: bool = True,
) -> dict[str, Any]:
    """One agent's reply is complete.

    `usage` carries the per-agent numbers (model/tokens/cost/duration) so the
    renderer can stamp a footer on that bubble; the renderer also reads tokens
    off this frame, so the shape stays a flat token mapping.

    `turn_complete` is false for every agent but the last: in a group turn each
    agent finishes separately, and only the final one ends the turn (the renderer
    must not drop its "running" state when agent 1 of 3 is done).
    """
    frame: dict[str, Any] = {
        "type": "finish",
        "data": dict(usage or {}),
        "msg_id": msg_id,
        "conversation_id": conversation_id,
        "created_at": int(time.time() * 1000),
        "status": "finish",
    }
    if turn_id:
        frame["turn_id"] = turn_id
    if not turn_complete:
        frame["turn_complete"] = False
    return frame


def error_frame(conversation_id: str, msg_id: str, message: str) -> dict[str, Any]:
    return {
        "type": "error",
        "data": {"message": message},
        "msg_id": msg_id,
        "conversation_id": conversation_id,
        "created_at": int(time.time() * 1000),
        "status": "error",
    }


_TOOL_STATUS = {
    "running": "running",
    "in_progress": "running",
    "pending": "running",
    "started": "running",
    "completed": "completed",
    "success": "completed",
    "done": "completed",
    "failed": "error",
    "error": "error",
}


# The renderer's ToolItem shows `${name} · ${description}`. Pick the one argument
# that describes what the call is doing, so "Bash" reads as "Bash · echo hello".
def _describe_tool_call(name: str, args: dict[str, Any]) -> str:
    if not args:
        return ""
    if name == "handoff":
        target = args.get("to")
        task = args.get("task")
        if isinstance(target, str) and isinstance(task, str) and target and task:
            return f"{target}：{task}"[:80]
    for key in ("command", "task", "file_path", "path", "pattern", "query", "url", "file_name"):
        value = args.get(key)
        if isinstance(value, str) and value:
            return value[:80]
    return ""


def tool_frame(
    conversation_id: str,
    msg_id: str,
    name: str,
    status: str,
    turn_id: str | None = None,
    call_id: str = "",
    args: dict[str, Any] | None = None,
    result: str | None = None,
) -> dict[str, Any]:
    """One tool invocation.

    The renderer keys tool cards by `call_id` and drops frames without one, so a
    missing vendor id is filled in here; later frames with the same id update the
    same card.

    `args` and `result` are the card's substance. They started empty — the card
    showed just a name ("Bash", "Read") with nothing behind it. The input lets the
    label read meaningfully ("Bash · echo hello"); the output fills the expandable
    detail panel.
    """
    data: dict[str, Any] = {
        "call_id": call_id or f"{name}-{msg_id}",
        "name": name,
        "status": _TOOL_STATUS.get(status, "running"),
    }
    description = _describe_tool_call(name, args or {}) if args else ""
    # Only send args/description when they carry something. A terminal frame with
    # no input would otherwise overwrite the running frame's real values in the
    # renderer's shallow merge, leaving a finished card strangely blank.
    if args:
        data["args"] = dict(args)
        if description:
            data["description"] = description
    if result is not None:
        data["output"] = result
    frame: dict[str, Any] = {
        "type": "tool_call",
        "data": data,
        "msg_id": msg_id,
        "conversation_id": conversation_id,
        "created_at": int(time.time() * 1000),
        "status": "work",
    }
    if turn_id:
        frame["turn_id"] = turn_id
    return frame
