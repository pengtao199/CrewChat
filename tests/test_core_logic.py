"""纯逻辑回归：provider/model 数据形状、标签推导、网关投影、版本号。

这些是无副作用、无网络、无文件系统的判定，适合做快速回归网（`uv run pytest`）。
端到端行为（真实对话 / 网关进程）按 `AGENTS/verification.md` 另行实跑，不在此覆盖。
"""

from __future__ import annotations

import pytest

from crewchat.core import version
from crewchat.core.gateway_ccswitch import _build_settings_config
from crewchat.core.model_catalog import infer_tags, known_context_limit
from crewchat.core.providers import Model, Provider, remove_model, upsert_model

# ---- 版本号 ------------------------------------------------------------------


def test_backend_version_is_v1() -> None:
    assert version.backend_version() == "v1"
    assert version.backend_version_number() == 1


@pytest.mark.parametrize(
    ("value", "expected"),
    [("v1", True), ("v12", True), ("v0", False), ("1", False), ("vx", False), ("v1.0", False)],
)
def test_is_valid_backend_version(value: str, expected: bool) -> None:
    assert version.is_valid_backend_version(value) is expected


def test_version_info_shape() -> None:
    info = version.version_info()
    assert info["backend"]["version"] == "v1"
    assert info["backend"]["scheme"] == "increment"
    assert info["frontend"]["scheme"] == "semver"


# ---- Model 兼容层 -------------------------------------------------------------


def test_model_from_raw_accepts_bare_string() -> None:
    model = Model.from_raw("gpt-4o")
    assert model is not None and model.id == "gpt-4o"


def test_model_from_raw_rejects_empty_and_non_string() -> None:
    assert Model.from_raw("") is None
    assert Model.from_raw("   ") is None
    assert Model.from_raw(123) is None
    assert Model.from_raw({"display_name": "no id"}) is None


def test_model_from_raw_filters_unknown_tags() -> None:
    model = Model.from_raw({"id": "m", "tags": ["vision", "bogus"]})
    assert model is not None and model.tags == ["vision"]


def test_model_label_falls_back_to_id() -> None:
    assert Model(id="m", display_name="Pretty").label == "Pretty"
    assert Model(id="m").label == "m"


# ---- Provider 方法与键处理 ----------------------------------------------------


def _provider(**overrides: object) -> Provider:
    base = dict(id="my-endpoint", name="n", base_url="https://x/v1", model="m")
    base.update(overrides)
    return Provider(**base)  # type: ignore[arg-type]


def test_provider_env_key_is_uppercased_and_sanitized() -> None:
    assert _provider(id="my.endpoint-1").env_key() == "MY_ENDPOINT_1_API_KEY"


def test_provider_key_count_splits_newlines_before_commas() -> None:
    """A newline separates accounts. A comma inside a line stays part of the secret."""
    newline = _provider(api_key="a\nb, c")
    assert newline.key_count() == 2
    assert newline.default_secret() == "a"
    comma = _provider(api_key="a, b")
    assert comma.key_count() == 2
    assert comma.default_secret() == "a"
    assert _provider(api_key="").key_count() == 0


def test_provider_keys_round_trip_with_remarks_and_keep_unedited_secrets() -> None:
    from crewchat.core.providers import replace_keys

    provider = _provider(api_key="sk-live")
    assert provider.keys[0].id == "legacy-1"
    replace_keys(
        provider,
        [
            {"id": "legacy-1", "remark": "工作", "secret": "••••••••", "is_default": False},
            {"id": "personal", "remark": "个人", "secret": "sk-other", "is_default": True},
        ],
    )
    assert provider.default_secret() == "sk-other"
    assert provider.keys[0].secret == "sk-live"
    assert provider.keys[0].remark == "工作"
    saved = provider.as_dict(reveal=True)
    assert saved["api_key"] == "sk-other"
    assert "sk-live" not in saved["api_key"]
    loaded = Provider.from_dict(saved)
    assert loaded.key_count() == 2
    assert loaded.default_secret() == "sk-other"
    assert {key.remark for key in loaded.keys} == {"工作", "个人"}


