from __future__ import annotations

import asyncio
import copy
import re
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

from ..core.config import get_settings
from ..executors import ExecRequest, get_executor
from ..executors.base import ExecEvent
from ..core.models import AgentSpec, load_specs
from ..core.projects import Project, agents_dir_for, load_projects
from ..core.providers import Provider, apply_brain_env, resolve_brain

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

    def _gateway_model(self, spec: AgentSpec, provider: Provider | None, needs_gateway: bool) -> str | None:
        """需要网关的身体经反代用上它的 (身体, provider, model)，返回该用的模型 id。

        每个经网关的 CLI 都返回这次的发布 id，写进这一次进程的配置。两个会话
        因此可以同时用两家供应商。没配大脑时不动，保留 CLI 自身登录。
        同一 (身体, provider, model) 只挂一次。
        """
        if provider is None or not needs_gateway:
            return None
        key = (spec.body, provider.id, provider.model)
        if key in _ROUTED:
            return _ROUTED[key]
        from ..core.gateway import get_gateway

        model_id = get_gateway(self.settings).activate(spec.body, provider)
        _ROUTED[key] = model_id
        return model_id

    async def stream_ask(
        self,
        spec: AgentSpec,
        prompt: str,
        session_key: str,
    ) -> AsyncIterator[ExecEvent]:
        """跑一次 agent 回合，边跑边产出事件。"""
        executor = get_executor(spec.body)
        apply_brain_env(self.settings, spec)
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
                # 需要网关的身体配了大脑就挂到反代上，否则它用自己的登录。
                model_id = self._gateway_model(spec, provider, executor.needs_gateway)
                # 不调用 ensure：它会把当前路由写进用户的全局 CLI 配置，两个会话就会抢同一家。
                effective = provider
                if model_id and provider is not None:
                    effective = copy.copy(provider)
                    effective.model = model_id
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
                    ),
                    effective,
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

#: (身体, provider id, model) → 该用的模型 id；挂过一次就复用，避免每回合重复 enter。
_ROUTED: dict[tuple[str, str, str], str | None] = {}


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
