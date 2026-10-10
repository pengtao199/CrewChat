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
        assert {"id", "name", "model", "base_url", "has_key", "models"} <= set(provider)
        assert isinstance(provider["models"], list)


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
    # The stored id is the same one the stream used, so a reload does not fork a bubble.
    rows = {m["metadata"]["agent_id"]: m for m in c.get(f"{PREFIX}/conversations/conv-g/messages").json()["items"]}
    stream_ids = {f["data"]["msg_id"] for f in texts}
    assert {row["id"] for row in rows.values()} == stream_ids
    assert set(rows) == {"a", "b"}
    assert rows["a"]["content"]["usage"]["model"] == "m1"
    assert rows["a"]["content"]["teammateMessage"] is True
    assert rows["a"]["content"]["senderName"] == specs[0].display




def test_handoff_sends_the_named_agent_the_subtask(client, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    """The delegate runs the subtask, and the stream shows the handoff itself."""
    import asyncio

    from crewchat.api import chat as chat_mod
    from crewchat.core.models import AgentSpec

    c, _ = client
    c.post(f"{PREFIX}/conversations", json={"id": "conv-h", "name": "handoff"})

    a = AgentSpec(id="a", name="甲", body="opencode", brain="p/m")
    b = AgentSpec(id="b", name="乙", body="codex", brain="p/m")
    specs = [a, b]
    prompts: list[tuple[str, str]] = []
    events = {
        "a": [_event("delta", text='handoff(to="乙", task="只回答海星")')],
        "b": [_event("delta", text="海星")],
    }

    by_id = {s.id: s for s in specs}

    class _Session:
        specs = by_id

        async def stream_ask(self, spec, prompt, memory_key):  # type: ignore[no-untyped-def]
            prompts.append((spec.id, prompt))
            for event in events[spec.id]:
                yield event

    monkeypatch.setattr(chat_mod, "_for_conversation", lambda _cid: (_Session(), specs))
    sent: list[dict] = []

    class _Hub:
        async def broadcast(self, name, data):
            sent.append({"name": name, "data": data})

    monkeypatch.setattr(chat_mod, "hub", _Hub())
    chat_mod._running.clear()
    chat_mod._speaker_state.clear()

    asyncio.run(chat_mod._run_turn("conv-h", specs, "用户原话：写一首诗", "turn-h"))

    assert [spec_id for spec_id, _ in prompts] == ["a", "b"]
    delegated = prompts[1][1]
    assert "子任务：" in delegated
    assert "只回答海星" in delegated
    assert "用户的任务" not in delegated
    assert "写一首诗" not in delegated

    tools = [f for f in sent if f["data"].get("type") == "tool_call"]
    assert len(tools) == 1
    assert tools[0]["data"]["data"]["name"] == "handoff"
    assert tools[0]["data"]["data"]["description"].startswith("🤖 乙：")
    assert "只回答海星" in tools[0]["data"]["data"]["description"]

    finishes = [f for f in sent if f["data"].get("type") == "finish"]
    assert [f["data"].get("turn_complete", True) for f in finishes] == [False, True]


def test_leader_plan_runs_the_second_step_with_the_first_output(client, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    """Step 2 sees step 1's output. The stream shows owner and order."""
    import asyncio
    from collections import deque

    from crewchat.api import chat as chat_mod
    from crewchat.core.models import AgentSpec

    c, _ = client
    c.post(f"{PREFIX}/conversations", json={"id": "conv-p", "name": "plan"})

    a = AgentSpec(id="a", name="甲", body="opencode", brain="p/m")
    b = AgentSpec(id="b", name="乙", body="codex", brain="p/m")
    specs = [a, b]
    prompts: list[tuple[str, str]] = []
    replies = {
        "a": deque(
            [
                'plan(owner="甲", task="复述该数字", done="只有该数字", after="乙")\n'
                'plan(owner="乙", task="给出数字42", done="只有42")',
                "42",
                "汇总：乙先给出42，甲复述42",
            ]
        ),
        "b": deque(["42"]),
    }

    by_id = {s.id: s for s in specs}

    class _Session:
        specs = by_id

        async def stream_ask(self, spec, prompt, memory_key):  # type: ignore[no-untyped-def]
            prompts.append((spec.id, prompt))
            yield _event("delta", text=replies[spec.id].popleft())

    monkeypatch.setattr(chat_mod, "_for_conversation", lambda _cid: (_Session(), specs))
    sent: list[dict] = []

    class _Hub:
        async def broadcast(self, name, data):
            sent.append(data)

    monkeypatch.setattr(chat_mod, "hub", _Hub())
    chat_mod._running.clear()
    chat_mod._speaker_state.clear()

    asyncio.run(chat_mod._run_turn("conv-p", specs, "先查出数字再复述", "turn-p"))

    assert [spec_id for spec_id, _prompt in prompts] == ["a", "b", "a", "a"]
    assert "用户的任务" not in prompts[1][1]
    assert "给出数字42" in prompts[1][1]
    assert "先查出数字再复述" not in prompts[1][1]
    assert "42" in prompts[2][1]
    assert "复述该数字" in prompts[2][1]
    assert "负责人" in prompts[3][1]

    plans = [frame for frame in sent if frame.get("type") == "tool_call" and frame["data"]["name"] == "plan"]
    assert [frame["data"]["description"] for frame in plans] == [
        "1. 🤖 乙：给出数字42",
        "2. 🤖 甲：复述该数字",
    ]


def test_manager_without_plan_does_not_run_members(client, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    import asyncio

    from crewchat.api import chat as chat_mod
    from crewchat.core.agent_presets import MANAGER_ID
    from crewchat.core.models import AgentSpec

    c, _ = client
    c.post(f"{PREFIX}/conversations", json={"id": "conv-lead", "name": "lead"})

    leader = AgentSpec(id=MANAGER_ID, name="管理员", body="llm", brain="p/m")
    worker = AgentSpec(id="coder", name="Coder", body="llm", brain="p/m")
    specs = [leader, worker]
    called: list[str] = []
    by_id = {spec.id: spec for spec in specs}

    class _Session:
        specs = by_id

        async def stream_ask(self, spec, prompt, memory_key):  # type: ignore[no-untyped-def]
            called.append(spec.id)
            yield _event("delta", text="这题我自己就能答：42")

    class _Hub:
        async def broadcast(self, name, data):
            return None

    monkeypatch.setattr(chat_mod, "_for_conversation", lambda _cid: (_Session(), specs))
    monkeypatch.setattr(chat_mod, "hub", _Hub())
    chat_mod._running.clear()
    asyncio.run(chat_mod._run_turn("conv-lead", specs, "多少？", "t-lead"))
    assert called == [MANAGER_ID]
    rows = c.get(f"{PREFIX}/conversations/conv-lead/messages").json()["items"]
    assert [row["metadata"]["agent_id"] for row in rows] == [MANAGER_ID]
    assert "42" in rows[0]["content"]["content"]


def test_manager_plan_runs_members_then_summarizes(client, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    import asyncio
    from collections import deque

    from crewchat.api import chat as chat_mod
    from crewchat.core.agent_presets import MANAGER_ID
    from crewchat.core.models import AgentSpec

    c, _ = client
    c.post(f"{PREFIX}/conversations", json={"id": "conv-mgr", "name": "mgr"})

    leader = AgentSpec(id=MANAGER_ID, name="管理员", body="llm", brain="p/m")
    worker = AgentSpec(id="coder", name="Coder", body="llm", brain="p/m")
    specs = [leader, worker]
    prompts: list[tuple[str, str]] = []
    replies = {
        MANAGER_ID: deque(
            [
                '先拆开。\nplan(owner="Coder", task="只回答海星", done="只有海星")',
                "汇总：Coder 回答了海星",
            ]
        ),
        "coder": deque(["海星"]),
    }
    by_id = {spec.id: spec for spec in specs}

    class _Session:
        specs = by_id

        async def stream_ask(self, spec, prompt, memory_key):  # type: ignore[no-untyped-def]
            prompts.append((spec.id, prompt))
            yield _event("delta", text=replies[spec.id].popleft())

    sent: list[dict] = []

    class _Hub:
        async def broadcast(self, name, data):
            sent.append(data)

    monkeypatch.setattr(chat_mod, "_for_conversation", lambda _cid: (_Session(), specs))
    monkeypatch.setattr(chat_mod, "hub", _Hub())
    chat_mod._running.clear()
    asyncio.run(chat_mod._run_turn("conv-mgr", specs, "用户原话不要复述", "t-mgr"))

    assert [spec_id for spec_id, _ in prompts] == [MANAGER_ID, "coder", MANAGER_ID]
    assert "只回答海星" in prompts[1][1]
    assert "用户原话不要复述" not in prompts[1][1]
    assert "各步结果" in prompts[2][1]
    assert "海星" in prompts[2][1]

    rows = c.get(f"{PREFIX}/conversations/conv-mgr/messages").json()["items"]
    lead = next(row for row in rows if row["metadata"]["agent_id"] == MANAGER_ID)
    assert "plan(" not in lead["content"]["content"]
    assert "先拆开" in lead["content"]["content"]


def test_manager_summary_includes_a_failed_member(client, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    import asyncio
    from collections import deque

    from crewchat.api import chat as chat_mod
    from crewchat.core.agent_presets import MANAGER_ID
    from crewchat.core.models import AgentSpec

    c, _ = client
    c.post(f"{PREFIX}/conversations", json={"id": "conv-fail", "name": "fail"})

    leader = AgentSpec(id=MANAGER_ID, name="管理员", body="llm", brain="p/m")
    worker = AgentSpec(id="coder", name="Coder", body="llm", brain="p/m")
    specs = [leader, worker]
    prompts: list[str] = []
    replies = {MANAGER_ID: deque(['plan(owner="Coder", task="改文件", done="有 diff")', "成员失败了"])}
    by_id = {spec.id: spec for spec in specs}

    class _Session:
        specs = by_id

        async def stream_ask(self, spec, prompt, memory_key):  # type: ignore[no-untyped-def]
            if spec.id == MANAGER_ID:
                prompts.append(prompt)
                yield _event("delta", text=replies[MANAGER_ID].popleft())
                return
            yield _event("error", text="引擎未安装")

    class _Hub:
        async def broadcast(self, name, data):
            return None

    monkeypatch.setattr(chat_mod, "_for_conversation", lambda _cid: (_Session(), specs))
    monkeypatch.setattr(chat_mod, "hub", _Hub())
    chat_mod._running.clear()
    asyncio.run(chat_mod._run_turn("conv-fail", specs, "改一处", "t-fail"))
    assert any("失败" in prompt for prompt in prompts)
    assert any("引擎未安装" in prompt for prompt in prompts)


def test_cancel_does_not_run_a_queued_handoff(client, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    """Cancelling the turn drops the queue. The delegate must not start."""
    import asyncio

    from crewchat.api import chat as chat_mod
    from crewchat.core.models import AgentSpec

    c, _ = client
    c.post(f"{PREFIX}/conversations", json={"id": "conv-c", "name": "cancel"})

    a = AgentSpec(id="a", name="甲", body="opencode", brain="p/m")
    b = AgentSpec(id="b", name="乙", body="codex", brain="p/m")
    specs = [a, b]
    called: list[str] = []

    by_id = {s.id: s for s in specs}

    class _Session:
        specs = by_id

        async def stream_ask(self, spec, prompt, memory_key):  # type: ignore[no-untyped-def]
            called.append(spec.id)
            if spec.id == "a":
                yield _event("delta", text='handoff(to="乙", task="只回答海星")')
                task = asyncio.current_task()
                assert task is not None
                task.cancel()
                return
            yield _event("delta", text="海星")

    monkeypatch.setattr(chat_mod, "_for_conversation", lambda _cid: (_Session(), specs))

    class _Hub:
        async def broadcast(self, name, data):
            return None

    monkeypatch.setattr(chat_mod, "hub", _Hub())
    chat_mod._running.clear()
    chat_mod._speaker_state.clear()

    async def _go() -> None:
        task = asyncio.create_task(chat_mod._run_turn("conv-c", specs, "用户原话", "turn-c"))
        try:
            await task
        except asyncio.CancelledError:
            pass

    asyncio.run(_go())
    assert called == ["a"]


def test_send_broadcasts_user_message_and_turn_completion(client, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    """A send must echo the user's row and close the turn on the socket.

    The renderer inserts the user bubble from `message.userCreated` and releases
    its running state from `turn.completed`. Without either, the message only
    appears after a history refetch and the conversation stays stuck "running".
    """
    import asyncio

    from crewchat.api import chat as chat_mod
    from crewchat.core.models import AgentSpec

    c, _ = client
    c.post(f"{PREFIX}/conversations", json={"id": "conv-s", "name": "send"})

    specs = [AgentSpec(id="a", body="llm", brain="p/m")]
    sent: list[tuple[str, dict]] = []

    class _Hub:
        async def broadcast(self, name, data):
            sent.append((name, data))

    monkeypatch.setattr(chat_mod, "hub", _Hub())
    monkeypatch.setattr(
        chat_mod, "_for_conversation", lambda _cid: (_fake_session(specs, {"a": [_event("delta", text="hi")]}), specs)
    )
    chat_mod._running.clear()
    chat_mod._speaker_state.clear()

    r = c.post(f"{PREFIX}/conversations/conv-s/messages", json={"content": "hello"})
    assert r.status_code == 200
    names = [n for n, _ in sent]
    assert "message.userCreated" in names
    assert "turn.completed" in names
    # The echoed row must be the one persisted, and land before the turn closes.
    created = dict(sent)["message.userCreated"]
    assert created["msg_id"] == r.json()["msg_id"]
    assert created["position"] == "right"
    assert created["content"] == "hello"
    assert names.index("message.userCreated") < names.index("turn.completed")

    completed = dict(sent)["turn.completed"]
    assert completed["session_id"] == "conv-s"
    assert completed["runtime"]["state"] == "idle"
    assert completed["runtime"]["can_send_message"] is True


def test_reply_carries_agent_identity_for_reload(client, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    """Every reply names its agent, so a reloaded thread still shows who spoke."""
    import asyncio

    from crewchat.api import chat as chat_mod
    from crewchat.core.models import AgentSpec

    c, _ = client
    c.post(f"{PREFIX}/conversations", json={"id": "conv-i", "name": "id"})

    specs = [AgentSpec(id="solo", body="llm", brain="p/m")]
    sent: list[dict] = []

    class _Hub:
        async def broadcast(self, name, data):
            sent.append(data)

    monkeypatch.setattr(chat_mod, "hub", _Hub())
    monkeypatch.setattr(
        chat_mod, "_for_conversation", lambda _cid: (_fake_session(specs, {"solo": [_event("delta", text="yo")]}), specs)
    )
    chat_mod._running.clear()
    chat_mod._speaker_state.clear()

    asyncio.run(chat_mod._run_turn("conv-i", specs, "hi", "t1"))

    texts = [f for f in sent if f.get("type") == "text"]
    assert texts and texts[0]["data"]["senderName"] == specs[0].display
    assert texts[0]["data"]["senderAgentId"] == "solo"
    assert texts[0]["data"]["senderAgentType"] == "llm"
    # A 1:1 reply is not a teammate reply.
    assert texts[0]["data"]["teammateMessage"] is False

    row = c.get(f"{PREFIX}/conversations/conv-i/messages").json()["items"][0]
    assert row["content"]["senderAgentId"] == "solo"
    assert row["content"]["senderAgentType"] == "llm"


def test_tool_frame_carries_input_and_output() -> None:  # type: ignore[no-untyped-def]
    """The card's label must describe the call, not just name it.

    The renderer shows `${name} · ${description}` and expands args/output. Frames
    that omitted args/output left every card saying only "Bash" or "Read" — no
    command, no file, and no result behind it.
    """
    from crewchat.api.hub import tool_frame

    frame = tool_frame("c", "m", "Bash", "running", "t", "call_1", {"command": "ls -la"})
    data = frame["data"]
    assert data["name"] == "Bash"
    assert data["status"] == "running"
    assert data["args"] == {"command": "ls -la"}
    assert data["description"] == "ls -la"

    # A terminal frame without args must leave the running frame's values alone —
    # the renderer shallow-merges by call_id, so sending empty args would blank them.
    frame = tool_frame("c", "m", "Bash", "completed", "t", "call_1", result="hello\n")
    data = frame["data"]
    assert data["status"] == "completed"
    assert data["output"] == "hello\n"
    assert "args" not in data and "description" not in data

    # A frame with no details at all must not describe nothing.
    assert "description" not in tool_frame("c", "m", "Bash", "running")["data"]


# ---- conversation roster (single vs group) -----------------------------------


def test_manager_restore_does_not_overwrite(client) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    missing = c.get(f"{PREFIX}/crewchat/agents/manager").json()
    assert missing["exists"] is False
    assert missing["ready"] is False
    assert missing["reason"] == "missing"

    first = c.post(f"{PREFIX}/crewchat/agents/manager/restore").json()
    assert first["created"] is True
    assert first["exists"] is True
    assert first["has_brain"] is False
    assert first["reason"] == "unconfigured"

    edited = c.put(
        f"{PREFIX}/crewchat/agents/crewchat-manager",
        json={"name": "室长", "body": "llm", "brain": "p/m", "enabled": True},
    )
    assert edited.status_code == 200, edited.text
    again = c.post(f"{PREFIX}/crewchat/agents/manager/restore").json()
    assert again["created"] is False
    assert again["has_brain"] is True
    assert c.get(f"{PREFIX}/crewchat/agents/crewchat-manager").json()["name"] == "室长"


def test_create_conversation_binds_single_agent(client) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    c.put(f"{PREFIX}/crewchat/agents/coder", json={"name": "Coder", "body": "llm", "brain": "p/m"})
    created = c.post(
        f"{PREFIX}/conversations",
        json={"id": "solo-1", "name": "solo", "agent": {"id": "coder"}},
    )
    assert created.status_code == 200, created.text
    body = created.json()
    assert body["extra"]["team_agent_ids"] == ["coder"]
    assert body["extra"]["leader_agent_id"] is None
    assert c.get(f"{PREFIX}/conversations/solo-1").json()["extra"]["team_agent_ids"] == ["coder"]


def test_create_group_requires_project_and_ready_manager(client, tmp_path) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    workspace = tmp_path / "ws-group"
    workspace.mkdir()
    project_id = c.post(
        f"{PREFIX}/crewchat/projects", json={"name": "g", "path": str(workspace)}
    ).json()["id"]
    c.put(f"{PREFIX}/crewchat/agents/coder", json={"name": "Coder", "body": "llm", "brain": "p/m"})

    missing_project = c.post(
        f"{PREFIX}/conversations",
        json={"extra": {"team_agent_ids": ["coder"]}},
    )
    assert missing_project.status_code == 400
    assert missing_project.json()["error"] == "missing_project"

    no_manager = c.post(
        f"{PREFIX}/conversations",
        json={"project_id": project_id, "extra": {"team_agent_ids": ["coder"]}},
    )
    assert no_manager.status_code == 400
    assert no_manager.json()["error"] == "manager_missing"

    c.post(f"{PREFIX}/crewchat/agents/manager/restore")
    unconfigured = c.post(
        f"{PREFIX}/conversations",
        json={"project_id": project_id, "extra": {"team_agent_ids": ["coder"]}},
    )
    assert unconfigured.status_code == 400
    assert unconfigured.json()["error"] == "manager_unconfigured"

    c.put(
        f"{PREFIX}/crewchat/agents/crewchat-manager",
        json={"name": "管理员", "body": "llm", "brain": "p/m", "enabled": True},
    )
    unknown = c.post(
        f"{PREFIX}/conversations",
        json={"project_id": project_id, "extra": {"team_agent_ids": ["ghost"]}},
    )
    assert unknown.status_code == 400
    assert unknown.json()["error"] == "unknown_agent"

    created = c.post(
        f"{PREFIX}/conversations",
        json={
            "id": "group-1",
            "name": "group",
            "project_id": project_id,
            "extra": {"team_agent_ids": ["coder", "coder"], "leader_agent_id": "crewchat-manager"},
        },
    )
    assert created.status_code == 200, created.text
    extra = created.json()["extra"]
    assert extra["team_agent_ids"] == ["crewchat-manager", "coder"]
    assert extra["leader_agent_id"] == "crewchat-manager"
    assert created.json()["project_id"] == project_id


def test_send_rejects_unbound_conversation(client) -> None:  # type: ignore[no-untyped-def]
    c, _ = client
    assert c.post(f"{PREFIX}/conversations", json={"id": "empty-1", "name": "x"}).status_code == 200
    r = c.post(f"{PREFIX}/conversations/empty-1/messages", json={"content": "hi"})
    assert r.status_code == 400
    assert r.json()["error"] == "no_agent_in_conversation"
