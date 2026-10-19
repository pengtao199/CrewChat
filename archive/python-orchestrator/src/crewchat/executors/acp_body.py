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

from ..core.providers import Provider
from .base import ExecRequest, ExecResult, Executor, TokenUsage, register


def _mcode_launcher() -> Path | None:
    """官方安装器把入口放在 `~/.minimax-code/bin`，不改用户的 shell PATH。"""
    found = shutil.which("mcode")
    if found:
        return Path(found)
    candidate = Path.home() / ".minimax-code" / "bin" / "mcode"
    return candidate if os.access(candidate, os.X_OK) else None


#: 每个 CLI 的 ACP 启动命令。cc-switch 网关已移除：身体直接使用各自 CLI 的
#: 登录与用户配置；模型统一管理由 CCR（`ccr/`）承担，逐会话注入留给未来的 Agent Host。
ACP_CLIS: dict[str, dict[str, Any]] = {
    "opencode": {"label": "OpenCode", "command": ["opencode", "acp"]},
    "openclaw": {"label": "OpenClaw", "command": ["openclaw", "acp"]},
    "claude": {"label": "Claude Code", "command": ["claude-agent-acp"]},
    "codex": {"label": "Codex CLI", "command": ["codex-acp"]},
    "gemini": {"label": "Gemini CLI", "command": ["gemini", "--acp"]},
    "grok": {"label": "Grok Build", "command": ["grok", "agent", "stdio"]},
    "hermes": {"label": "Hermes Agent", "command": ["hermes", "acp"]},
    "pi": {"label": "Pi", "command": ["pi-acp"]},
    "mcode": {"label": "MiniMax Code", "command": ["mcode", "acp"]},
}


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
        """权限审批已退役：产品面的权限卡在 Codeg（它托管自己的 ACP 会话）。

        Python 侧自跑身体时不再弹卡，兜底为放行（没有允许项则拒绝）。
        """
        return _allow_response(options)

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


async def _discard_stderr(proc: asyncio.subprocess.Process) -> None:
    """读掉子进程 stderr。acp 默认把它接到管道上，没人读时管道写满，CLI 会卡在写日志。"""
    stream = proc.stderr
    if stream is None:
        return
    try:
        while await stream.read(65536):
            pass
    except Exception:  # noqa: BLE001 — 进程退出时管道会断，丢掉即可
        return


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

def _allow_outcome(option_id: str) -> Any:
    """`AllowedOutcome` 要两个字段：判别值 `selected` + 选中的 `option_id`。

    只传 `option_id` 会在构造时就 pydantic 校验失败——改这里时注意。
    """
    return acp.schema.AllowedOutcome(outcome="selected", option_id=option_id)


def _deny_outcome() -> Any:
    return acp.schema.DeniedOutcome(outcome="cancelled")


def _option_value(option: Any) -> str:
    """这个选项的机器值：用 ACP 自己的 kind。"""
    return str(getattr(option, "kind", "") or "")


#: 兜底放行认的「允许」类选项（ACP 的 option kind）。
_ALLOW_KINDS = ("allow_once", "allow_always")


def _allow_response(options: list[Any]) -> acp.RequestPermissionResponse:
    """兜底放行：挑一个允许项，没有就拒绝。"""
    allow = next((o for o in options if _option_value(o) in _ALLOW_KINDS), None)
    if allow is None:
        return acp.RequestPermissionResponse(outcome=_deny_outcome())
    return acp.RequestPermissionResponse(outcome=_allow_outcome(getattr(allow, "option_id", "")))


class AcpBody(Executor):
    """一个讲 ACP 的 CLI。`command` 由注册时按 `ACP_CLIS` 填进来。"""

    kind = "cli"
    has_builtin_brain = True
    command: list[str] = []

    @classmethod
    def available(cls) -> bool:
        if not cls.command:
            return False
        if cls.command[0] == "mcode":
            return _mcode_launcher() is not None
        return shutil.which(cls.command[0]) is not None

    async def run(self, req: ExecRequest, provider: Provider | None) -> ExecResult:
        started = time.monotonic()
        text_parts: list[str] = []
        usage = TokenUsage()
        cost_usd: float | None = None
        error: str | None = None

        # 用当前进程环境启动 CLI：登录与用户配置由各 CLI 自己持有。
        env = dict(os.environ)

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
            elif kind == "permission":
                req.emit(
                    "permission",
                    text=data.get("text") or "",
                    tool=data.get("tool") or "",
                    call_id=data.get("call_id") or "",
                    args=data.get("args") if isinstance(data.get("args"), dict) else None,
                    meta={"options": data.get("options") or []},
                )

        client = _AcpClient(on_event)
        session_id = req.session
        try:
            async with acp.spawn_agent_process(
                client, *self.command, cwd=str(req.cwd), env=env
            ) as (conn, proc):
                stderr_task = asyncio.create_task(_discard_stderr(proc))
                try:
                    await conn.initialize(
                        protocol_version=acp.PROTOCOL_VERSION,
                        client_info=acp.schema.Implementation(name="CrewChat", version="1.0.0"),
                    )
                    session_id, _ = await _open_session(conn, req)
                    reply = await conn.prompt(
                        session_id=session_id, prompt=[acp.text_block(req.prompt)]
                    )
                    _accumulate(reply, usage)
                finally:
                    stderr_task.cancel()
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
            },
        )
        register(body)
