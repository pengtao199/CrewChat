from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from typing import Any

from ..core.config import get_settings
from ..executors import ExecRequest, get_executor
from ..executors.base import ExecEvent
from ..core.models import AgentSpec, load_specs
from ..core.projects import Project, get_project
from ..core.providers import Provider, apply_brain_env, resolve_brain

Emitter = Any


class RunOutcome:
    def __init__(self) -> None:
        self.text: str = ""
        self.error: str | None = None
        self.meta: dict[str, Any] = {}
class Session:
    """一条会话：绑定了项目、在场 agent、以及每个 agent 的记忆。"""

    def __init__(self, project: Project) -> None:
        self.project = project
        self.settings = get_settings()
        self.specs: dict[str, AgentSpec] = {}
        self.sessions: dict[str, str] = {}
        self.last_meta: dict[str, dict[str, Any]] = {}
        self.reload()

    @property
    def workspace(self) -> Path:
        return self.project.workspace()

    def reload(self) -> None:
        self.specs = {s.id: s for s in load_specs(self.project.agents_path())}
    def provider_for(self, spec: AgentSpec) -> Provider | None:
        return resolve_brain(self.settings, spec)
    async def stream_ask(
        self,
        spec: AgentSpec,
        prompt: str,
        session_key: str,
    ) -> AsyncIterator[ExecEvent]:
        """跑一次 agent 回合，边跑边产出事件。"""
        executor = get_executor(spec.body)
        apply_brain_env(self.settings, spec)
        memory_key = self.sessions.setdefault(spec.id, f"{session_key}:{spec.id}")
        queue: asyncio.Queue[ExecEvent | None] = asyncio.Queue()

        def on_event(event: ExecEvent) -> None:
            queue.put_nowait(event)

        async def drive() -> RunOutcome:
            try:
                result = await executor.run(
                    ExecRequest(
                        prompt=prompt,
                        cwd=spec.resolve_cwd(self.workspace),
                        timeout=self.settings.executor_timeout,
                        session=memory_key,
                        on_event=on_event,
                    ),
                    self.provider_for(spec),
                )
            except Exception as exc:
                outcome = RunOutcome()
                outcome.error = f"{type(exc).__name__}: {exc}"
                queue.put_nowait(ExecEvent(kind="error", text=outcome.error))
                return outcome
            outcome = RunOutcome()
            outcome.text = result.text
            outcome.error = result.error
            outcome.meta = {
                **result.meta,
                "duration_ms": result.duration_ms,
                "cost_usd": result.cost_usd,
                "num_turns": result.num_turns,
                "usage": result.usage.to_dict() if result.usage else None,
            }
            if result.error:
                queue.put_nowait(ExecEvent(kind="error", text=result.error))
            return outcome

        async def runner() -> None:
            nonlocal outcome_box
            outcome_box = await drive()
            queue.put_nowait(None)

        outcome_box: RunOutcome | None = None
        task = asyncio.create_task(runner())
        try:
            while True:
                event = await queue.get()
                if event is None:
                    if outcome_box is not None:
                        yield ExecEvent(kind="done", meta=outcome_box.meta)
                    break
                yield event
        finally:
            if not task.done():
                task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            if outcome_box is not None:
                self.last_meta[spec.id] = outcome_box.meta
_sessions: dict[str, Session] = {}


def session_for(session_key: str, project_id: str | None = None) -> Session:
    if session_key not in _sessions:
        settings = get_settings()
        settings.ensure_dirs()
        _sessions[session_key] = Session(get_project(settings, project_id))
    else:
        _sessions[session_key].reload()
    return _sessions[session_key]
