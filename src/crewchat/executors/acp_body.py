"""通用 ACP 身体：任何讲 ACP 的 CLI 都能当身体，不用逐个写适配器。

ACP（Agent Client Protocol，Zed 提出）规定了客户端（我们）和 agent（CLI）之间
怎么做会话：`initialize → session/new → session/prompt`，之后 agent 用
`session/update` 流式回文本、思考、工具调用、用量，用 `session/request_permission`
请示。**协议只有一份**，所以：

    加一个 CLI = 在这里的 `ACP_CLIS` 加一行（它的 acp 启动命令）。
    不用为它写旗标处理、事件解析、会话续接、工具事件映射——那些是各 CLI 私有的，
    也正是之前"每加一个 CLI 就要手写一个适配器、再实测一遍"的来源。

对比以前的逐个适配：claude 用 `--stream-json`、codex 用 `exec`、opencode 用
`run --format json`，三套私有格式 → 三个适配器。ACP 把这一层收敛成一份。
"""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import acp

from ..core.config import Settings
from ..core.providers import Provider, available_skills, shared_mcp_servers
from .base import ExecRequest, ExecResult, Executor, TokenUsage, register


# ---- 每个 CLI 的私有部分，只剩"怎么把它拉起来" ------------------------------

def _opencode_env(provider: Provider | None, req: ExecRequest, env: dict[str, str]) -> None:
    """opencode 的一次运行要自己的配置，不继承用户全局那份。

    否则每个 agent 都背着用户全部 MCP（一次简单回复 47 秒），用户改一个字影响所有
    agent。这里只装它的供应商、它声明的技能与 MCP。
    """
    env["OPENCODE_CONFIG_CONTENT"] = _inline_config(provider, req.skills, req.mcps)
    env.setdefault("OPENCODE_FAST_BOOT", "1")
    env.setdefault("OPENCODE_DISABLE_MODELS_FETCH", "1")
    env.setdefault("OPENCODE_DISABLE_FILEWATCHER", "1")
    env.setdefault("OPENCODE_DISABLE_AUTOUPDATE", "1")


#: 会讲 ACP 的 CLI → 启动方式与它自己那点额外环境。加一个 CLI 就是加一行。
#: `npx` 那两个是 Zed 官方 ACP 桥（claude/codex 本体还没有原生 acp 子命令），
#: 首次运行由 npx 拉取。所有条目都只是"启动命令不同"，协议完全一样。
ACP_CLIS: dict[str, dict[str, Any]] = {
    "opencode": {"label": "OpenCode", "command": ["opencode", "acp"], "env": _opencode_env},
    "openclaw": {"label": "OpenClaw", "command": ["openclaw", "acp"]},
    "claude": {"label": "Claude Code", "command": ["npx", "-y", "@zed-industries/claude-code-acp"]},
    "codex": {"label": "Codex CLI", "command": ["npx", "-y", "@zed-industries/codex-acp"]},
}


def _mcp_entry(spec: dict[str, Any]) -> dict[str, Any]:
    """`~/.agents/mcp.json` 的一条 → opencode 的 `mcp` 条目（形状不同，转一次）。"""
    url = spec.get("url")
    if url:
        return {"type": "remote", "url": url, "enabled": True}
    command = spec.get("command")
    if not command:
        return {}
    entry: dict[str, Any] = {
        "type": "local",
        "command": [str(command), *[str(a) for a in (spec.get("args") or [])]],
        "enabled": True,
    }
    env = spec.get("env") or spec.get("environment")
    if env:
        entry["environment"] = {str(k): str(v) for k, v in env.items()}
    return entry


def _inline_config(provider: Provider | None, skills: list[str], mcps: list[str]) -> str:
    """为这一次运行生成 opencode 配置，不回退到用户全局配置。"""
    cfg: dict[str, Any] = {"$schema": "https://opencode.ai/config.json"}

    if provider is not None and provider.id:
        models = {mid: {"name": mid} for mid in (provider.model_ids() or [provider.model])}
        cfg["provider"] = {
            provider.id: {
                "npm": "@ai-sdk/openai-compatible",
                "name": provider.name or provider.id,
                "options": {
                    "baseURL": provider.base_url,
                    "apiKey": f"{{env:{provider.env_key()}}}",
                },
                "models": models,
            }
        }

    if skills:
        known = {s["name"]: s["path"] for s in available_skills()}
        paths = sorted({str(Path(p).parent) for name in skills if (p := known.get(name))})
        if paths:
            cfg["skills"] = {"paths": paths}

    if mcps:
        servers = shared_mcp_servers()
        entries = {
            name: entry for name in mcps if name in servers and (entry := _mcp_entry(servers[name]))
        }
        if entries:
            cfg["mcp"] = entries

    return json.dumps(cfg, ensure_ascii=False)


