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


def tool_frame(
    conversation_id: str,
    msg_id: str,
    name: str,
    status: str,
    turn_id: str | None = None,
    call_id: str = "",
) -> dict[str, Any]:
    """One tool invocation.

    The renderer keys tool cards by `call_id` and drops frames without one, so a
    missing vendor id is filled in here; later frames with the same id update the
    same card.
    """
    frame: dict[str, Any] = {
        "type": "tool_call",
        "data": {
            "call_id": call_id or f"{name}-{msg_id}",
            "name": name,
            "args": {},
            "status": _TOOL_STATUS.get(status, "running"),
        },
        "msg_id": msg_id,
        "conversation_id": conversation_id,
        "created_at": int(time.time() * 1000),
        "status": "work",
    }
    if turn_id:
        frame["turn_id"] = turn_id
    return frame
