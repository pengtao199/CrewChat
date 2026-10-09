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


def test_agent_enabled_persists(client) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    created = c.put(
        f"{PREFIX}/crewchat/agents/demo",
        json={"id": "demo", "name": "Demo", "body": "llm", "brain": "p/m", "enabled": False},
    )
    assert created.status_code == 200, created.text
    assert created.json()["enabled"] is False
    listed = c.get(f"{PREFIX}/crewchat/agents").json()["agents"]
    assert next(a for a in listed if a["id"] == "demo")["enabled"] is False


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
    "/engines/management",
    "/agents",
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


# ---- projects (workspace as first-class) -------------------------------------


def test_projects_crud(client, tmp_path) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    assert c.get(f"{PREFIX}/crewchat/projects").json() == []  # no projects by default

    workspace = tmp_path / "ws"
    workspace.mkdir()
    created = c.post(f"{PREFIX}/crewchat/projects", json={"name": "demo", "path": str(workspace)})
    assert created.status_code == 200, created.text
    project_id = created.json()["id"]
    assert created.json()["path"] == str(workspace.resolve())

    assert c.get(f"{PREFIX}/crewchat/projects/{project_id}").json()["name"] == "demo"
    assert c.put(f"{PREFIX}/crewchat/projects/{project_id}", json={"name": "renamed"}).json()["name"] == "renamed"

    bad = c.post(f"{PREFIX}/crewchat/projects", json={"name": "x", "path": str(tmp_path / "nope")})
    assert bad.status_code == 400

    assert c.delete(f"{PREFIX}/crewchat/projects/{project_id}").json() == {"success": True}
    assert c.get(f"{PREFIX}/crewchat/projects/{project_id}").status_code == 404


def test_conversation_binds_project(client, tmp_path) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    workspace = tmp_path / "ws2"
    workspace.mkdir()
    project_id = c.post(f"{PREFIX}/crewchat/projects", json={"name": "p", "path": str(workspace)}).json()["id"]

    created = c.post(f"{PREFIX}/conversations", json={"id": "c-proj", "name": "x", "project_id": project_id})
    assert created.json()["project_id"] == project_id
    assert c.get(f"{PREFIX}/conversations/c-proj").json()["project_id"] == project_id


# ---- agents are global; session workspace is three-state ---------------------


def test_agents_dir_is_global(client) -> None:  # type: ignore[no-untyped-def]
    from crewchat.core.config import get_settings
    from crewchat.core.projects import agents_dir_for

    settings = get_settings()
    assert agents_dir_for(settings) == settings.agents_home
    assert agents_dir_for(settings, "any-project") == settings.agents_home  # project ignored


def test_session_workspace_three_state(client, tmp_path) -> None:  # type: ignore[no-untyped-def]
    from crewchat.core.config import get_settings
    from crewchat.core.projects import Project
    from crewchat.orchestration import session as session_mod

    settings = get_settings()
    bound = Project(id="p", name="p", path=str(tmp_path))
    assert session_mod._session_workspace(settings, "conv:1", bound) == tmp_path

    scratch = session_mod._session_workspace(settings, "conv:abc/123", None)
    assert scratch.parent == settings.data_dir / "workspaces"
    assert "/" not in scratch.name and scratch.name  # key sanitized


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


def _fake_session(specs, events_by_agent):
    """A session whose `stream_ask` replays canned events per agent."""
    by_id = {s.id: s for s in specs}

    class _Session:
        specs = by_id

        async def stream_ask(self, spec, prompt, memory_key):
            for event in events_by_agent[spec.id]:
                yield event

    return _Session()


def _event(kind, **kwargs):
    from crewchat.executors.base import ExecEvent

    return ExecEvent(kind=kind, **kwargs)


