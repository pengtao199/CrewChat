from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path
from typing import Any

from .config import Settings

_OVERRIDABLE = (
    "llm_base_url",
    "llm_model",
    "llm_api_key",
    "llm_temperature",
    "llm_max_tokens",
    "claude_model",
    "claude_permission_mode",
    "codex_model",
    "codex_sandbox",
    "codex_approval",
    "codex_network",
    "local_password",
)


def overrides_path(settings: Settings) -> Path:
    return settings.data_dir / "overrides.json"


def load_overrides(settings: Settings) -> dict[str, Any]:
    path = overrides_path(settings)
    if not path.exists():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return {k: v for k, v in data.items() if k in _OVERRIDABLE}


def save_overrides(settings: Settings, values: dict[str, Any]) -> dict[str, Any]:
    """Merge `values` into the override file atomically. Blank API key is ignored."""
    current = load_overrides(settings)
    for key in _OVERRIDABLE:
        if key not in values:
            continue
        value = values[key]
        if key == "llm_api_key":
            if not value:
                continue
            current[key] = value
            continue
        current[key] = value

    path = overrides_path(settings)
    path.parent.mkdir(parents=True, exist_ok=True)
    handle, tmp = tempfile.mkstemp(dir=str(path.parent), suffix=".tmp")
    try:
        with os.fdopen(handle, "w", encoding="utf-8") as fh:
            json.dump(current, fh, ensure_ascii=False, indent=2)
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise
    return current


def clear_overrides(settings: Settings) -> None:
    overrides_path(settings).unlink(missing_ok=True)


def current_settings() -> Settings:
    """Settings with UI overrides layered on top of env/.env."""
    base = Settings()
    overrides = load_overrides(base)
    if overrides:
        base = base.model_copy(update=overrides)
    return base


def mask_secret(value: str) -> str:
    if not value:
        return ""
    if len(value) <= 10:
        return "*" * len(value)
    return f"{value[:5]}{'*' * 6}{value[-4:]}"


def has_key(settings: Settings) -> bool:
    return bool(settings.llm_api_key.get_secret_value())