def test_provider_as_dict_masks_key_unless_revealed() -> None:
    provider = _provider(api_key="secret")
    assert provider.as_dict()["api_key"] == "已设置"
    assert provider.as_dict(reveal=True)["api_key"] == "secret"
    assert _provider(api_key="").as_dict()["api_key"] == ""


def test_upsert_and_remove_model() -> None:
    provider = _provider(models=["a", "b"])
    upsert_model(provider, Model(id="a", display_name="A2"))
    assert provider.get_model("a") is not None
    assert provider.get_model("a").display_name == "A2"  # type: ignore[union-attr]
    assert provider.model_ids() == ["a", "b"]

    upsert_model(provider, Model(id="c"))
    assert provider.model_ids() == ["a", "b", "c"]

    assert remove_model(provider, "zzz") is False
    assert remove_model(provider, "b") is True
    assert provider.model_ids() == ["a", "c"]


# ---- 标签推导 -----------------------------------------------------------------


def test_infer_tags_embedding_short_circuits() -> None:
    assert infer_tags(Model(id="text-embedding-3-small")) == ["embedding"]


def test_infer_tags_unknown_model_is_text_only() -> None:
    assert infer_tags(Model(id="acme-model-1")) == ["text"]


def test_infer_tags_uses_enriched_vision_over_name() -> None:
    model = Model(id="acme-vision-pro")
    assert infer_tags(model, {"capabilities": {"vision": True}}) == ["text", "vision"]


def test_infer_tags_free_from_suffix() -> None:
    assert infer_tags(Model(id="meta/llama-3.3:free")) == ["free"]


def test_infer_tags_free_from_zero_price() -> None:
    assert infer_tags(Model(id="acme-zero"), {"price": {"input": 0, "output": 0}}) == ["free"]


def test_infer_tags_reasoning_by_name_convention() -> None:
    assert infer_tags(Model(id="deepseek-r1")) == ["reasoning", "tool_use"]


def test_infer_tags_long_context_by_name_convention() -> None:
    assert infer_tags(Model(id="acme-200k")) == ["long_context"]


def test_infer_tags_long_context_by_context_limit() -> None:
    assert "long_context" in infer_tags(Model(id="acme-x", context_limit=400_000))


def test_known_context_limit() -> None:
    assert known_context_limit("openai/gpt-4.1-mini") == 1_047_576
    assert known_context_limit("some-unknown-model") is None


# ---- 网关投影（通用 provider → 各 CLI 的 settings_config） ---------------------


def test_build_settings_config_codex_uses_responses_wire_api() -> None:
    provider = _provider(base_url="https://up/v1", api_key="k", api_format="openai_responses")
    settings_config, meta = _build_settings_config("codex", provider)
    assert 'wire_api = "responses"' in settings_config["config"]
    assert settings_config["auth"]["OPENAI_API_KEY"] == "k"
    assert meta["apiFormat"] == "openai_responses"


def test_build_settings_config_claude_sets_anthropic_base_url() -> None:
    provider = _provider(base_url="https://up/v1", api_key="k", api_format="anthropic")
    settings_config, meta = _build_settings_config("claude", provider)
    assert settings_config["env"]["ANTHROPIC_BASE_URL"] == "https://up/v1"
    assert settings_config["env"]["ANTHROPIC_AUTH_TOKEN"] == "k"
    assert meta["apiFormat"] == "anthropic"


def test_build_settings_config_opencode_lists_models() -> None:
    provider = _provider(base_url="https://up/v1", api_key="k", models=["a", "b"])
    settings_config, _ = _build_settings_config("opencode", provider)
    assert settings_config["options"]["baseURL"] == "https://up/v1"
    assert settings_config["base_url"] == "https://up/v1"
    assert settings_config["api_key"] == "k"
    assert set(settings_config["models"]) == {"a", "b"}


def test_build_settings_config_rejects_unknown_app() -> None:
    with pytest.raises(ValueError):
        _build_settings_config("cursor", _provider())


# ---- brain resolution -------------------------------------------------------


