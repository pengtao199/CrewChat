from __future__ import annotations

import chainlit as cl
from chainlit.data.sql_alchemy import SQLAlchemyDataLayer
from chainlit.message import Message

from .config import get_settings
from .executors.base import ExecEvent
from .models import AgentSpec
from .agents import AgentFactory
from .orchestrator import Session, drop_session, session_for
from .overrides import current_settings, save_overrides
from .projects import Project, get_project, load_projects, save_projects
from .providers import load_providers
from .settings_panel import (
    K_CODEX_MODEL,
    K_CODEX_SANDBOX,
    K_CLAUDE_MODEL,
    K_CLAUDE_PERM,
    K_GROUP,
    K_MAX_TOKENS,
    K_MODEL,
    K_NEW_PROJECT,
    K_PROJECT,
    K_TEAM,
    K_TEMP,
    K_BASE_URL,
    K_API_KEY,
    K_PROVIDER,
    apply_brain,
    brain_markdown,
    build_settings,
    capabilities_markdown,
    env_markdown,
    selected_provider_id,
    team_markdown,
)

STARTERS = (
    ("🩺 排查问题", "帮我看看这个项目最近改动的代码，找出会导致线上问题的点。"),
    ("👥 群聊：技术选型", "要在 Postgres、SQLite、纯文件三种存储里选一个。先各自表态，再互相质疑，最后收敛。"),
)


def _db_url() -> str:
    settings = get_settings()
    settings.ensure_dirs()
    return f"sqlite+aiosqlite:///{settings.db_path}"


@cl.data_layer
def data_layer() -> SQLAlchemyDataLayer:
    return SQLAlchemyDataLayer(conninfo=_db_url())


@cl.on_app_startup
async def on_startup() -> None:
    get_settings().ensure_dirs()


def _session() -> Session:
    """只在 Chainlit 请求上下文里可用（set_chat_profiles 在启动期没有 session）。"""
    key = str(cl.context.session.id)
    project_id = cl.user_session.get("project_id")
    existing = cl.user_session.get(_SESSION_KEY)
    if existing is not None and existing.project.id == project_id:
        existing.reload()
        return existing
    settings = get_settings()
    session = session_for(key, project_id)
    cl.user_session.set(_SESSION_KEY, session)
    return session


_SESSION_KEY = "crewchat.session"


@cl.set_chat_profiles
async def chat_profiles() -> list[cl.ChatProfile]:
    settings = get_settings()
    settings.ensure_dirs()
    first = load_projects(settings)[0]
    rows = AgentFactory(settings, first.workspace()).health()
    starters = [cl.Starter(label=label, message=msg) for label, msg in STARTERS]
    return [
        cl.ChatProfile(
            name=f"{r['emoji']} {r['name']}",
            markdown_description=_desc(r),
            default=i == 0,
            starters=starters,
        )
        for i, r in enumerate(rows)
    ]


def _desc(row: dict) -> str:
    brain = (
        f"{row['brain_provider']} / `{row['brain_model']}`"
        if row["brain_model"]
        else ("CLI 内置" if row["brain_builtin"] else "**未配置大脑**")
    )
    caps = [c for c in (
        f"skill×{len(row['skills'])}" if row["skills"] else "",
        f"mcp×{len(row['mcps'])}" if row["mcps"] else "",
        f"plugin×{len(row['plugins'])}" if row["plugins"] else "",
        "memory" if row["memory"] else "",
    ) if c]
    return (
        f"{row['role'] or row['name']}\n\n"
        f"- 身体：`{row['body']}`\n- 大脑：{brain}\n"
        f"- 能力：{' · '.join(caps) or '—'}\n"
        f"- {'✅ 就绪' if row['ready'] else '⛔️ 不可用'}"
    )


@cl.on_chat_start
async def on_start() -> None:
    session = _session()
    await _push_sidebar(session)
    if not session.specs:
        await cl.Message(
            content=(
                f"### 这个项目还没有 agent\n\n"
                f"在 `{session.project.agents_path()}` 建一个 `*.jsonc`：\n\n"
                "```json\n"
                '{\n  "name": "编码工人",\n  "emoji": "👨‍💻",\n'
                '  "brain": "default",\n  "body": "codex",\n'
                '  "role": "资深工程师",\n  "goal": "交付能跑的代码",\n'
                '  "memory": true\n}\n```\n\n'
                "`brain` 引用大脑池 id，`body` 选 `codex` / `claude` / `llm`。"
            ),
            author="system",
        ).send()
    await build_settings(session.health()).send()


@cl.action_callback
async def refresh_status() -> None:
    await cl.Message(
        content="\n\n".join([env_markdown(), brain_markdown()]), author="system"
    ).send()
    await _push_sidebar(_session())


def _problems(session: Session) -> list[str]:
    settings = current_settings()
    out: list[str] = []
    if not load_providers(settings):
        out.append("**大脑池为空** — 在设置 → 🧠 大脑 里添加模型端点")
    elif not any(p.has_key() for p in load_providers(settings)):
        out.append("**没有可用的 API key** — `llm` 身体的 agent 会不可用")
    from .diagnostics import snapshot

    probes = snapshot(settings)
    for probe in (probes["codex"], probes["claude"]):
        if not probe.installed:
            out.append(f"**{probe.label}** 未安装")
        elif probe.logged_in is False:
            out.append(f"**{probe.label}** 未登录 — {probe.detail or ''}")
    for row in session.health():
        if not row["ready"]:
            out.append(f"Agent **{row['name']}** 不可用 — {row.get('note') or '缺少大脑'}")
    return out


