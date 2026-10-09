"""HTTP-level regression for endpoints behind visible buttons.

Covers the agent composer options payload (regression: 500 from a pydantic
serialization error), and the MCP / Skills CRUD the Tools and Skills pages call.
Uses an isolated HOME and data dir so the shared `~/.agents` is never touched.
"""

from __future__ import annotations

from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from crewchat.core import config

PREFIX = "/api/v1"


@pytest.fixture()
def client(tmp_path, monkeypatch):  # type: ignore[no-untyped-def]
    home = tmp_path / "home"
    (home / ".agents").mkdir(parents=True)
    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("CREWCHAT_DATA_DIR", str(tmp_path / "data"))
    config.get_settings.cache_clear()

    from crewchat.core import providers
    from crewchat.api import skills_api
    from crewchat.api import store as store_mod

    monkeypatch.setattr(providers, "_HOME_AGENTS", home / ".agents", raising=False)
    monkeypatch.setattr(skills_api, "_SKILLS_DIR", home / ".agents" / "skills", raising=False)
    # The store singleton builds its schema once against whichever data dir was
    # active; reset it so each test initializes against its own tmp dir.
    store_mod._store = None

    from crewchat.api.app import create_app

    yield TestClient(create_app()), home
    config.get_settings.cache_clear()


# ---- agent composer options (was 500) ---------------------------------------


def test_agents_options_serializes(client) -> None:  # type: ignore[no-untyped-def]
    test_client, _ = client
    r = test_client.get(f"{PREFIX}/crewchat/agents/options")
    assert r.status_code == 200, r.text
    body = r.json()
    assert set(body) >= {"providers", "bodies", "skills", "mcps", "plugins"}
    for provider in body["providers"]:
        assert {"id", "name", "model", "base_url", "has_key"} <= set(provider)


# ---- MCP CRUD ----------------------------------------------------------------


def test_mcp_crud_roundtrip(client) -> None:  # type: ignore[no-untyped-def]
    c, home = client
    assert c.get(f"{PREFIX}/mcp/servers").json() == []

    created = c.post(
        f"{PREFIX}/mcp/servers",
        json={"name": "demo", "description": "d", "transport": {"type": "stdio", "command": "echo", "args": ["hi"]}},
    )
    assert created.status_code == 200, created.text
    server = created.json()
    assert server["id"] == "demo"
    assert server["transport"]["command"] == "echo"

    assert len(c.get(f"{PREFIX}/mcp/servers").json()) == 1

    updated = c.put(
        f"{PREFIX}/mcp/servers/demo",
        json={"name": "demo", "description": "d2", "transport": {"type": "http", "url": "https://x"}},
    )
    assert updated.json()["description"] == "d2"
    assert updated.json()["transport"]["type"] == "http"

    toggled = c.post(f"{PREFIX}/mcp/servers/demo/toggle")
    assert toggled.json()["enabled"] is False

    assert c.delete(f"{PREFIX}/mcp/servers/demo").status_code == 200
    assert c.get(f"{PREFIX}/mcp/servers").json() == []
    assert (home / ".agents" / "mcp.json").exists()


def test_mcp_create_requires_name(client) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    assert c.post(f"{PREFIX}/mcp/servers", json={"transport": {"type": "stdio", "command": "x"}}).status_code == 400


# ---- Skills ------------------------------------------------------------------


def test_skills_import_and_delete(client, tmp_path) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    source = tmp_path / "my-skill"
    source.mkdir()
    (source / "SKILL.md").write_text("---\nname: my-skill\ndescription: does things\n---\n\nbody\n", encoding="utf-8")

    limits = c.get(f"{PREFIX}/skills/import-limits").json()
    assert {"max_file_bytes", "max_total_bytes"} <= set(limits)

    imported = c.post(f"{PREFIX}/skills/import", json={"skill_path": str(source)})
    assert imported.status_code == 200
    assert imported.json()["skill_name"] == "my-skill"

    names = {s["name"] for s in c.get(f"{PREFIX}/skills").json()}
    assert "my-skill" in names

    assert c.delete(f"{PREFIX}/skills/my-skill").status_code == 200
    assert "my-skill" not in {s["name"] for s in c.get(f"{PREFIX}/skills").json()}


def test_skills_info_and_detect_paths(client, tmp_path) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    source = tmp_path / "info-skill"
    source.mkdir()
    (source / "SKILL.md").write_text("---\ndescription: hello\n---\n", encoding="utf-8")
    info = c.post(f"{PREFIX}/skills/info", json={"skill_path": str(source)})
    assert info.json() == {"name": "info-skill", "description": "hello"}
    assert c.get(f"{PREFIX}/skills/detect-paths").status_code == 200


# ---- guard: endpoints that back shipped settings pages must be implemented ----


#: Every GET the shipped settings pages (Model / Usage / Agents / Engines /
#: Skills / Tools / System) load. A 501 here means a page renders dead.
SHIPPED_PAGE_ENDPOINTS = [
    "/crewchat/version",
    "/crewchat/agents",
    "/crewchat/agents/options",
    "/crewchat/engines",
    "/crewchat/gateway",
    "/crewchat/usage",
    "/providers",
    "/model-tags",
    "/mcp/servers",
    "/skills",
    "/skills/import-limits",
    "/skills/import-history",
    "/agents/management",
    "/assistants",
    "/settings/client",
    "/settings",
    "/system/info",
    "/system/current-user",
    "/conversations",
]