def test_resolve_brain_splits_provider_and_overrides_model(monkeypatch) -> None:
    from crewchat.core import providers as P
    from crewchat.core.models import AgentSpec

    pool = {"1fd10b59": _provider(id="1fd10b59", model="default-model")}
    monkeypatch.setattr(P, "get_provider", lambda _settings, pid: pool.get(pid) if pid else None)

    # `providerId/modelId` → provider resolved AND model overridden (modelId may contain '/').
    spec = AgentSpec(id="a", body="claude", brain="1fd10b59/nvidia/nemotron:free")
    provider = P.resolve_brain(object(), spec)
    assert provider is not None
    assert provider.id == "1fd10b59"
    assert provider.model == "nvidia/nemotron:free"

    # CLI body without a model part → leave the CLI's own brain alone.
    assert P.resolve_brain(object(), AgentSpec(id="b", body="claude", brain="1fd10b59")) is None

    # llm body resolves even without a model part.
    llm = P.resolve_brain(object(), AgentSpec(id="c", body="llm", brain="1fd10b59"))
    assert llm is not None and llm.id == "1fd10b59"


def test_only_real_session_ids_are_resumed(monkeypatch, tmp_path) -> None:  # type: ignore[no-untyped-def]
    """A session id must come from the CLI, never be invented.

    Resuming a made-up id makes the CLI exit 1 (`--resume` on an unknown session
    is an error, not a create request), which is why every fresh conversation
    used to fail on its very first turn.
    """
    import asyncio

    from crewchat.core.models import AgentSpec
    from crewchat.orchestration import session as session_mod

    spec = AgentSpec(id="a1", body="llm", brain="p/m")

    class _Executor:
        needs_gateway = False
        seen: list[str | None] = []

        async def run(self, req, provider):  # noqa: ANN001
            _Executor.seen.append(req.session)
            from crewchat.executors.base import ExecResult

            # The CLI mints a real id and hands it back on the result.
            return ExecResult(text="ok", session="real-cli-session-id")

    monkeypatch.setattr(session_mod, "get_executor", lambda _body: _Executor())

    from crewchat.core.config import get_settings

    session = session_mod.Session.__new__(session_mod.Session)
    session.workspace_dir = tmp_path
    session.settings = get_settings()
    session.specs = {"a1": spec}
    session.sessions = {}
    session.last_meta = {}
    session.project = None

    async def drain():
        out = []
        async for ev in session.stream_ask(spec, "hi", "conv:abc"):
            out.append(ev.kind)
        return out

    # First turn: nothing to resume yet.
    asyncio.run(drain())
    assert _Executor.seen == [None], "an invented session id was passed to the executor"
    # The CLI's real id is remembered for this conversation+agent.
    assert session.sessions["conv:abc:a1"] == "real-cli-session-id"

    # Second turn resumes it, and a different conversation does not reuse it.
    asyncio.run(drain())
    assert _Executor.seen[-1] == "real-cli-session-id"
    asyncio.run(_drain_other(session, spec))
    assert _Executor.seen[-1] is None


async def _drain_other(session, spec) -> None:  # type: ignore[no-untyped-def]
    async for _ in session.stream_ask(spec, "hi", "conv:other"):
        pass


def test_group_prompt_carries_prior_outputs_in_order() -> None:
    """A later speaker must see earlier speakers' output — that is the collaboration.

    Without it every agent answers the same question in parallel; with it a
    reviewer can act on a specific contribution.
    """
    from crewchat.core.models import AgentSpec
    from crewchat.orchestration.group import PER_OUTPUT_LIMIT, build_prompt

    a = AgentSpec(id="a", name="甲", role="写代码")
    b = AgentSpec(id="b", name="乙", role="审代码")

    # First speaker: no prior block at all.
    first = build_prompt(a, [a, b], "实现 slugify", [])
    assert "已经产出" not in first
    assert "写代码" in first  # own persona present
    assert "在场 agent（你之外）：🤖 乙" in first
    assert "实现 slugify" in first

    # Second speaker: both prior blocks, named and ordered.
    second = build_prompt(b, [a, b], "实现 slugify", [("🤖 甲", "我写了 def slugify(): ...")])
    assert "已经产出" in second
    assert "--- 🤖 甲 ---" in second
    assert "def slugify" in second
    assert second.index("已经产出") < second.index("用户的任务")

    # Order is preserved across several prior speakers.
    many = build_prompt(b, [a, b], "t", [("🤖 甲", "A"), ("🤖 丙", "C")])
    assert many.index("--- 🤖 甲 ---") < many.index("--- 🤖 丙 ---")


