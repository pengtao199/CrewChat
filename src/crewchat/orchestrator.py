from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

from .agents import AgentFactory
from .config import Settings, get_settings
from .executors import ExecRequest, get_executor
from .executors.base import ExecEvent
from .models import AgentSpec, load_specs
from .projects import Project, get_project
from .providers import Provider

Emitter = Any


class RunOutcome:
    def __init__(self) -> None:
        self.text: str = ""
        self.error: str | None = None
        self.meta: dict[str, Any] = {}

    @property
    def ok(self) -> bool:
        return self.error is None and bool(self.text)


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

    def factory(self) -> AgentFactory:
        return AgentFactory(self.settings, self.workspace)

    def pick(self, agent_id: str | None) -> AgentSpec | None:
        if agent_id and agent_id in self.specs:
            return self.specs[agent_id]
        return next(iter(self.specs.values()), None)

    def team(self, agent_ids: list[str]) -> list[AgentSpec]:
        return [self.specs[a] for a in agent_ids if a in self.specs]

    def provider_for(self, spec: AgentSpec) -> Provider | None:
        """CLI 身体自带大脑；只有 `brain: provider/model` 才用大脑池覆盖。"""
        if spec.is_cli_backed and "/" not in spec.brain:
            return None
        return AgentFactory(self.settings, self.workspace).provider_for(spec)

    async def ask(
        self,
        spec: AgentSpec,
        prompt: str,
        session_key: str,
        on_event: Emitter = None,
    ) -> RunOutcome:
        executor = get_executor(spec.body)
        factory = self.factory()
        factory.apply_env(spec)
        memory_key = self.sessions.setdefault(spec.id, f"{session_key}:{spec.id}")
        outcome = RunOutcome()
        try:
            result = await executor.run(
                ExecRequest(
                    prompt=prompt,
                    cwd=Path(spec.cwd).expanduser() if spec.cwd else self.workspace,
                    timeout=self.settings.executor_timeout,
                    session=memory_key,
                    on_event=on_event,
                ),
                self.provider_for(spec),
            )
        except Exception as exc:
            outcome.error = f"{type(exc).__name__}: {exc}"
            return outcome
        outcome.text = result.text
        outcome.error = result.error
        outcome.meta = {
            **result.meta,
            "duration_ms": result.duration_ms,
            "cost_usd": result.cost_usd,
            "num_turns": result.num_turns,
        }
        return outcome

    async def stream_ask(
        self,
        spec: AgentSpec,
        prompt: str,
        session_key: str,
    ) -> AsyncIterator[ExecEvent]:
        """跑一次 agent 回合，边跑边产出事件。"""
        executor = get_executor(spec.body)
        factory = self.factory()
        factory.apply_env(spec)
        memory_key = self.sessions.setdefault(spec.id, f"{session_key}:{spec.id}")
        queue: asyncio.Queue[ExecEvent | None] = asyncio.Queue()

        def on_event(event: ExecEvent) -> None:
            queue.put_nowait(event)

        async def drive() -> RunOutcome:
            try:
                result = await executor.run(
                    ExecRequest(
                        prompt=prompt,
                        cwd=Path(spec.cwd).expanduser() if spec.cwd else self.workspace,
                        timeout=self.settings.executor_timeout,
                        session=memory_key,
                        on_event=on_event,
                    ),
                    self.provider_for(spec),
                )
            except Exception as exc:
                outcome = RunOutcome()
                outcome.error = f"{type(exc).__name__}: {exc}"
                return outcome
            outcome = RunOutcome()
            outcome.text = result.text
            outcome.error = result.error
            outcome.meta = {
                **result.meta,
                "duration_ms": result.duration_ms,
                "cost_usd": result.cost_usd,
                "num_turns": result.num_turns,
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
                    break
                yield event
        finally:
            if not task.done():
                task.cancel()
            await asyncio.gather(task, return_exceptions=True)
            if outcome_box is not None:
                self.last_meta[spec.id] = outcome_box.meta

    async def group_turn(
        self,
        specs: list[AgentSpec],
        message: str,
        session_key: str,
    ) -> AsyncIterator[tuple[AgentSpec, ExecEvent]]:
        """群聊：在场 agent 依次发言并能看到彼此的产出。"""
        transcript: list[str] = []
        for spec in specs:
            others = "、".join(s.display for s in specs if s.id != spec.id) or "（无）"
            head = "\n".join(p for p in (spec.persona(), "", f"在场 agent（你之外）：{others}") if p)
            prior = ("\n上一轮讨论：\n" + "\n".join(transcript) + "\n") if transcript else ""
            prompt = (
                head
                + "\n\n"
                + prior
                + f"用户：{message}\n\n"
                + "给出你的判断与产出。别人错了直接指出；不要复述、不要客套、不要说『好的』。"
                + "控制在 6 句以内。"
            )
            async for event in self.stream_ask(spec, prompt, session_key):
                yield spec, event
            meta = self.last_meta.get(spec.id) or {}
            transcript.append(f"[{spec.display}] {meta.get('_text', '')}")

    def health(self) -> list[dict[str, Any]]:
        return AgentFactory(self.settings, self.workspace).health()


_sessions: dict[str, Session] = {}


def session_for(session_key: str, project_id: str | None = None) -> Session:
    if session_key not in _sessions:
        settings = get_settings()
        settings.ensure_dirs()
        _sessions[session_key] = Session(get_project(settings, project_id))
    else:
        _sessions[session_key].reload()
    return _sessions[session_key]


def drop_session(session_key: str) -> None:
    _sessions.pop(session_key, None)