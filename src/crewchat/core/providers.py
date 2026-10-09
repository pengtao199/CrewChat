from __future__ import annotations

import json
import os
import re
import tempfile
from pathlib import Path
from typing import Any

from crewai.project.json_loader import load_jsonc_file

from .config import Settings

_HOME_AGENTS = Path.home() / ".agents"

# 模型标签的合法取值。固定枚举而非自由字符串，这样页面才能按标签筛选/统计。
# 键是存储用的稳定 id，值是 i18n key（由前端负责翻译）。
MODEL_TAGS: tuple[str, ...] = (
    "text",
    "vision",
    "reasoning",
    "tool_use",
    "long_context",
    "fast",
    "free",
    "embedding",
)


class Model:
    """供应商下的一个模型。

    历史上 `Provider.models` 是 `list[str]`（裸模型 id）。现在升级为对象，
    承载标签 / 能力 / 上下文窗口 / 启停等，**同时向后兼容**旧的字符串写法
    （见 `Model.from_raw`），因此旧的 `providers.jsonc` 无需迁移即可读。
    """

    def __init__(
        self,
        id: str,
        display_name: str = "",
        tags: list[str] | None = None,
        context_limit: int | None = None,
        enabled: bool = True,
        protocol: str | None = None,
        capabilities: dict[str, Any] | None = None,
        price: dict[str, Any] | None = None,
    ) -> None:
        self.id = id
        self.display_name = display_name
        self.tags = [t for t in (tags or []) if t in MODEL_TAGS]
        self.context_limit = context_limit
        self.enabled = enabled
        self.protocol = protocol
        self.capabilities = capabilities or {}
        self.price = price or {}

    @property
    def label(self) -> str:
        """展示用名称：优先自定义名，否则退回模型 id。"""
        return self.display_name or self.id

    def has_tag(self, tag: str) -> bool:
        return tag in self.tags

    def as_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {
            "id": self.id,
            "enabled": self.enabled,
            "tags": self.tags,
        }
        if self.display_name:
            data["display_name"] = self.display_name
        if self.context_limit is not None:
            data["context_limit"] = self.context_limit
        if self.protocol:
            data["protocol"] = self.protocol
        if self.capabilities:
            data["capabilities"] = self.capabilities
        if self.price:
            data["price"] = self.price
        return data

    @classmethod
    def from_raw(cls, raw: Any) -> Model | None:
        """兼容两种写法：裸字符串 id，或模型对象。"""
        if isinstance(raw, str):
            text = raw.strip()
            return cls(id=text) if text else None
        if isinstance(raw, dict):
            model_id = str(raw.get("id") or "").strip()
            if not model_id:
                return None
            context_limit = raw.get("context_limit")
            return cls(
                id=model_id,
                display_name=str(raw.get("display_name") or ""),
                tags=list(raw.get("tags") or []),
                context_limit=int(context_limit) if isinstance(context_limit, (int, float)) else None,
                enabled=bool(raw.get("enabled", True)),
                protocol=str(raw["protocol"]) if raw.get("protocol") else None,
                capabilities=dict(raw.get("capabilities") or {}),
                price=dict(raw.get("price") or {}),
            )
        return None


class Provider:
    """一个可复用的**通用**模型端点（大脑池条目）。Agent 通过 id 引用，不拥有它。

    这是唯一真相源，存 `.data/providers.jsonc`。网关（cc-switch 核心）里的按 CLI
    分的那几份 provider，是由这里**投影**出来的派生物，见 `core/gateway.py`。
    """

    def __init__(
        self,
        id: str,
        name: str,
        base_url: str,
        model: str,
        api_key: str = "",
        models: list[Model | str] | None = None,
        api_format: str = "openai_chat",
        temperature: float | None = None,
        max_tokens: int | None = None,
        extra_headers: dict[str, str] | None = None,
        enabled: bool = True,
        is_full_url: bool = False,
        models_url_override: str = "",
    ) -> None:
        self.id = id
        self.name = name
        self.base_url = base_url
        self.model = model
        self.api_key = api_key
        self.models = [m for m in (Model.from_raw(raw) for raw in (models or [])) if m]
        self.api_format = api_format
        self.temperature = temperature
        self.max_tokens = max_tokens
        self.extra_headers = extra_headers or {}
        self.enabled = enabled
        self.is_full_url = is_full_url
        self.models_url_override = models_url_override

    def env_key(self) -> str:
        """opencode 的 `{env:XXX}` 约定里用到的变量名。"""
        return f"{re.sub(r'[^0-9A-Za-z]', '_', self.id).upper()}_API_KEY"

    def has_key(self) -> bool:
        return bool(self.api_key)

    def key_count(self) -> int:
        """有效 API Key 个数（多 Key 以换行/逗号分隔）。"""
        return len([k for k in re.split(r"[,\n]", self.api_key or "") if k.strip()])

    def get_model(self, model_id: str) -> Model | None:
        return next((m for m in self.models if m.id == model_id), None)

    def model_ids(self) -> list[str]:
        return [m.id for m in self.models]

    def as_dict(self, reveal: bool = False) -> dict[str, Any]:
        return {
            "id": self.id,
            "name": self.name,
            "base_url": self.base_url,
            "model": self.model,
            "models": [m.as_dict() for m in self.models],
            "api_format": self.api_format,
            "temperature": self.temperature,
            "max_tokens": self.max_tokens,
            "extra_headers": self.extra_headers,
            "enabled": self.enabled,
            "is_full_url": self.is_full_url,
            "models_url_override": self.models_url_override,
            "api_key": self.api_key if reveal else ("已设置" if self.has_key() else ""),
        }

    @classmethod
    def from_dict(cls, raw: dict[str, Any]) -> Provider:
        return cls(
            id=str(raw["id"]),
            name=str(raw.get("name") or raw["id"]),
            base_url=str(raw.get("base_url") or "https://api.openai.com/v1"),
            model=str(raw.get("model") or ""),
            api_key=str(raw.get("api_key") or ""),
            models=list(raw.get("models") or []),
            api_format=str(raw.get("api_format") or "openai_chat"),
            temperature=raw.get("temperature"),
            max_tokens=raw.get("max_tokens"),
            extra_headers=dict(raw.get("extra_headers") or {}),
            enabled=bool(raw.get("enabled", True)),
            is_full_url=bool(raw.get("is_full_url", False)),
            models_url_override=str(raw.get("models_url_override") or ""),
        )


