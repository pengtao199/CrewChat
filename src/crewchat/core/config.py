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
    codex_provider: str = "gateway"
    codex_model: str = ""
    codex_sandbox: str = "workspace-write"
    codex_approval: str = "on-failure"
    codex_network: bool = True

    executor_timeout: float = 1800.0
    max_turns: int = 12

    language: str = "zh-CN"

    data_dir: Path = _default_data_dir()

    # 模型网关：端口在 core/gateway.py，实现由 `gateway_kind` 选。见 AGENTS/gateway.md。
    # 核心复用适配器的实现，但数据归我们：网关用 `gateway_dir` 作为自己的 home，
    # 不碰用户本机的 `~/.cc-switch/`。
    gateway_kind: str = "cc-switch"
    # 核心源码已固化进本仓库（gateway/cc-switch/），从仓库内构建，不依赖外部克隆。
    gateway_binary: Path = (
        Path(__file__).resolve().parents[3] / "gateway" / "cc-switch" / "src-tauri" / "target" / "release" / "ccsw-proxy"
    )
    gateway_proxy_port: int = 15721
    gateway_control_port: int = 15722
    gateway_home: Path | None = None  # 覆盖；默认 `.data/gateway`

    @property
    def gateway_dir(self) -> Path:
        return self.gateway_home or (self.data_dir / "gateway")

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
        self.gateway_dir.mkdir(parents=True, exist_ok=True)


@lru_cache
def get_settings() -> Settings:
    return Settings()