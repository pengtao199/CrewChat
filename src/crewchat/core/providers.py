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
        models: list[str] | None = None,
        api_format: str = "openai_chat",
        temperature: float | None = None,
        max_tokens: int | None = None,
        extra_headers: dict[str, str] | None = None,
    ) -> None:
        self.id = id
        self.name = name
        self.base_url = base_url
        self.model = model
        self.api_key = api_key
        self.models = models or []
        self.api_format = api_format
        self.temperature = temperature
        self.max_tokens = max_tokens
        self.extra_headers = extra_headers or {}

    def env_key(self) -> str:
        """opencode 的 `{env:XXX}` 约定里用到的变量名。"""
        return f"{re.sub(r'[^0-9A-Za-z]', '_', self.id).upper()}_API_KEY"

    def has_key(self) -> bool:
        return bool(self.api_key)

    def as_dict(self, reveal: bool = False) -> dict[str, Any]:
        return {
            "id": self.id,
            "name": self.name,
            "base_url": self.base_url,
            "model": self.model,
            "models": self.models,
            "api_format": self.api_format,
            "temperature": self.temperature,
            "max_tokens": self.max_tokens,
            "api_key": self.api_key if reveal else ("*" * 0 + ("已设置" if self.has_key() else "")),
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
            extra_headers=raw.get("extra_headers") or {},
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
