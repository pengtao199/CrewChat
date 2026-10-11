from __future__ import annotations

import asyncio
import re
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

from ..core.config import get_settings
from ..executors import ExecRequest, get_executor
from ..executors.base import ExecEvent
from ..core.models import AgentSpec, load_specs
from ..core.projects import Project, agents_dir_for, load_projects
from ..core.providers import Provider, resolve_brain

Emitter = Any


class RunOutcome:
    def __init__(self) -> None:
        self.text: str = ""
        self.error: str | None = None
        self.meta: dict[str, Any] = {}


class Session:
    """一条会话：可选的工作区 + 在场 agent + 每个 agent 的记忆。

    agent 是全局的（不随工作区变）；`workspace` 只决定执行 cwd。
    """

    def __init__(self, project: Project | None, workspace: Path) -> None:
        self.project = project
        self.workspace_dir = workspace
        self.settings = get_settings()
        self.specs: dict[str, AgentSpec] = {}
        self.sessions: dict[str, str] = {}
        self.last_meta: dict[str, dict[str, Any]] = {}
        self.reload()

    @property
    def workspace(self) -> Path:
        return self.workspace_dir

    def reload(self) -> None:
        # agent 全局：不随 project 变。
        self.specs = {s.id: s for s in load_specs(agents_dir_for(self.settings))}

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
        # Key sessions per (conversation, agent): agents are global, but each
        # conversation needs its own memory. A stable key so the mapping below
        # survives across turns.
        session_slot = f"{session_key}:{spec.id}"
        # Only a session id the CLI actually gave us may be resumed. Inventing
        # one (`conv:<id>:<agent>`) makes the CLI abort with exit 1, because
        # `--resume` on an unknown session is an error, not a "create" request.
        known_session = self.sessions.get(session_slot)
        queue: asyncio.Queue[ExecEvent | None] = asyncio.Queue()

        def on_event(event: ExecEvent) -> None:
            queue.put_nowait(event)

        async def drive() -> RunOutcome:
            provider = self.provider_for(spec)
            try:
                # cc-switch 网关已移除：身体使用各自 CLI 的登录与用户配置。
                # `provider` 仅作为契约参数透传，留给未来的 Agent Host 注入。
                result = await executor.run(
                    ExecRequest(
                        prompt=prompt,
                        cwd=spec.resolve_cwd(self.workspace),
                        timeout=self.settings.executor_timeout,
                        session=known_session,
                        on_event=on_event,
                        skills=list(spec.skills),
                        mcps=list(spec.mcps),
                        app_id=spec.body,
                        conversation_id=session_key,
                    ),
                    provider,
                )
            except Exception as exc:
                outcome = RunOutcome()
                outcome.error = f"{type(exc).__name__}: {exc}"
                queue.put_nowait(ExecEvent(kind="error", text=outcome.error))
                return outcome
            outcome = RunOutcome()
            outcome.text = result.text
            outcome.error = result.error
            # Remember the session id the CLI minted, so the next turn in this
            # conversation resumes the same thread instead of starting over.
            if result.session:
                self.sessions[session_slot] = result.session
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


def _resolve_project(settings: Any, project_id: str | None) -> Project | None:
    if not project_id:
        return None
    for project in load_projects(settings):
        if project.id == project_id:
            return project
    return None


def _session_workspace(settings: Any, session_key: str, project: Project | None) -> Path:
    """cwd 三态：绑了工作区 → 该工作区；否则每会话一个沙盒目录（避免并发互踩）。"""
    if project is not None:
        return project.workspace()
    safe = re.sub(r"[^A-Za-z0-9_.-]", "_", session_key)[:80] or "session"
    return settings.data_dir / "workspaces" / safe


def session_for(session_key: str, project_id: str | None = None) -> Session:
    if session_key not in _sessions:
        settings = get_settings()
        settings.ensure_dirs()
        project = _resolve_project(settings, project_id)
        workspace = _session_workspace(settings, session_key, project)
        workspace.mkdir(parents=True, exist_ok=True)
        _sessions[session_key] = Session(project, workspace)
    else:
        _sessions[session_key].reload()
    return _sessions[session_key]


def reload_all() -> None:
    """重新加载所有已缓存 Session 的 agent（agent 全局，改一处全体可见）。"""
    for session in _sessions.values():
        session.reload()
