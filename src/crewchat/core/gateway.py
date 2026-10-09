"""模型网关：**端口 + 适配器**。

业务只依赖这里的 `ModelGateway` 端口；底层实现可替换（cc-switch / LiteLLM / 其它），
换实现不改上层。选哪个由 `Settings.gateway_kind` 决定，适配器在各自模块里。

无论换谁，都遵守这些不变式（见 `AGENTS/gateway.md`）：

- **一份通用 provider 是唯一真相源**（`.data/providers.jsonc`，`core/providers.py`）；
- 网关只负责把某个 CLI 的流量**反代 + 协议转换**到该 provider；
- 网关自身的进程/数据是**派生物**，可重建，绝不作为我们的真相源。
"""

from __future__ import annotations

from typing import Any, Protocol

from .config import Settings, get_settings
from .providers import Provider


class ModelGateway(Protocol):
    """本地模型网关。所有方法都应是"当前实现无关"的语义。"""

    control: str
    """本地控制口基地址（如 `http://127.0.0.1:15722`），供复用核心能力的适配器调用。"""

    def status(self) -> dict[str, Any]:
        """供界面展示：`{available, alive, running, channels}`。"""
        ...

    def ensure_running(self) -> bool:
        """确保网关可用（按需拉起）。"""
        ...

    def stop(self) -> None:
        """停止网关。"""
        ...

    def activate(self, app: str, provider: Provider) -> str | None:
        """让 CLI `app` 经网关使用 `provider`；返回该 CLI 应使用的模型 id。

        支持「逐请求路由」的实现（如 cc-switch 的 Stack）返回那个模型 id，调用方用它
        替换 provider.model，从而**同一 CLI 可同时用多个 provider/模型**；不支持时返回
        `None`，调用方沿用 provider.model（该 CLI 只有一条当前通道）。
        """
        ...

    def deactivate(self, app: str) -> None:
        """让 CLI `app` 回到直连。"""
        ...

    def usage(self, days: int = 30) -> list[dict[str, Any]]:
        """按天的用量记录（字段由实现决定，界面只做展示）。"""
        ...


def get_gateway(settings: Settings | None = None) -> ModelGateway:
    settings = settings or get_settings()
    kind = settings.gateway_kind
    if kind == "cc-switch":
        from .gateway_ccswitch import CcSwitchGateway

        return CcSwitchGateway(settings)
    raise ValueError(f"未知的网关实现: {kind!r}（gateway_kind）")
