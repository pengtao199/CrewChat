from __future__ import annotations

import time
from typing import Any

from claude_agent_sdk import (
    AssistantMessage,
    ClaudeAgentOptions,
    ClaudeSDKClient,
    ResultMessage,
    StreamEvent,
    SystemMessage,
    TaskProgressMessage,
    TextBlock,
    ThinkingBlock,
    ToolUseBlock,
    UserMessage,
)

from ..core.config import Settings
from ..core.providers import Provider
from .base import ExecRequest, ExecResult, Executor, register


@register
class ClaudeBody(Executor):
    id = "claude"
    label = "Claude Code"
    has_builtin_brain = True

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or Settings()
        self._sessions: dict[str, ClaudeSDKClient] = {}

    @classmethod
    def available(cls) -> bool:
        import shutil

        return shutil.which("claude") is not None

    def _options(self, req: ExecRequest, provider: Provider | None, stderr) -> ClaudeAgentOptions:
        model = (provider.model if provider else None) or self.settings.claude_model
        opts: dict[str, Any] = {
            "cwd": str(req.cwd),
            "add_dirs": [str(req.cwd)],
            "permission_mode": self.settings.claude_permission_mode,
            "include_partial_messages": True,
            "max_turns": self.settings.claude_max_turns,
            "stderr": stderr,
        }
        if model:
            opts["model"] = model
        if req.session:
            opts["resume"] = req.session
            opts["continue_conversation"] = True
        return ClaudeAgentOptions(**opts)

    async def run(self, req: ExecRequest, provider: Provider | None) -> ExecResult:
        started = time.monotonic()
        stderr_sink: list[str] = []
        opts = self._options(req, provider, stderr_sink)
        text_parts: list[str] = []
        result: ResultMessage | None = None
        errors: list[str] = []

        async def consume(client: ClaudeSDKClient) -> None:
            nonlocal result
            await client.query(req.prompt)
            async for msg in client.receive_response():
                match msg:
                    case StreamEvent():
                        req.emit("delta", text=_stream_text(msg))
                    case AssistantMessage():
                        for block in msg.content:
                            match block:
                                case ThinkingBlock():
                                    req.emit("thinking", text=block.thinking)
                                case TextBlock() if block.text:
                                    text_parts.append(block.text)
                                    req.emit("delta", text=block.text)
                                case ToolUseBlock():
                                    req.emit("tool", tool=block.name, status="running", call_id=block.id)
                    case UserMessage():
                        blocks = msg.content if isinstance(msg.content, list) else []
                        for block in blocks:
                            if isinstance(block, ToolUseBlock):
                                req.emit("tool", tool=block.name, status="running", call_id=block.id)
                    case TaskProgressMessage():
                        req.emit("notice", text=str(getattr(msg, "task_id", "")))
                    case SystemMessage() if str(msg.subtype).startswith("error"):
                        errors.append(str(msg.data))
                    case ResultMessage():
                        result = msg
                    case _:
                        pass

        client = self._sessions.get(req.session or "")
        try:
            if client is None:
                async with ClaudeSDKClient(options=opts) as fresh:
                    if req.session:
                        self._sessions[req.session] = fresh
                    await consume(fresh)
            else:
                await consume(client)
        except Exception as exc:
            errors.append(f"{type(exc).__name__}: {exc}")

        tail = "".join(stderr_sink).strip()
        if tail:
            errors.append(f"cli stderr: {tail[-1500:]}")

        text = "".join(text_parts).strip()
        if result is not None and not text:
            text = (result.result or "").strip()

        error = None
        if result is not None and result.is_error:
            error = _result_error(result) or "; ".join(errors) or "claude reported error"
        elif not text and errors:
            error = "; ".join(errors)

        return ExecResult(
            text=text,
            duration_ms=int((time.monotonic() - started) * 1000),
            session=result.session_id if result is not None else req.session,
            cost_usd=result.total_cost_usd if result is not None else None,
            num_turns=result.num_turns if result is not None else None,
            error=error,
            meta={"body": self.id, "model": opts.model or "claude-default"},
        )

    async def close(self) -> None:
        for client in self._sessions.values():
            try:
                await client.disconnect()
            except Exception:
                pass
        self._sessions.clear()


def _stream_text(msg: StreamEvent) -> str:
    event = msg.event if isinstance(msg.event, dict) else {}
    if event.get("type") != "content_block_delta":
        return ""
    delta = event.get("delta") or {}
    return delta.get("text") if delta.get("type") == "text_delta" else ""


def _result_error(result: ResultMessage) -> str:
    parts: list[str] = []
    for err in result.errors or []:
        parts.append(getattr(err, "message", None) or str(err))
    if not parts and result.result:
        parts.append(str(result.result))
    return " | ".join(parts)[:2000]