def test_handoff_request_is_recognized_and_queued() -> None:
    """A handoff is a request the loop accepts, not a process the agent starts."""
    from crewchat.core.models import AgentSpec
    from crewchat.orchestration.group import (
        accept_handoff,
        parse_handoff,
        plan_steps,
        resolve_handoff,
    )

    a = AgentSpec(id="a", name="甲")
    b = AgentSpec(id="b", name="乙")
    peers = [a, b]

    assert parse_handoff("只是提到 handoff 这个词") is None
    assert parse_handoff('handoff(to="", task="x")') is None
    assert parse_handoff('handoff(to="乙", task="只回答海星")') == ("乙", "只回答海星")

    assert resolve_handoff("乙", peers, "a") == b
    assert resolve_handoff("b", peers, "a") == b
    assert resolve_handoff("甲", peers, "a") is None  # not yourself
    assert resolve_handoff("不在场", peers, "a") is None

    steps = plan_steps(peers, "用户原话")
    assert accept_handoff(steps, 0, b, "只回答海星", "🤖 甲", max_steps=4)
    assert [(step.spec.id, step.kind, step.task) for step in steps] == [
        ("a", "roster", "用户原话"),
        ("b", "handoff", "只回答海星"),
    ]
    # Already queued: a second request for the same agent is not stacked.
    assert not accept_handoff(steps, 0, b, "另一件", "🤖 甲", max_steps=4)


def test_plan_runs_in_dependency_order_and_drops_unknown_owners() -> None:
    """A later step waits for the owner it names, and someone absent is not queued."""
    from crewchat.core.models import AgentSpec
    from crewchat.orchestration.group import apply_plan, parse_plan, plan_steps

    a = AgentSpec(id="a", name="甲")
    b = AgentSpec(id="b", name="乙")
    text = (
        'plan(owner="甲", task="复述该数字", done="只有该数字", after="乙")\n'
        'plan(owner="乙", task="给出数字42", done="只有42")\n'
        'plan(owner="不在场", task="忽略", done="无")'
    )
    tasks = parse_plan(text)
    assert [task.agent for task in tasks] == ["甲", "乙", "不在场"]
    assert tasks[0].context == ["乙"]
    assert tasks[0].expected_output == "只有该数字"

    steps = plan_steps([a, b], "用户原话")
    queued = apply_plan(steps, 0, tasks, [a, b], a, max_steps=4)
    assert [spec.name for spec, _task in queued] == ["乙", "甲"]
    assert [(step.spec.name, step.kind) for step in steps] == [
        ("甲", "roster"),
        ("乙", "handoff"),
        ("甲", "handoff"),
        ("甲", "handoff"),
    ]
    assert "给出数字42" in steps[1].task
    assert "用户原话" not in steps[1].task
    assert "负责人" in steps[-1].task


def test_lead_steps_start_with_the_manager_only() -> None:
    from crewchat.core.agent_presets import MANAGER_ID
    from crewchat.core.models import AgentSpec
    from crewchat.orchestration.group import lead_steps, strip_control_calls

    leader = AgentSpec(id=MANAGER_ID, name="管理员")
    steps = lead_steps(leader, "拆这个任务")
    assert [(step.spec.id, step.kind, step.task) for step in steps] == [
        (MANAGER_ID, "lead", "拆这个任务")
    ]
    assert (
        strip_control_calls(
            '先说明意图\nplan(owner="甲", task="写", done="有文件")\n然后等他们。'
        )
        == "先说明意图\n然后等他们。"
    )


