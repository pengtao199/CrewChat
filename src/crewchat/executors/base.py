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
class ExecResult:
    text: str = ""
    exit_code: int = 0
    duration_ms: int = 0
    session: str | None = None
    cost_usd: float | None = None
    num_turns: int | None = None
    error: str | None = None
    meta: dict[str, Any] = field(default_factory=dict)


class Executor(ABC):
    id: ClassVar[str]
    label: ClassVar[str]
    has_builtin_brain: ClassVar[bool]

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
            "available": cls.available(),
        }
        for cls in sorted(_REGISTRY.values(), key=lambda c: c.id)
    ]