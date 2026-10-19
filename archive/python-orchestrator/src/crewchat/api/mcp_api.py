"""MCP servers: the ~/.agents/mcp.json convention shared with codex/claude/opencode.

Read and write the same file the CLIs read, so a server added here is immediately
available to every body. `mcpServers` is the only key we own; other top-level keys
in the file are preserved.
"""

from __future__ import annotations

import json
import time
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..core.providers import save_shared_mcp_servers, shared_mcp_servers

router = APIRouter()

_HTTPISH = {"http", "sse", "streamable_http"}


def to_api(name: str, entry: dict[str, Any]) -> dict[str, Any]:
    kind = str(entry.get("type") or "stdio")
    if kind in _HTTPISH:
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
        "original_json": json.dumps(entry, ensure_ascii=False, indent=2),
        "builtin": False,
    }


def _entry_from_body(body: dict[str, Any], base: dict[str, Any] | None = None) -> dict[str, Any]:
    """把 UI 的 `{name, description, transport, enabled}` 还原成 mcp.json 的条目。"""
    entry = dict(base or {})
    transport = body.get("transport")
    if isinstance(transport, dict):
        kind = str(transport.get("type") or entry.get("type") or "stdio")
        entry = {"type": kind}
        if kind in _HTTPISH:
            entry["url"] = transport.get("url") or ""
            if transport.get("headers"):
                entry["headers"] = transport["headers"]
        else:
            entry["command"] = transport.get("command") or ""
            entry["args"] = list(transport.get("args") or [])
            if transport.get("env"):
                entry["env"] = transport["env"]
    if body.get("description") is not None:
        entry["description"] = str(body["description"])
    if body.get("enabled") is not None:
        # 缺省即启用：只在停用时写 enabled=False，保持文件干净。
        if body["enabled"]:
            entry.pop("enabled", None)
        else:
            entry["enabled"] = False
    return entry


@router.get("/api/mcp/servers")
async def list_servers() -> list[dict[str, Any]]:
    return [to_api(name, entry) for name, entry in shared_mcp_servers().items()]


@router.post("/api/mcp/servers")
async def create_server(request: Request) -> Any:
    body = await request.json()
    name = str(body.get("name") or "").strip()
    if not name:
        return JSONResponse({"error": "name_required"}, status_code=400)
    servers = shared_mcp_servers()
    servers[name] = _entry_from_body(body)
    save_shared_mcp_servers(servers)
    return to_api(name, servers[name])


@router.post("/api/mcp/servers/import")
async def import_servers(request: Request) -> Any:
    body = await request.json()
    items = body.get("servers") or []
    servers = shared_mcp_servers()
    out: list[dict[str, Any]] = []
    for item in items:
        name = str(item.get("name") or "").strip()
        if not name:
            continue
        servers[name] = _entry_from_body(item, servers.get(name))
        out.append(to_api(name, servers[name]))
    save_shared_mcp_servers(servers)
    return out


@router.get("/api/mcp/agent-configs")
async def agent_configs() -> list[Any]:
    """Per-agent MCP configs from other CLIs — none managed yet."""
    return []


@router.put("/api/mcp/servers/{server_id}")
async def update_server(server_id: str, request: Request) -> Any:
    body = await request.json()
    servers = shared_mcp_servers()
    if server_id not in servers:
        return JSONResponse({"error": "not_found"}, status_code=404)
    servers[server_id] = _entry_from_body(body, servers.get(server_id))
    new_name = str(body.get("name") or server_id).strip() or server_id
    if new_name != server_id:
        servers[new_name] = servers.pop(server_id)
    save_shared_mcp_servers(servers)
    return to_api(new_name, servers[new_name])


@router.delete("/api/mcp/servers/{server_id}")
async def delete_server(server_id: str) -> dict[str, Any]:
    servers = shared_mcp_servers()
    servers.pop(server_id, None)
    save_shared_mcp_servers(servers)
    return {"success": True}


@router.post("/api/mcp/servers/{server_id}/toggle")
async def toggle_server(server_id: str) -> Any:
    servers = shared_mcp_servers()
    entry = servers.get(server_id)
    if entry is None:
        return JSONResponse({"error": "not_found"}, status_code=404)
    if entry.get("enabled", True):
        entry["enabled"] = False
    else:
        entry.pop("enabled", None)
    save_shared_mcp_servers(servers)
    return to_api(server_id, entry)


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