# ---- ACP 客户端：把 agent 的更新翻译成我们统一的 ExecEvent --------------------

class _AcpClient(acp.Client):
    """我们作为 ACP 客户端。

    ACP 里的回调全在一个协议上，各 CLI 不再有差别——这正是通用化的落点。权限默认
    放行（等价于各 CLI 的 `--auto`），文件读写走真实文件系统。
    """

    def __init__(self, on_event: Callable[[str, dict[str, Any]], None]) -> None:
        self._emit = on_event

    async def session_update(self, session_id: str, update: Any, **kwargs: Any) -> None:
        kind = type(update).__name__
        if kind in ("AgentMessageChunk", "UserMessageChunk"):
            text = getattr(getattr(update, "content", None), "text", None)
            if text:
                self._emit("delta", {"text": text})
        elif kind == "AgentThoughtChunk":
            text = getattr(getattr(update, "content", None), "text", None)
            if text:
                self._emit("thinking", {"text": text})
        elif kind in ("ToolCallStart", "ToolCallProgress"):
            self._emit(
                "tool",
                {
                    "tool": getattr(update, "title", "") or getattr(update, "kind", "") or "tool",
                    "status": getattr(update, "status", "") or "in_progress",
                    "call_id": getattr(update, "tool_call_id", "") or "",
                    "args": getattr(update, "raw_input", None),
                    "result": _stringify(getattr(update, "raw_output", None)),
                },
            )
        elif kind == "UsageUpdate":
            cost = getattr(update, "cost", None)
            self._emit("usage", {"cost": getattr(cost, "amount", None)})

    async def request_permission(
        self, session_id: str, tool_call: Any, options: list[Any], **kwargs: Any
    ) -> acp.RequestPermissionResponse:
        allow = next(
            (
                o
                for o in options
                if getattr(o, "kind", "") in ("allow_once", "allow_always")
            ),
            options[0] if options else None,
        )
        if allow is None:
            return acp.RequestPermissionResponse(
                outcome=acp.schema.DeniedOutcome(outcome="cancelled")
            )
        return acp.RequestPermissionResponse(
            outcome=acp.schema.AllowedOutcome(option_id=allow.option_id)
        )

    async def read_text_file(
        self, session_id: str, path: str, line: int | None = None, limit: int | None = None, **kwargs: Any
    ) -> acp.ReadTextFileResponse:
        try:
            text = Path(path).read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            text = f"<unreadable: {exc}>"
        return acp.ReadTextFileResponse(content=text)

    async def write_text_file(
        self, session_id: str, path: str, content: str, **kwargs: Any
    ) -> acp.WriteTextFileResponse | None:
        target = Path(path)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        return acp.WriteTextFileResponse()


def _stringify(value: Any) -> str | None:
    if value is None:
        return None
    return value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)[:2000]


def _accumulate(reply: Any, usage: TokenUsage) -> None:
    """把 ACP 的 PromptResponse.usage 归到我们统一的计数上。"""
    u = getattr(reply, "usage", None)
    if u is None:
        return
    usage.input_tokens += int(getattr(u, "input_tokens", 0) or 0)
    usage.output_tokens += int(getattr(u, "output_tokens", 0) or 0)
    usage.reasoning_tokens += int(getattr(u, "thought_tokens", 0) or 0)
    usage.cache_read_tokens += int(getattr(u, "cached_read_tokens", 0) or 0)
    usage.cache_creation_tokens += int(getattr(u, "cached_write_tokens", 0) or 0)


# ---- 身体本体 ---------------------------------------------------------------

