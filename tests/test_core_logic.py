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


def test_provider_key_count_handles_multiple_secrets() -> None:
    assert _provider(api_key="a\nb, c").key_count() == 3
    assert _provider(api_key="").key_count() == 0


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
    assert set(settings_config["models"]) == {"a", "b"}


def test_build_settings_config_rejects_unknown_app() -> None:
    with pytest.raises(ValueError):
        _build_settings_config("cursor", _provider())
