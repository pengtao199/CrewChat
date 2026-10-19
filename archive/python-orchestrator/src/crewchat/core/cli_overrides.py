from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .config import Settings

_OVERRIDABLE = (
    "claude_model",
    "claude_permission_mode",
    "codex_model",
    "codex_sandbox",
    "codex_approval",
    "codex_network",
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
def current_settings() -> Settings:
    """Settings with UI overrides layered on top of env/.env."""
    base = Settings()
    overrides = load_overrides(base)
    if overrides:
        base = base.model_copy(update=overrides)
    return base
