from __future__ import annotations

from abc import ABC, abstractmethod
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, ClassVar, Literal

from ..core.providers import Provider

EventKind = Literal["delta", "thinking", "tool", "notice", "done", "error"]


@dataclass(slots=True)
class ExecEvent:
    kind: EventKind
    text: str = ""
    tool: str = ""
    status: str = ""
    # 厂商自己的调用 id：界面按它合并同一工具的状态更新，缺了就丢弃这一帧
    call_id: str = ""
    meta: dict[str, Any] = field(default_factory=dict)


@dataclass(slots=True)
class ExecRequest:
    prompt: str
    cwd: Path
    timeout: float = 1800.0
    session: str | None = None
    on_event: Callable[[ExecEvent], None] | None = None

    def emit(self, kind: EventKind, **kw: Any) -> None:
        if self.on_event is None:
            return
        self.on_event(ExecEvent(kind=kind, **kw))


@dataclass(slots=True)
class TokenUsage:
    """一次身体运行的 token 消耗，作为跨身体的统一形状。

    各身体来源不同（claude 的 model_usage、codex 的 TurnCompletedEvent.usage、
    opencode 的 step-finish、llm 的 OpenAI usage），这里归一到同一组计数字段。
    只统计**经我们 app 发起的**运行——不读网关日志、不掺用户裸跑 CLI 的消耗。
    """

    input_tokens: int = 0
    output_tokens: int = 0
    cache_read_tokens: int = 0
    cache_creation_tokens: int = 0
    reasoning_tokens: int = 0

    def is_empty(self) -> bool:
        return not any(
            (
                self.input_tokens,
                self.output_tokens,
                self.cache_read_tokens,
                self.cache_creation_tokens,
                self.reasoning_tokens,
            )
        )

    def to_dict(self) -> dict[str, int]:
        return {
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cache_read_tokens": self.cache_read_tokens,
            "cache_creation_tokens": self.cache_creation_tokens,
            "reasoning_tokens": self.reasoning_tokens,
        }


def _int(value: Any) -> int:
    """把 SDK 给的各种数字形状（int / float / str / None）安全转成非负 int。"""
    try:
        return max(0, int(value))
    except (TypeError, ValueError):
        return 0


@dataclass(slots=True)
class ExecResult:
    text: str = ""
    exit_code: int = 0
    duration_ms: int = 0
    session: str | None = None
    cost_usd: float | None = None
    num_turns: int | None = None
    usage: TokenUsage | None = None
    error: str | None = None
    meta: dict[str, Any] = field(default_factory=dict)


EngineKind = Literal["cli", "api", "builtin"]


class Executor(ABC):
    id: ClassVar[str]
    label: ClassVar[str]
    has_builtin_brain: ClassVar[bool]
    # 引擎类别：本地 CLI（用户装）/ 远程 API（配模型即可）/ 应用内置（随包）
    kind: ClassVar[EngineKind] = "cli"

    @classmethod
    def available(cls) -> bool:
        return True

    @abstractmethod
    async def run(self, req: ExecRequest, provider: Provider | None) -> ExecResult: ...

    async def close(self) -> None:
        return None


_REGISTRY: dict[str, type[Executor]] = {}


def register(cls: type[Executor]) -> type[Executor]:
    _REGISTRY[cls.id] = cls
    return cls


def get_executor(body: str) -> Executor:
    if body not in _REGISTRY:
        raise KeyError(f"unknown body {body!r}; available: {', '.join(sorted(_REGISTRY))}")
    return _REGISTRY[body]()


def list_executors() -> list[dict[str, Any]]:
    return [
        {
            "id": cls.id,
            "label": cls.label,
            "has_builtin_brain": cls.has_builtin_brain,
            "kind": cls.kind,
            "available": cls.available(),
        }
        for cls in sorted(_REGISTRY.values(), key=lambda c: c.id)
    ]