def test_group_prompt_truncates_oversized_prior_output() -> None:
    """A long upstream reply must not be pasted whole into the next speaker."""
    from crewchat.core.models import AgentSpec
    from crewchat.orchestration.group import build_prompt

    a = AgentSpec(id="a", name="甲")
    spec = AgentSpec(id="b", name="乙")
    huge = "x" * 50_000
    prompt = build_prompt(spec, [a, spec], "t", [("🤖 甲", huge)], limit=100)

    assert "已截断" in prompt
    assert huge not in prompt
    assert len(prompt) < 1000


def test_opencode_config_carries_own_provider_not_the_users() -> None:
    """Each opencode run must get its own config, not inherit the user's global one.

    Inheriting the global config made every agent carry all of the user's MCP
    servers (one simple reply took ~47s and any global edit leaked into every
    agent). The inline config must carry exactly the brain provider.
    """
    import json

    from crewchat.core.providers import Provider
    from crewchat.executors.acp_body import _inline_config

    prov = Provider(
        id="agnes", name="Agnes", base_url="https://apihub.agnes-ai.com/v1",
        model="ccs-opencode-agnes-ai/agnes-2.5-flash", api_key="sk-test",
    )
    cfg = json.loads(_inline_config(prov, [], [], "opencode"))

    assert "mcp" not in cfg  # no inherited servers
    block = cfg["providers"]["gw"]
    # baseURL points at the local reverse proxy with the app segment, not the upstream
    assert block["settings"]["baseURL"] == "http://127.0.0.1:15721/opencode/v1"
    # the key is the gateway placeholder, never the real provider key
    assert block["settings"]["apiKey"] == "PROXY_MANAGED"
    assert block["package"] == "aisdk:@ai-sdk/openai-compatible"
    assert "sk-test" not in json.dumps(cfg)
    assert "agnes" not in cfg["providers"]
    # catalog name stays slash-free; modelID is the published id the gateway routes on
    assert block["models"]["main"]["modelID"] == "ccs-opencode-agnes-ai/agnes-2.5-flash"
    assert cfg["model"] == "gw/main"

    from pathlib import Path

    from crewchat.executors.acp_body import _opencode_env, _release_homes
    from crewchat.executors.base import ExecRequest

    env: dict[str, str] = {"OPENCODE_SERVER_PASSWORD": "nope"}
    _opencode_env(prov, ExecRequest(prompt="hi", cwd=Path("/tmp"), app_id="opencode"), env)
    try:
        written = json.loads(Path(env["OPENCODE_CONFIG_DIR"]).joinpath("opencode.json").read_text())
        assert written["providers"]["gw"]["models"]["main"]["modelID"] == "ccs-opencode-agnes-ai/agnes-2.5-flash"
        assert "sk-test" not in json.dumps(written)
        assert env["OPENCODE_TEST_HOME"]
        assert env["OPENCODE_DISABLE_PROJECT_CONFIG"] == "1"
        assert "OPENCODE_SERVER_PASSWORD" not in env
    finally:
        _release_homes(env)


