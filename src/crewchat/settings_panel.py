from __future__ import annotations

import chainlit as cl
from chainlit.input_widget import (
    MultiSelect,
    NumberInput,
    Select,
    Slider,
    Switch,
    Tab,
    TextInput,
)

from .diagnostics import snapshot
from .overrides import current_settings, mask_secret
from .projects import load_projects
from .providers import Provider, available_plugins, available_skills, load_providers, save_providers

K_PROJECT = "project_id"
K_NEW_PROJECT = "new_project_path"
K_TEAM = "team_ids"
K_GROUP = "group_mode"
K_PROVIDER = "provider_id"
K_BASE_URL = "llm_base_url"
K_MODEL = "llm_model"
K_API_KEY = "llm_api_key"
K_TEMP = "llm_temperature"
K_MAX_TOKENS = "llm_max_tokens"
K_CODEX_MODEL = "codex_model"
K_CODEX_SANDBOX = "codex_sandbox"
K_CLAUDE_MODEL = "claude_model"
K_CLAUDE_PERM = "claude_permission_mode"

SANDBOX_CHOICES = ("read-only", "workspace-write", "danger-full-access")
PERMISSION_CHOICES = ("default", "acceptEdits", "bypassPermissions", "dontAsk", "plan")

STATUS_MARK = {True: "已登录", False: "未登录", None: "—"}


def env_markdown(settings=None) -> str:
    cfg = settings or current_settings()
    probes = snapshot(cfg)
    lines = ["### 🦴 本机 CLI", ""]
    for probe in (probes["codex"], probes["claude"]):
        if not probe.installed:
            lines.append(f"- ⛔️ **{probe.label}** — 未安装")
            continue
        mark = "✅" if probe.ready else "⚠️"
        method = f"（{probe.auth_method}）" if probe.auth_method else ""
        lines.append(
            f"- {mark} **{probe.label}** `{probe.version}` · {STATUS_MARK[probe.logged_in]}{method}"
        )
        if probe.detail and not probe.ready:
            lines.append(f"  ↳ {probe.detail}")
    return "\n".join(lines)


def brain_markdown(settings=None) -> str:
    cfg = settings or current_settings()
    providers = load_providers(cfg)
    if not providers:
        return "### 🧠 大脑池为空\n\n在设置里添加一个模型端点。"
    lines = ["### 🧠 大脑池", "", "| | 名称 | 提供商 | 模型 | key |", "| :-: | --- | --- | --- | :-: |"]
    from .diagnostics import provider_name

    for p in providers:
        lines.append(
            f"| {'●' if p.id == selected_provider_id() else ''} | **{p.name}** | "
            f"{provider_name(p.base_url)} | `{p.model}` | {'✅' if p.has_key() else '—'} |"
        )
    return "\n".join(lines)


_selected_provider = ""


def selected_provider_id() -> str:
    if _selected_provider:
        return _selected_provider
    providers = load_providers(current_settings())
    return providers[0].id if providers else ""


def capabilities_markdown() -> str:
    skills = available_skills()
    plugins = available_plugins()
    return "\n".join(
        [
            "### 🧩 可复用能力（来自 `~/.agents/`）",
            "",
            f"- **Skills** {len(skills)} 个 · 例：{', '.join(s['name'] for s in skills[:4])}",
            f"- **Plugins** {len(plugins)} 个 · {', '.join(plugins) or '无'}",
            "- **MCP** 见 `~/.agents/mcp.json`（在 agent 的 `mcps` 字段里按名字引用）",
        ]
    )


def team_markdown(rows: list[dict]) -> str:
    if not rows:
        return "### 👥 这个项目还没有 agent\n\n在项目的 `agents/*.jsonc` 里添加。"
    lines = [
        "### 👥 Agent",
        "",
        "| | Agent | 身体 | 大脑 | 能力 | 状态 |",
        "| :-: | --- | --- | --- | --- | :-: |",
    ]
    for r in rows:
        brain = (
            f"{r['brain_provider']} / `{r['brain_model']}`"
            if r["brain_model"]
            else ("CLI 内置" if r["brain_builtin"] else "—")
        )
        caps = []
        if r["skills"]:
            caps.append(f"skill×{len(r['skills'])}")
        if r["mcps"]:
            caps.append(f"mcp×{len(r['mcps'])}")
        if r["plugins"]:
            caps.append(f"plugin×{len(r['plugins'])}")
        if r["memory"]:
            caps.append("memory")
        lines.append(
            f"| {r['emoji']} | **{r['name']}** | `{r['body']}` | {brain} | "
            f"{' · '.join(caps) or '—'} | {'✅' if r['ready'] else '⛔️'} |"
        )
    return "\n".join(lines)


def _picker(
    items: dict[str, str],
    *,
    widget_id: str,
    label: str,
    initial: str = "",
    empty: str,
    tooltip: str | None = None,
) -> Select | TextInput:
    """Chainlit 拒绝空 items，所以没有可选项时只给一个只读提示。"""
    if not items:
        return TextInput(id=widget_id, label=label, initial=empty, disabled=True)
    return Select(id=widget_id, label=label, items=items, initial=initial, tooltip=tooltip)


