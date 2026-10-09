"""引擎管理：探测 CLI 的安装/版本/更新（**端口 + 适配器**）。

与网关同构：业务只依赖这里的 `EngineTools` 端口，实现可替换。当前实现复用 cc-switch
的工具层（它枚举每个工具的**所有**安装、判冲突、生成升级命令）。选哪个由
`Settings.gateway_kind` 决定——引擎工具与网关由同一个 headless 核心提供。
"""

from __future__ import annotations

from typing import Any, Protocol

from .config import Settings, get_settings


class EngineTools(Protocol):
    """本机执行引擎的探测与生命周期管理。"""

    def scan(self) -> list[dict[str, Any]]:
        """每个受支持 CLI 的安装分布：path/version/runnable/source/冲突/升级命令。"""
        ...

    def install(self, tools: list[str]) -> None:
        """安装指定的 CLI（**会真正执行系统命令**）。"""
        ...

    def update(self, tools: list[str]) -> None:
        """更新指定的 CLI（**会真正执行系统命令**）。"""
        ...


def get_engine_tools(settings: Settings | None = None) -> EngineTools:
    settings = settings or get_settings()
    kind = settings.gateway_kind
    if kind == "cc-switch":
        from .engines_ccswitch import CcSwitchEngineTools

        return CcSwitchEngineTools(settings)
    raise ValueError(f"未知的引擎工具实现: {kind!r}（gateway_kind）")
