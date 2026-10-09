"""用量记账：统一 token 形状、各身体解析、落库聚合。"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

from crewchat.api.store import Store
from crewchat.core import config
from crewchat.core.models import AgentSpec
from crewchat.executors import base as exec_base
from crewchat.executors.base import ExecResult, TokenUsage, _int
from crewchat.executors.claude_body import _usage_from_result
from crewchat.executors.opencode_body import _accumulate_usage, _parse
from crewchat.orchestration import session as session_mod
from crewchat.orchestration.session import Session

# ---- 统一形状 -----------------------------------------------------------------


def test_int_coercion_is_safe() -> None:
    assert _int(5) == 5
    assert _int("7") == 7
    assert _int(3.9) == 3
    assert _int(None) == 0
    assert _int("x") == 0
    assert _int(-4) == 0


def test_token_usage_emptiness_and_dict() -> None:
    assert TokenUsage().is_empty() is True
    u = TokenUsage(input_tokens=1)
    assert u.is_empty() is False
    assert u.to_dict() == {
        "input_tokens": 1,
        "output_tokens": 0,
        "cache_read_tokens": 0,
        "cache_creation_tokens": 0,
        "reasoning_tokens": 0,
    }


# ---- opencode 解析 ------------------------------------------------------------


def test_opencode_parses_step_finish() -> None:
    line = (
        '{"type":"step_finish","part":{"type":"step-finish","id":"p1","cost":0.5,'
        '"tokens":{"input":10,"output":2,"reasoning":1,"cache":{"read":3,"write":4}}}}'
    )
    parsed = _parse(line)
    assert parsed is not None
    kind, payload, _ = parsed
    assert kind == "usage"
    assert payload["cost"] == 0.5
    assert payload["tokens"]["input"] == 10


def test_opencode_accumulates_usage() -> None:
    usage = TokenUsage()
    _accumulate_usage(usage, {"tokens": {"input": 10, "output": 2, "reasoning": 1, "cache": {"read": 3, "write": 4}}})
    _accumulate_usage(usage, {"tokens": {"input": 5, "output": 1}})
    assert usage.to_dict() == {
        "input_tokens": 15,
        "output_tokens": 3,
        "cache_read_tokens": 3,
        "cache_creation_tokens": 4,
        "reasoning_tokens": 1,
    }


# ---- claude 解析 --------------------------------------------------------------


def test_claude_usage_prefers_model_usage() -> None:
    result = SimpleNamespace(
        model_usage={
            "claude-sonnet-4": {
                "inputTokens": 10,
                "outputTokens": 2,
                "cacheReadInputTokens": 3,
                "cacheCreationInputTokens": 4,
            }
        },
        usage=None,
    )
    usage = _usage_from_result(result)
    assert usage.to_dict() == {
        "input_tokens": 10,
        "output_tokens": 2,
        "cache_read_tokens": 3,
        "cache_creation_tokens": 4,
        "reasoning_tokens": 0,
    }


def test_claude_usage_falls_back_to_usage_dict() -> None:
    result = SimpleNamespace(
        model_usage={},
        usage={
            "input_tokens": 7,
            "output_tokens": 1,
            "cache_read_input_tokens": 2,
            "cache_creation_input_tokens": 3,
        },
    )
    usage = _usage_from_result(result)
    assert (usage.input_tokens, usage.output_tokens) == (7, 1)
    assert (usage.cache_read_tokens, usage.cache_creation_tokens) == (2, 3)


# ---- 落库与聚合 ---------------------------------------------------------------


def _fresh_store(tmp_path, monkeypatch) -> Store:  # type: ignore[no-untyped-def]
    monkeypatch.setenv("CREWCHAT_DATA_DIR", str(tmp_path))
    config.get_settings.cache_clear()
    return Store()


def test_usage_summary_roundtrip(tmp_path, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    store = _fresh_store(tmp_path, monkeypatch)
    try:
        store.record_usage(
            conversation_id="c1",
            turn_id="t1",
            agent_id="a1",
            body="codex",
            model="gpt-5",
            usage={"input_tokens": 100, "output_tokens": 20, "reasoning_tokens": 8},
            cost_usd=None,
            duration_ms=1200,
        )
        store.record_usage(
            conversation_id="c1",
            turn_id="t2",
            agent_id="a2",
            body="claude",
            model="claude-sonnet-4",
            usage={"input_tokens": 50, "output_tokens": 10},
            cost_usd=0.0123,
            duration_ms=800,
        )

        summary = store.usage_summary(30)
        assert summary["totals"]["requests"] == 2
        assert summary["totals"]["input_tokens"] == 150
        assert summary["totals"]["output_tokens"] == 30
        assert summary["totals"]["cost_usd"] == 0.0123

        bodies = {row["body"]: row for row in summary["by_body"]}
        assert set(bodies) == {"codex", "claude"}
        assert bodies["claude"]["cost_usd"] == 0.0123
        assert bodies["codex"]["reasoning_tokens"] == 8

        assert summary["by_model"][0]["requests"] == 1
        assert len(summary["by_day"]) == 1
    finally:
        config.get_settings.cache_clear()


def test_usage_summary_empty(tmp_path, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    store = _fresh_store(tmp_path, monkeypatch)
    try:
        summary = store.usage_summary(7)
        assert summary["totals"]["requests"] == 0
        assert summary["by_body"] == []
    finally:
        config.get_settings.cache_clear()


# ---- 编排事件契约：stream_ask 结束时回吐 done + usage ---------------------------


@exec_base.register
class _FakeBody(exec_base.Executor):
    id = "fake-usage-body"
    label = "fake"
    has_builtin_brain = True

    async def run(self, req, provider):  # type: ignore[no-untyped-def]
        return ExecResult(
            text="hi",
            cost_usd=0.5,
            usage=TokenUsage(input_tokens=3, output_tokens=1),
            meta={"body": self.id, "model": "fake-model"},
        )


def test_stream_ask_emits_done_with_usage(tmp_path, monkeypatch) -> None:  # type: ignore[no-untyped-def]
    spec = AgentSpec(id="a", name="A", body="fake-usage-body")
    monkeypatch.setattr(session_mod, "load_specs", lambda _path: [spec])
    monkeypatch.setattr(session_mod, "agents_dir_for", lambda _settings: tmp_path)

    # 未绑定工作区 → 会话工作区由调用方传入（这里直接用 tmp_path）。
    session = Session(None, tmp_path)

    async def collect() -> list:  # type: ignore[type-arg]
        return [event async for event in session.stream_ask(spec, "hello", "conv:x")]

    events = asyncio.run(collect())
    done = [event for event in events if event.kind == "done"]
    assert done, "stream_ask must end with a done event"
    meta = done[0].meta
    assert meta["body"] == "fake-usage-body"
    assert meta["usage"] == {
        "input_tokens": 3,
        "output_tokens": 1,
        "cache_read_tokens": 0,
        "cache_creation_tokens": 0,
        "reasoning_tokens": 0,
    }
    assert meta["cost_usd"] == 0.5