def test_claude_and_codex_homes_keep_published_ids_and_the_placeholder() -> None:
    """Each spawn writes its own home. The real key stays out of the file and the env."""
    from pathlib import Path

    from crewchat.core.providers import Provider
    from crewchat.executors.acp_body import (
        PROXY_TOKEN,
        _claude_env,
        _codex_env,
        _release_homes,
        _scrub_secret,
    )
    from crewchat.executors.base import ExecRequest

    claude = Provider(
        id="deepseek", name="DeepSeek", base_url="https://api.deepseek.com",
        model="ccs-claude-deepseek--deepseek-v4", api_key="sk-test",
    )
    env = {"DEEPSEEK_API_KEY": "sk-test"}
    _scrub_secret(env, claude.api_key)
    _claude_env(claude, ExecRequest(prompt="hi", cwd=Path("/tmp"), app_id="claude"), env)
    try:
        blob = Path(env["CLAUDE_CONFIG_DIR"]).joinpath("settings.json").read_text()
        assert "ccs-claude-deepseek--deepseek-v4" in blob
        assert PROXY_TOKEN in blob
        assert "sk-test" not in blob
        assert "sk-test" not in env.values()
    finally:
        _release_homes(env)

    left = Provider(
        id="deepseek", name="DeepSeek", base_url="https://api.deepseek.com",
        model="ccs-deepseek/deepseek/deepseek-v4-pro", api_key="sk-left",
    )
    right = Provider(
        id="agnes", name="Agnes", base_url="https://api.agnes.test",
        model="ccs-agnes/agnes-2", api_key="sk-right",
    )
    left_env: dict[str, str] = {}
    right_env: dict[str, str] = {}
    req = ExecRequest(prompt="hi", cwd=Path("/tmp"), app_id="codex")
    _codex_env(left, req, left_env)
    _codex_env(right, req, right_env)
    try:
        left_text = Path(left_env["CODEX_HOME"]).joinpath("config.toml").read_text()
        right_text = Path(right_env["CODEX_HOME"]).joinpath("config.toml").read_text()
        assert left_env["CODEX_HOME"] != right_env["CODEX_HOME"]
        assert "ccs-deepseek/deepseek/deepseek-v4-pro" in left_text
        assert "ccs-agnes/agnes-2" in right_text
        assert 'wire_api = "responses"' in left_text
        assert "127.0.0.1:15721/v1" in left_text
        assert PROXY_TOKEN in left_text and PROXY_TOKEN in right_text
        assert "sk-" not in left_text and "sk-" not in right_text
    finally:
        _release_homes(left_env)
        _release_homes(right_env)


def test_every_cli_process_keeps_its_own_published_id() -> None:
    """Different sessions of the same CLI carry different published ids, not one current route."""
    from pathlib import Path

    from crewchat.core.providers import Provider
    from crewchat.executors.acp_body import (
        PROXY_TOKEN,
        _gemini_env,
        _grok_env,
        _hermes_env,
        _mcode_env,
        _openclaw_env,
        _pi_env,
        _release_homes,
    )
    from crewchat.executors.base import ExecRequest

    def prov(model: str) -> Provider:
        return Provider(id="deepseek", name="DeepSeek", base_url="https://api.deepseek.com", model=model, api_key="sk-test")

    left = "ccs-hermes-deepseek/deepseek-v4-pro"
    right = "ccs-hermes-agnes/agnes-2"
    homes: list[dict[str, str]] = []

    def bind(hook, app: str, model: str) -> dict[str, str]:
        env: dict[str, str] = {}
        hook(prov(model), ExecRequest(prompt="hi", cwd=Path("/tmp"), app_id=app), env)
        homes.append(env)
        return env

    try:
        gemini = bind(_gemini_env, "gemini", "ccs-gemini-deepseek--deepseek-v4-pro")
        assert gemini["GEMINI_MODEL"] == "ccs-gemini-deepseek--deepseek-v4-pro"
        assert gemini["GEMINI_API_KEY"] == PROXY_TOKEN
        assert "sk-test" not in gemini.values()

        grok_a = bind(_grok_env, "grok", left)
        grok_b = bind(_grok_env, "grok", right)
        assert grok_a["GROK_HOME"] != grok_b["GROK_HOME"]
        text = Path(grok_a["GROK_CONFIG_PATH"]).read_text()
        assert left in text and PROXY_TOKEN in text and "sk-test" not in text
        assert "/grokbuild/v1" in text

        hermes = bind(_hermes_env, "hermes", left)
        hermes_text = Path(hermes["HERMES_HOME"]).joinpath("config.yaml").read_text()
        assert left in hermes_text and PROXY_TOKEN in hermes_text and "sk-" not in hermes_text
        assert "api_mode: chat_completions" in hermes_text
        assert "discover_models: false" in hermes_text
        assert Path(hermes["HERMES_HOME"]).is_relative_to(Path.home() / ".hermes" / "crewchat-sessions")

        pi = bind(_pi_env, "pi", left)
        pi_text = Path(pi["PI_CODING_AGENT_DIR"]).joinpath("models.json").read_text()
        assert left in pi_text and PROXY_TOKEN in pi_text and "sk-" not in pi_text
        assert "openai-completions" in pi_text
        pi_settings = Path(pi["PI_CODING_AGENT_DIR"]).joinpath("settings.json").read_text()
        assert '"quietStartup": true' in pi_settings

        mcode = bind(_mcode_env, "mcode", left)
        mcode_text = Path(mcode["MINIMAX_DATA_DIR"]).joinpath("config.yaml").read_text()
        assert left in mcode_text and PROXY_TOKEN in mcode_text and "sk-" not in mcode_text

        claw = bind(_openclaw_env, "openclaw", left)
        claw_text = Path(claw["OPENCLAW_CONFIG_PATH"]).read_text()
        assert left in claw_text and PROXY_TOKEN in claw_text and "sk-" not in claw_text
        assert "openai-completions" in claw_text
        assert '"mode": "local"' in claw_text
    finally:
        for env in homes:
            _release_homes(env)


