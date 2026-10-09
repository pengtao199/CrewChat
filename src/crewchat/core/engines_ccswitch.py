"""cc-switch 适配器：复用它的工具层做引擎探测/安装/更新。

cc-switch 的 `commands::misc` 里有成熟的实现（`probe_tool_installations` /
`run_tool_lifecycle_action`），我们把它们经 `ccsw-proxy` 的 `/tools` 控制口暴露出来，
不重造枚举/冲突/锚定逻辑。
"""

from __future__ import annotations

from typing import Any

from .config import Settings, get_settings
from .gateway_ccswitch import CcSwitchGateway


class CcSwitchEngineTools:
    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or get_settings()
        self._gateway = CcSwitchGateway(self.settings)

    def _ready(self) -> str:
        if not self._gateway.ensure_running():
            raise RuntimeError("网关核心未就绪，无法探测引擎")
        return self._gateway.control

    def scan(self) -> list[dict[str, Any]]:
        import httpx

        resp = httpx.get(f"{self._ready()}/tools", timeout=30)
        resp.raise_for_status()
        return list(resp.json())

    def versions(self) -> list[dict[str, Any]]:
        import httpx

        # 会拉 npm/github 的最新版本，网络请求，给足超时。
        resp = httpx.get(f"{self._ready()}/tools/versions", timeout=60)
        resp.raise_for_status()
        return list(resp.json())

    def install(self, tools: list[str]) -> None:
        self._lifecycle(tools, "install")

    def update(self, tools: list[str]) -> None:
        self._lifecycle(tools, "update")

    def _lifecycle(self, tools: list[str], action: str) -> None:
        import httpx

        # 安装/更新会真正执行 npm/brew/官方脚本，可能较久。
        resp = httpx.post(
            f"{self._ready()}/tools/lifecycle",
            json={"tools": tools, "action": action},
            timeout=900,
        )
        resp.raise_for_status()