@cl.on_settings_update
async def on_settings_update(values: dict) -> None:
    values = values or {}
    settings = get_settings()
    settings.ensure_dirs()

    new_path = str(values.get(K_NEW_PROJECT) or "").strip()
    if new_path:
        projects = load_projects(settings)
        if not any(p.path == new_path for p in projects):
            from pathlib import Path as P

            projects.append(
                Project(id=f"p{len(projects)}", name=P(new_path).name or new_path, path=new_path)
            )
            save_projects(settings, projects)
            values = {**values, K_PROJECT: projects[-1].id}

    project_id = str(values.get(K_PROJECT) or "") or None
    if project_id:
        cl.user_session.set("project_id", project_id)
        cl.user_session.set(_SESSION_KEY, session_for(str(cl.context.session.id), project_id))

    apply_brain(values, settings)
    persist = {
        K_CODEX_MODEL: "codex_model",
        K_CODEX_SANDBOX: "codex_sandbox",
        K_CLAUDE_MODEL: "claude_model",
        K_CLAUDE_PERM: "claude_permission_mode",
    }
    payload = {field: str(values[key]).strip() for key, field in persist.items() if key in values}
    if payload:
        save_overrides(current_settings(), payload)

    await _push_sidebar(_session())


@cl.on_message
async def on_user_message(message: Message) -> None:
    text = (message.content or "").strip()
    if not text:
        return
    session = _session()
    values = dict(cl.context.session.settings or {})
    team = [str(v) for v in (values.get(K_TEAM) or [])]
    group = bool(values.get(K_GROUP)) and len(team) >= 2
    thread = message.thread_id or cl.context.session.id

    if group:
        specs = session.team(team)
        if specs:
            await _run_group(session, specs, text, thread)
            return

    profile = (cl.context.session.chat_profile or "").strip()
    spec = _pick(session, profile)
    if spec is None:
        await cl.Message(content="这个项目还没有 agent。先在 `agents/*.jsonc` 里添加。", author="system").send()
        return
    await _run_one(session, spec, _persona(spec, text), thread)


def _pick(session: Session, profile: str) -> AgentSpec | None:
    if profile:
        for spec in session.specs.values():
            if spec.display in profile or spec.name == profile:
                return spec
    return session.pick(None)


def _persona(spec: AgentSpec, text: str) -> str:
    persona = spec.persona()
    return f"{persona}\n\n---\n\n{text}" if persona else text


async def _run_one(session: Session, spec: AgentSpec, prompt: str, thread: str) -> None:
    label = spec.display
    step = cl.Step(name=f"{label} 执行中…", type="run")
    stream = cl.Message(content="", author=label)
    tasks = cl.TaskList(name=f"{label} 的动作")
    states: dict[str, cl.TaskStatus] = {}

    async with step:
        async for event in session.stream_ask(spec, prompt, thread):
            if event.kind == "delta" and event.text:
                await stream.stream_token(event.text)
            elif event.kind == "tool":
                _sync_tool(tasks, states, event)
        await stream.send()
        await step.update(name=f"{label} 完成")


async def _run_group(session: Session, specs: list[AgentSpec], text: str, thread: str) -> None:
    await cl.Message(
        content=f"👥 群聊 · {' → '.join(s.display for s in specs)}", author="system"
    ).send()
    async for spec, event in session.group_turn(specs, text, thread):
        if event.kind == "delta" and event.text:
            await cl.Message(content=event.text, author=spec.display).send()
        elif event.kind == "error" and event.text:
            await cl.Message(content=f"```\n{event.text}\n```", author="system").send()


def _sync_tool(tasks: cl.TaskList, states: dict[str, cl.TaskStatus], event: ExecEvent) -> None:
    if event.status == "idle":
        for title, status in list(states.items()):
            if status is cl.TaskStatus.RUNNING:
                states[title] = cl.TaskStatus.DONE
        _apply(tasks, states)
        return
    title = event.tool or "tool"
    states[title] = {
        "completed": cl.TaskStatus.DONE,
        "failed": cl.TaskStatus.FAILED,
        "in_progress": cl.TaskStatus.RUNNING,
    }.get(event.status, cl.TaskStatus.RUNNING)
    if title not in [t.title for t in tasks.tasks]:
        tasks.tasks = [*tasks.tasks, cl.Task(title=title, status=states[title])]
    else:
        _apply(tasks, states)


def _apply(tasks: cl.TaskList, states: dict[str, cl.TaskStatus]) -> None:
    tasks.tasks = [cl.Task(title=t.title, status=states.get(t.title, t.status)) for t in tasks.tasks]


async def _push_sidebar(session: Session) -> None:
    settings = get_settings()
    projects = load_projects(settings)
    active = session.project
    await cl.ElementSidebar.set_title(f"{active.name}")
    await cl.ElementSidebar.set_elements(
        [
            cl.Text(
                content="\n\n".join(
                    [team_markdown(session.health()), env_markdown(), brain_markdown(), capabilities_markdown()]
                ),
                name="项目概览",
                display="side",
            )
        ],
        key=f"crewchat:{active.id}",
    )


@cl.on_feedback
async def on_feedback(feedback) -> None:  # type: ignore[no-untyped-def]
    print(f"[feedback] {feedback.score}")


@cl.on_app_shutdown
async def on_shutdown() -> None:
    print("[crewchat] shutdown")