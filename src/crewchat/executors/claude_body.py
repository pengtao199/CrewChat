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
from .base import ExecRequest, ExecResult, Executor, TokenUsage, _int, register


def _pick(obj: Any, key: str) -> Any:
    return obj.get(key) if isinstance(obj, dict) else getattr(obj, key, None)


def _usage_from_result(result: ResultMessage) -> TokenUsage:
    """claude 的 per-model 用量优先（camelCase + costUSD）；退化到聚合 usage（snake）。"""
    usage = TokenUsage()
    for entry in (result.model_usage or {}).values():
        usage.input_tokens += _int(_pick(entry, "inputTokens"))
        usage.output_tokens += _int(_pick(entry, "outputTokens"))
        usage.cache_read_tokens += _int(_pick(entry, "cacheReadInputTokens"))
        usage.cache_creation_tokens += _int(_pick(entry, "cacheCreationInputTokens"))
    if usage.is_empty() and result.usage:
        usage.input_tokens = _int(result.usage.get("input_tokens"))
        usage.output_tokens = _int(result.usage.get("output_tokens"))
        usage.cache_read_tokens = _int(result.usage.get("cache_read_input_tokens"))
        usage.cache_creation_tokens = _int(result.usage.get("cache_creation_input_tokens"))
    return usage


@register
class ClaudeBody(Executor):
    id = "claude"
    label = "Claude Code"
    has_builtin_brain = True
    kind = "cli"
    needs_gateway = True

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or Settings()

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
            # Resume by id only. Adding `continue_conversation` here makes the
            # CLI emit `--continue` alongside `--resume`, and `--continue` wins:
            # the run silently attaches to the most recent session instead of
            # this conversation's, so every conversation would share one thread.
            opts["resume"] = req.session
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
            # With include_partial_messages the CLI sends the same reply twice:
            # incremental StreamEvents and then the assembled AssistantMessage.
            # Emitting both repeats every token, so text comes from exactly one
            # source — partials when they arrive, otherwise the final message.
            streamed_text = False
            await client.query(req.prompt)
            async for msg in client.receive_response():
                match msg:
                    case StreamEvent():
                        delta = _stream_text(msg)
                        if delta:
                            streamed_text = True
                            text_parts.append(delta)
                            req.emit("delta", text=delta)
                    case AssistantMessage():
                        for block in msg.content:
                            match block:
                                case ThinkingBlock():
                                    req.emit("thinking", text=block.thinking)
                                case TextBlock() if block.text:
                                    if streamed_text:
                                        continue
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

        # One client per run. A cached client cannot work here: it would have to
        # outlive the `async with` that closes it, and executors are constructed
        # per turn anyway, so the cache could only ever hand back a closed client.
        try:
            async with ClaudeSDKClient(options=opts) as client:
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

        usage = _usage_from_result(result) if result is not None else TokenUsage()
        return ExecResult(
            text=text,
            duration_ms=int((time.monotonic() - started) * 1000),
            session=result.session_id if result is not None else req.session,
            cost_usd=result.total_cost_usd if result is not None else None,
            num_turns=result.num_turns if result is not None else None,
            usage=None if usage.is_empty() else usage,
            error=error,
            meta={"body": self.id, "model": opts.model or "claude-default"},
        )

    async def close(self) -> None:
        """No long-lived clients: each run opens and closes its own."""
        return None


def _stream_text(msg: StreamEvent) -> str:
    """Visible text only.

    Thinking arrives as thinking_delta on the same channel; it is deliberately
    dropped here so it cannot leak into the reply text (it is emitted separately
    from ThinkingBlock).
    """
    event = msg.event if isinstance(msg.event, dict) else {}
    if event.get("type") != "content_block_delta":
        return ""
    delta = event.get("delta") or {}
    return delta.get("text") or "" if delta.get("type") == "text_delta" else ""


def _result_error(result: ResultMessage) -> str:
    parts: list[str] = []
    for err in result.errors or []:
        parts.append(getattr(err, "message", None) or str(err))
    if not parts and result.result:
        parts.append(str(result.result))
    return " | ".join(parts)[:2000]