"""MCP servers: read from the ~/.agents/mcp.json convention."""

from __future__ import annotations

import time
from typing import Any

from fastapi import APIRouter, Request

from ..core.providers import shared_mcp_servers

router = APIRouter()


def to_api(name: str, entry: dict[str, Any]) -> dict[str, Any]:
    kind = str(entry.get("type") or "stdio")
    if kind in {"http", "sse", "streamable_http"}:
        transport: dict[str, Any] = {
            "type": kind,
            "url": entry.get("url") or "",
            "headers": entry.get("headers") or {},
        }
    else:
        transport = {
            "type": "stdio",
            "command": entry.get("command") or "",
            "args": entry.get("args") or [],
            "env": entry.get("env") or {},
        }
    now = int(time.time() * 1000)
    return {
        "id": name,
        "name": name,
        "description": entry.get("description") or "",
        "enabled": entry.get("enabled", True),
        "transport": transport,
        "tools": [],
        "last_test_status": "unchecked",
        "created_at": now,
        "updated_at": now,
        "original_json": __import__("json").dumps(entry, ensure_ascii=False, indent=2),
        "builtin": False,
    }


@router.get("/api/mcp/servers")
async def list_servers() -> list[dict[str, Any]]:
    return [to_api(name, entry) for name, entry in shared_mcp_servers().items()]


@router.post("/api/mcp/test-connection")
async def test_connection(request: Request) -> dict[str, Any]:
    body = await request.json()
    transport = body.get("transport") or {}
    if transport.get("type") == "stdio":
        import shutil

        command = str(transport.get("command") or "")
        ok = bool(shutil.which(command))
        return {"success": ok, "status": "connected" if ok else "error",
                "message": None if ok else f"未找到命令：{command}"}
    return {"success": True, "status": "connected"}