def providers_file(settings: Settings) -> Path:
    return settings.data_dir / "providers.jsonc"


def load_providers(settings: Settings) -> list[Provider]:
    path = providers_file(settings)
    if not path.exists():
        return []
    raw = load_jsonc_file(str(path))
    items = raw.get("providers", raw if isinstance(raw, list) else [])
    out: list[Provider] = []
    for item in items:
        if isinstance(item, dict) and item.get("id"):
            out.append(Provider.from_dict(item))
    return out


def get_provider(settings: Settings, provider_id: str | None) -> Provider | None:
    providers = load_providers(settings)
    if provider_id:
        for provider in providers:
            if provider.id == provider_id:
                return provider
    return next((p for p in providers if p.has_key()), None)


def upsert_model(provider: Provider, model: Model) -> None:
    """新增或按 id 覆盖供应商下的模型。"""
    for index, existing in enumerate(provider.models):
        if existing.id == model.id:
            provider.models[index] = model
            return
    provider.models.append(model)


def remove_model(provider: Provider, model_id: str) -> bool:
    """移除模型；返回是否真的删掉了。"""
    before = len(provider.models)
    provider.models = [m for m in provider.models if m.id != model_id]
    return len(provider.models) != before


def save_providers(settings: Settings, providers: list[Provider]) -> None:
    path = providers_file(settings)
    path.parent.mkdir(parents=True, exist_ok=True)
    handle, tmp = tempfile.mkstemp(dir=str(path.parent), suffix=".tmp")
    try:
        with os.fdopen(handle, "w", encoding="utf-8") as fh:
            json.dump({"providers": [p.as_dict(reveal=True) for p in providers]}, fh, ensure_ascii=False, indent=2)
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise


def shared_mcp_servers() -> dict[str, Any]:
    """复用 ~/.agents/mcp.json（codex/claude/opencode 共用的约定）。"""
    path = _HOME_AGENTS / "mcp.json"
    if not path.exists():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8")).get("mcpServers", {}) or {}
    except (OSError, ValueError):
        return {}


def available_skills() -> list[dict[str, str]]:
    """扫描 ~/.agents/skills 与 ~/.agents/plugins/*/skills。"""
    found: dict[str, str] = {}
    for root in (_HOME_AGENTS / "skills",):
        if not root.is_dir():
            continue
        for entry in sorted(root.iterdir()):
            if entry.is_dir() and (entry / "SKILL.md").is_file():
                found[entry.name] = str(entry / "SKILL.md")
    plugins = _HOME_AGENTS / "plugins"
    if plugins.is_dir():
        for plugin in sorted(plugins.iterdir()):
            skills = plugin / "skills"
            if skills.is_dir():
                for entry in sorted(skills.iterdir()):
                    if entry.is_dir() and (entry / "SKILL.md").is_file():
                        found.setdefault(entry.name, str(entry / "SKILL.md"))
    return [{"name": k, "path": v} for k, v in sorted(found.items())]


def available_plugins() -> list[str]:
    plugins = _HOME_AGENTS / "plugins"
    if not plugins.is_dir():
        return []
    return sorted(p.name for p in plugins.iterdir() if p.is_dir())


def resolve_brain(settings: Settings, spec: Any) -> Provider | None:
    """Agent 的大脑：CLI 身体自带大脑，只有显式写成 `provider/model` 才覆盖。"""
    if spec.is_cli_backed and "/" not in spec.brain:
        return None
    return get_provider(settings, spec.brain or None)


def apply_brain_env(settings: Settings, spec: Any) -> None:
    """把大脑端点暴露给子进程（部分 CLI 通过环境变量读取上游）。"""
    provider = resolve_brain(settings, spec)
    if provider and provider.api_key:
        os.environ.setdefault("OPENAI_API_KEY", provider.api_key)
        os.environ.setdefault("OPENAI_BASE_URL", provider.base_url)