def test_opencode_config_only_includes_declared_capabilities() -> None:
    """An agent's skills/MCPs are the only ones written; undeclared ones are dropped."""
    import json

    from crewchat.core.providers import Provider
    from crewchat.executors.acp_body import _inline_config

    prov = Provider(id="agnes", name="Agnes", base_url="https://x/v1", model="m", api_key="k")
    # A made-up mcp name is not in the shared registry, so it must not appear.
    cfg = json.loads(_inline_config(prov, ["definitely-not-a-skill"], ["nope-not-an-mcp"]))
    assert "skills" not in cfg
    assert "mcp" not in cfg


def test_acp_registers_the_cli_bodies() -> None:
    """All CLIs are ACP bodies; `llm` stays because it is direct, not a CLI."""
    from crewchat.executors import list_executors

    by_id = {e["id"]: e for e in list_executors()}
    for cli in ("opencode", "openclaw", "claude", "codex"):
        assert by_id[cli]["kind"] == "cli"
    assert by_id["llm"]["kind"] == "api"


def test_acp_model_option_lookup_handles_flat_and_grouped() -> None:
    """Selecting the agent's brain must find the value flat or nested in a group."""
    from types import SimpleNamespace

    from crewchat.executors.acp_body import _select_values

    flat = SimpleNamespace(options=[SimpleNamespace(value="a/b"), SimpleNamespace(value="agnes/agnes-2.5-flash")])
    assert "agnes/agnes-2.5-flash" in _select_values(flat)

    grouped = SimpleNamespace(options=[SimpleNamespace(options=[SimpleNamespace(value="x/y")])])
    assert "x/y" in _select_values(grouped)


def test_every_scanned_tool_is_an_acp_body() -> None:
    """Every CLI the engines page scans must have a body, or the two disagree.

    cc-switch scans a fixed tool set; the composer's picker reads the body
    registry. If a scanned tool has no body, the engines page shows it but the
    picker does not — exactly the "engines page has many, picker has three" gap.
    """
    from crewchat.executors import list_executors
    from crewchat.executors.acp_body import ACP_CLIS

    scanned = {"claude", "codex", "gemini", "grok", "opencode", "openclaw", "hermes", "pi", "mcode"}
    assert scanned <= set(ACP_CLIS)
    assert scanned <= {e["id"] for e in list_executors()}


def test_manager_restore_writes_once(tmp_path, monkeypatch) -> None:
    from crewchat.core import config
    from crewchat.core.agent_presets import MANAGER_ID, manager_reason, restore_manager
    from crewchat.core.models import AgentSpec

    monkeypatch.setenv("CREWCHAT_DATA_DIR", str(tmp_path / "data"))
    config.get_settings.cache_clear()
    settings = config.get_settings()
    first = restore_manager(settings)
    assert first["created"] is True
    assert first["reason"] == "unconfigured"
    path = settings.agents_home / f"{MANAGER_ID}.jsonc"
    path.write_text(path.read_text(encoding="utf-8").replace('"brain": ""', '"brain": "p/m"'), encoding="utf-8")
    second = restore_manager(settings)
    assert second["created"] is False
    assert second["ready"] is True
    assert manager_reason(AgentSpec(id=MANAGER_ID, enabled=False, brain="p/m")) == "disabled"
    config.get_settings.cache_clear()