def build_settings(rows: list[dict], settings=None) -> cl.ChatSettings:
    cfg = settings or current_settings()
    projects = load_projects(cfg)
    providers = load_providers(cfg)
    provider = next((p for p in providers if p.id == selected_provider_id()), None)

    project_tab = Tab(
        id="tab_project",
        label="📁 项目",
        inputs=[
            _picker(
                {p.id: f"{p.name} — {p.path}" for p in projects},
                widget_id=K_PROJECT,
                label="当前项目",
                initial=projects[0].id if projects else "",
                empty="还没有项目，先在下面添加",
            ),
            TextInput(
                id=K_NEW_PROJECT,
                label="添加项目（绝对路径）",
                initial="",
                placeholder="/Users/you/code/my-project",
            ),
        ],
    )

    ids = [str(r["id"]) for r in rows]
    agent_tab = Tab(
        id="tab_agent",
        label="👥 Agent",
        inputs=[
            Switch(
                id=K_GROUP,
                label="群聊模式",
                initial=False,
                tooltip="开启后由下面的参与人依次发言并互相引用",
            ),
            *(
                [
                    MultiSelect(
                        id=K_TEAM,
                        label="群聊参与人",
                        items={str(r["id"]): f"{r['emoji']} {r['name']}" for r in rows},
                        initial=ids,
                    )
                ]
                if rows
                else [TextInput(id=K_TEAM, label="群聊参与人", initial="这个项目还没有 agent", disabled=True)]
            ),
        ],
    )

    brain_items = {p.id: f"{p.name} — {p.model}" for p in providers}
    brain_tab = Tab(
        id="tab_brain",
        label="🧠 大脑",
        inputs=[
            _picker(
                brain_items,
                widget_id=K_PROVIDER,
                label="模型端点",
                initial=provider.id if provider else "",
                empty="大脑池为空",
            ),
            TextInput(
                id=K_BASE_URL,
                label="base_url",
                initial=provider.base_url if provider else "",
                placeholder="https://api.deepseek.com/v1",
            ),
            TextInput(
                id=K_MODEL,
                label="模型",
                initial=provider.model if provider else "",
                placeholder="deepseek-chat",
            ),
            TextInput(
                id=K_API_KEY,
                label="API key",
                initial="",
                placeholder=mask_secret(provider.api_key) if provider else "sk-…",
                tooltip="留空 = 不修改已保存的 key",
            ),
            Slider(
                id=K_TEMP,
                label="temperature",
                initial=float(provider.temperature if provider and provider.temperature is not None else 0.7),
                min=0.0,
                max=2.0,
                step=0.05,
            ),
            NumberInput(
                id=K_MAX_TOKENS,
                label="max_tokens",
                initial=int(provider.max_tokens if provider and provider.max_tokens else 4096),
                min=256,
                max=131072,
                step=256,
            ),
        ],
    )

    body_tab = Tab(
        id="tab_body",
        label="🦴 身体",
        inputs=[
            TextInput(
                id=K_CODEX_MODEL,
                label="codex 模型（空=CLI 默认）",
                initial=cfg.codex_model,
                placeholder="gpt-5-codex",
            ),
            Select(
                id=K_CODEX_SANDBOX,
                label="codex 沙箱",
                values=list(SANDBOX_CHOICES),
                initial=cfg.codex_sandbox if cfg.codex_sandbox in SANDBOX_CHOICES else "workspace-write",
            ),
            TextInput(
                id=K_CLAUDE_MODEL,
                label="claude 模型（空=CLI 默认）",
                initial=cfg.claude_model,
                placeholder="sonnet",
            ),
            Select(
                id=K_CLAUDE_PERM,
                label="claude 权限模式",
                values=list(PERMISSION_CHOICES),
                initial=cfg.claude_permission_mode
                if cfg.claude_permission_mode in PERMISSION_CHOICES
                else "bypassPermissions",
            ),
        ],
    )

    return cl.ChatSettings(inputs=[project_tab, agent_tab, brain_tab, body_tab])


def apply_brain(values: dict, settings=None) -> None:
    """把 🧠 大脑 tab 的改动写回 providers.jsonc。"""
    cfg = settings or current_settings()
    providers = load_providers(cfg)
    if not providers:
        return
    picked = str(values.get(K_PROVIDER) or selected_provider_id())
    globals()["_selected_provider"] = picked
    target = next((p for p in providers if p.id == picked), providers[0])
    for key, attr, cast in (
        (K_BASE_URL, "base_url", str),
        (K_MODEL, "model", str),
    ):
        raw = values.get(key)
        if raw is not None and str(raw).strip():
            setattr(target, attr, str(raw).strip())
    if values.get(K_API_KEY):
        target.api_key = str(values[K_API_KEY]).strip()
    if values.get(K_TEMP) is not None:
        target.temperature = float(values[K_TEMP])
    if values.get(K_MAX_TOKENS) is not None:
        target.max_tokens = int(values[K_MAX_TOKENS])
    save_providers(cfg, providers)