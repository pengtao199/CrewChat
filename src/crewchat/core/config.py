from __future__ import annotations

from functools import lru_cache
from pathlib import Path

from pydantic_settings import BaseSettings, SettingsConfigDict


def _default_data_dir() -> Path:
    return Path.home() / "Library" / "Application Support" / "CrewChat"


class Settings(BaseSettings):
    model_config = SettingsConfigDict(
        env_prefix="CREWCHAT_",
        env_file=".env",
        env_file_encoding="utf-8",
        extra="ignore",
    )

    host: str = "127.0.0.1"
    port: int = 8000

    claude_model: str = ""
    claude_permission_mode: str = "bypassPermissions"
    claude_max_turns: int = 24

    opencode_model: str = ""
    codex_model: str = ""
    codex_sandbox: str = "workspace-write"
    codex_approval: str = "on-failure"
    codex_network: bool = True

    executor_timeout: float = 1800.0
    max_turns: int = 12

    language: str = "zh-CN"

    data_dir: Path = _default_data_dir()

    @property
    def db_path(self) -> Path:
        return self.data_dir / "crewchat.db"

    @property
    def agents_home(self) -> Path:
        """全局 agent 目录。agent 是「公司的人」，不归任何工作区。"""
        return self.data_dir / "agents"

    @property
    def workspace(self) -> Path:
        return self.data_dir / "workspace"

    def ensure_dirs(self) -> None:
        self.data_dir.mkdir(parents=True, exist_ok=True)
        self.workspace.mkdir(parents=True, exist_ok=True)
        self.agents_home.mkdir(parents=True, exist_ok=True)


@lru_cache
def get_settings() -> Settings:
    return Settings()