class AcpBody(Executor):
    """一个讲 ACP 的 CLI。`command` 由注册时按 `ACP_CLIS` 填进来。"""

    kind = "cli"
    has_builtin_brain = True
    needs_gateway = False
    command: list[str] = []
    env_hook: Callable[[Provider | None, ExecRequest, dict[str, str]], None] | None = None

    @classmethod
    def available(cls) -> bool:
        return bool(cls.command) and shutil.which(cls.command[0]) is not None

    async def run(self, req: ExecRequest, provider: Provider | None) -> ExecResult:
        started = time.monotonic()
        text_parts: list[str] = []
        usage = TokenUsage()
        cost_usd: float | None = None
        error: str | None = None

        env = dict(os.environ)
        if provider is not None and provider.api_key:
            env.setdefault(provider.env_key(), provider.api_key)
        if self.env_hook is not None:
            self.env_hook(provider, req, env)

        def on_event(kind: str, data: dict[str, Any]) -> None:
            nonlocal cost_usd
            if kind == "delta":
                text_parts.append(data["text"])
                req.emit("delta", text=data["text"])
            elif kind == "thinking":
                req.emit("thinking", text=data["text"])
            elif kind == "tool":
                req.emit(
                    "tool",
                    tool=data["tool"],
                    status=data["status"],
                    call_id=data["call_id"],
                    args=data["args"] if isinstance(data["args"], dict) else None,
                    result=data["result"],
                )
            elif kind == "usage":
                if data.get("cost") is not None:
                    cost_usd = (cost_usd or 0.0) + float(data["cost"] or 0.0)

        client = _AcpClient(on_event)
        session_id = req.session
        try:
            async with acp.spawn_agent_process(
                client, *self.command, cwd=str(req.cwd), env=env
            ) as (conn, _proc):
                await conn.initialize(
                    protocol_version=acp.PROTOCOL_VERSION,
                    client_info=acp.schema.Implementation(name="CrewChat", version="1.0.0"),
                )
                session_id, config_options = await _open_session(conn, req)
                # ACP 把"用哪个模型"也做成一个配置项。agent 默认用它自己的那家
                # （opencode 默认 openrouter），不选就会跑到别人的额度上。按该 agent
                # 的大脑选出对应项——这一步对任何讲 ACP 的 CLI 都一样。
                await _select_model(conn, session_id, config_options, provider)
                reply = await conn.prompt(session_id=session_id, prompt=[acp.text_block(req.prompt)])
                _accumulate(reply, usage)
        except Exception as exc:  # noqa: BLE001 — 任何失败都要变成一次可读的 error
            error = f"{type(exc).__name__}: {exc}"

        return ExecResult(
            text="".join(text_parts).strip(),
            exit_code=0 if not error else 1,
            duration_ms=int((time.monotonic() - started) * 1000),
            session=session_id,
            cost_usd=cost_usd,
            usage=None if usage.is_empty() else usage,
            error=error,
            meta={"body": self.id, "model": provider.model if provider else "cli-default"},
        )


async def _open_session(conn: Any, req: ExecRequest) -> tuple[str, Any]:
    """续接已有会话；没有就新建。返回 (session_id, 配置项)。

    ACP 把会话交给 agent 自己存，我们只记 id。配置项里包含"可选哪些模型"。
    """
    if req.session:
        try:
            resumed = await conn.load_session(session_id=req.session, cwd=str(req.cwd))
            return getattr(resumed, "session_id", req.session), getattr(resumed, "config_options", None)
        except Exception:  # noqa: BLE001 — 续接失败就退回新建，不影响这一轮
            pass
    created = await conn.new_session(cwd=str(req.cwd))
    return created.session_id, getattr(created, "config_options", None)


def _select_values(option: Any) -> set[str]:
    """一个配置项可选的值。选项可能直接是值，也可能分组（再嵌一层）。"""
    values: set[str] = set()
    for entry in getattr(option, "options", None) or []:
        nested = getattr(entry, "options", None)
        if nested:  # SessionConfigSelectGroup
            values.update(str(getattr(o, "value", "")) for o in nested)
        else:
            values.add(str(getattr(entry, "value", "")))
    return values


async def _select_model(conn: Any, session_id: str, config_options: Any, provider: Provider | None) -> None:
    """把会话的模型选成该 agent 的大脑。

    ACP 把"用哪个模型"做成一个配置项，agent 默认用它自己那家（opencode 默认
    openrouter）。不选，请求就落到别人的额度上。找到那个提供了我们要的模型的配置项，
    设过去——这一步对任何讲 ACP 的 CLI 都一样。
    """
    if provider is None or not provider.id:
        return
    want = f"{provider.id}/{provider.model}"
    for option in config_options or []:
        if want in _select_values(option):
            await conn.set_config_option(
                config_id=getattr(option, "id", "model"), session_id=session_id, value=want
            )
            return


def register_acp_bodies() -> None:
    """把 `ACP_CLIS` 里的每个 CLI 注册成一个身体。已被 bespoke 适配器占用的 id 跳过。"""
    from .base import _REGISTRY  # noqa: PLC0415 — 注册期读取当前注册表

    for cli_id, spec in ACP_CLIS.items():
        if cli_id in _REGISTRY:
            continue
        body = type(
            f"Acp_{cli_id}",
            (AcpBody,),
            {
                "id": cli_id,
                "label": spec.get("label", cli_id),
                "command": list(spec["command"]),
                # staticmethod：否则取 `self.env_hook` 会把它当方法绑定，多传一个 self。
                "env_hook": staticmethod(spec["env"]) if spec.get("env") else None,
            },
        )
        register(body)