def test_group_turn_gives_each_agent_its_own_message(client, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    """A group turn must not collapse every agent into one blob.

    Each agent replies under its own msg_id (so the renderer draws one bubble per
    speaker) and carries its own usage (so the footer is per reply, not per turn).
    """
    import asyncio

    from crewchat.api import chat as chat_mod
    from crewchat.core.models import AgentSpec

    c, _ = client
    c.post(f"{PREFIX}/conversations", json={"id": "conv-g", "name": "group"})

    specs = [AgentSpec(id="a", body="claude", brain="p/m"), AgentSpec(id="b", body="codex", brain="p/m")]
    events = {
        "a": [
            _event("delta", text="from a"),
            _event("done", meta={"model": "m1", "usage": {"input_tokens": 3, "output_tokens": 4}}),
        ],
        "b": [
            _event("delta", text="from b"),
            _event("done", meta={"model": "m2", "usage": {"input_tokens": 5, "output_tokens": 6}}),
        ],
    }
    monkeypatch.setattr(
        chat_mod, "_for_conversation", lambda _cid: (_fake_session(specs, events), specs)
    )

    sent: list[dict] = []

    class _Hub:
        async def broadcast(self, name, data):
            sent.append({"name": name, "data": data})

    monkeypatch.setattr(chat_mod, "hub", _Hub())
    chat_mod._running.clear()
    chat_mod._speaker_state.clear()

    asyncio.run(chat_mod._run_turn("conv-g", specs, "hi", "turn-1"))

    texts = [f for f in sent if f["data"].get("type") == "text"]
    # One msg_id per agent, and the group turn names its speakers.
    assert {f["data"]["msg_id"] for f in texts} == {
        f["data"]["msg_id"] for f in texts
    } and len({f["data"]["msg_id"] for f in texts}) == 2
    assert all(f["data"]["data"]["teammateMessage"] is True for f in texts)
    assert {f["data"]["data"]["senderName"] for f in texts} == {s.display for s in specs}

    finishes = [f for f in sent if f["data"].get("type") == "finish"]
    # Only the last agent ends the turn; the first must not stop the spinner.
    assert [f["data"].get("turn_complete", True) for f in finishes] == [False, True]
    assert finishes[0]["data"]["data"]["model"] == "m1"
    assert finishes[1]["data"]["data"]["model"] == "m2"

    # Each reply is persisted as its own row, tagged with its speaker + usage.
    rows = {m["metadata"]["agent_id"]: m for m in c.get(f"{PREFIX}/conversations/conv-g/messages").json()["items"]}
    assert set(rows) == {"a", "b"}
    assert rows["a"]["content"]["usage"]["model"] == "m1"
    assert rows["a"]["content"]["teammateMessage"] is True
    assert rows["a"]["content"]["senderName"] == specs[0].display


def test_retry_targets_one_agent(client, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    """Retrying one agent must not replay the whole turn's other speakers."""
    from crewchat.api import chat as chat_mod

    c, _ = client
    c.post(f"{PREFIX}/conversations", json={"id": "conv-r", "name": "retry"})
    c.post(f"{PREFIX}/conversations/conv-r/agent-ids", json={"agent_ids": []})
    c.post(f"{PREFIX}/conversations/conv-r/messages", json={"content": "ignored"})

    from crewchat.api.store import store

    store().add_message("u1", "conv-r", "user", "the question", "you", "2026-01-01T00:00:00Z")

    from crewchat.core.models import AgentSpec

    specs = [AgentSpec(id="a", body="claude", brain="p/m"), AgentSpec(id="b", body="codex", brain="p/m")]
    monkeypatch.setattr(
        chat_mod, "_for_conversation", lambda _cid: (_fake_session(specs, {"b": [_event("delta", text="b")], "a": []}), specs)
    )

    seen: list[list[str]] = []

    class _Task:
        def cancel(self):  # pragma: no cover - we only inspect the request
            pass

    def _create_task(coro):
        seen.append([s.id for s in specs])
        coro.close()
        return _Task()

    monkeypatch.setattr(chat_mod.asyncio, "create_task", _create_task)
    chat_mod._running.clear()

    r = c.post(f"{PREFIX}/conversations/conv-r/retry", json={"agent_id": "b"})
    assert r.status_code == 200
    assert r.json()["runtime"]["speaking"] == specs[1].display