@pytest.mark.parametrize("path", SHIPPED_PAGE_ENDPOINTS)
def test_shipped_page_endpoint_is_implemented(client, path: str) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    status = c.get(f"{PREFIX}{path}").status_code
    assert status != 501, f"{path} is not implemented (501) but backs a shipped page"
    assert status < 500, f"{path} returned {status}"


# ---- conversation messages: cursor page shape --------------------------------


def test_messages_returns_cursor_page(client) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    assert c.post(f"{PREFIX}/conversations", json={"id": "conv-2", "name": "x"}).status_code == 200
    from crewchat.api.store import store

    store().add_message("m1", "conv-2", "user", "hi", "you", "2026-01-01T00:00:01Z")
    store().add_message("m2", "conv-2", "assistant", "hello", "🤖 A", "2026-01-01T00:00:02Z")

    page = c.get(f"{PREFIX}/conversations/conv-2/messages?limit=50&content_mode=compact").json()
    assert [m["id"] for m in page["items"]] == ["m1", "m2"]
    assert page["oldest_cursor"] == "m1"
    assert page["newest_cursor"] == "m2"
    assert "has_more_before" in page and "has_more_after" in page
    assert page["items"][0]["type"] == "text"
    assert page["items"][0]["content"] == {"content": "hi"}
    assert page["items"][0]["position"] == "right"

    limited = c.get(f"{PREFIX}/conversations/conv-2/messages?limit=1").json()
    assert [m["id"] for m in limited["items"]] == ["m2"]
    assert limited["has_more_before"] is True

    assert c.get(f"{PREFIX}/conversations/conv-2/messages/m1").json()["id"] == "m1"
    assert c.get(f"{PREFIX}/conversations/conv-2/messages/nope").status_code == 404


# ---- filesystem picker (workspace/project selection) -------------------------


def test_fs_dir_and_read(client, tmp_path) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    workspace = tmp_path / "ws"
    (workspace / "sub").mkdir(parents=True)
    (workspace / "a.txt").write_text("hello", encoding="utf-8")
    (workspace / ".hidden").write_text("x", encoding="utf-8")

    entries = c.post(f"{PREFIX}/fs/dir", json={"dir": str(workspace), "root": str(workspace)}).json()
    by_name = {e["name"]: e for e in entries}
    assert by_name["sub"]["is_dir"] is True
    assert by_name["a.txt"]["isFile"] is True
    assert ".hidden" not in by_name  # dotfiles are hidden from the picker

    assert c.post(f"{PREFIX}/fs/read", json={"path": str(workspace / "a.txt")}).json() == "hello"


def test_system_info_exposes_work_dir(client) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    assert c.get(f"{PREFIX}/system/info").json().get("work_dir")


# ---- sidebar archive flow (button on the conversation list) ------------------


def test_gateway_model_routes_cli_body(monkeypatch) -> None:  # type: ignore[no-untyped-def]
    from types import SimpleNamespace

    from crewchat.core.models import AgentSpec
    from crewchat.orchestration import session as session_mod

    calls: list[tuple[str, str, str]] = []

    class _FakeGateway:
        def activate(self, app: str, provider) -> str:  # type: ignore[no-untyped-def]
            calls.append((app, provider.id, provider.model))
            return f"ccs-claude-{provider.id}--{provider.model}"

    monkeypatch.setattr("crewchat.core.gateway.get_gateway", lambda *a, **k: _FakeGateway())
    session_mod._ROUTED.clear()

    session = session_mod.Session.__new__(session_mod.Session)
    session.settings = None  # type: ignore[assignment]
    spec = AgentSpec(id="a", body="claude", brain="p/m")
    provider = SimpleNamespace(id="p", model="m")

    assert session._gateway_model(spec, provider, True) == "ccs-claude-p--m"
    assert session._gateway_model(spec, provider, True) == "ccs-claude-p--m"  # cached
    assert calls == [("claude", "p", "m")]

    # A body that self-configures per run (opencode/llm) is never routed globally.
    assert session._gateway_model(AgentSpec(id="b", body="opencode", brain="p/m"), provider, False) is None
    # no brain → leave the CLI's own login alone.
    assert session._gateway_model(spec, None, True) is None
    assert calls == [("claude", "p", "m")]


def test_sidebar_archive_flow(client) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    assert c.post(f"{PREFIX}/conversations", json={"id": "conv-1", "name": "x"}).status_code == 200

    assert c.post(f"{PREFIX}/sidebar/conversation/conv-1/archive").status_code == 200
    archived = c.get(f"{PREFIX}/sidebar/archived").json()
    assert {i["id"] for i in archived["items"]} == {"conv-1"}

    assert c.post(f"{PREFIX}/sidebar/conversation/conv-1/unarchive").status_code == 200
    assert c.get(f"{PREFIX}/sidebar/archived").json()["items"] == []

    c.post(f"{PREFIX}/sidebar/conversation/conv-1/archive")
    assert c.delete(f"{PREFIX}/sidebar/archived").json() == {"teams_deleted": 0, "conversations_deleted": 1}

    assert c.post(f"{PREFIX}/sidebar/team/t1/archive").status_code == 404
