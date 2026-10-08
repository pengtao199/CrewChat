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
) -> dict[str, Any]:
    frame: dict[str, Any] = {
        "type": "text",
        "data": {"content": content},
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


def finish_frame(conversation_id: str, msg_id: str, turn_id: str | None = None) -> dict[str, Any]:
    frame: dict[str, Any] = {
        "type": "finish",
        "data": {},
        "msg_id": msg_id,
        "conversation_id": conversation_id,
        "created_at": int(time.time() * 1000),
        "status": "finish",
    }
    if turn_id:
        frame["turn_id"] = turn_id
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


def tool_frame(
    conversation_id: str, msg_id: str, tool: str, status: str, turn_id: str | None = None
) -> dict[str, Any]:
    frame: dict[str, Any] = {
        "type": "tool_call",
        "data": {"tool": tool, "status": status},
        "msg_id": msg_id,
        "conversation_id": conversation_id,
        "created_at": int(time.time() * 1000),
        "status": "work",
    }
    if turn_id:
        frame["turn_id"] = turn_id
    return frame
