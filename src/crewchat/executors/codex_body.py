from __future__ import annotations

import functools
import shutil
import subprocess
import time
from typing import Any

from openai_codex_sdk import (
    AgentMessageItem,
    Codex,
    CommandExecutionItem,
    FileChangeItem,
    ItemCompletedEvent,
    ItemStartedEvent,
    ItemUpdatedEvent,
    McpToolCallItem,
    ReasoningItem,
    Thread,
    ThreadErrorEvent,
    TurnCompletedEvent,
    TurnFailedEvent,
)

from ..config import Settings
from ..providers import Provider
from .base import ExecRequest, ExecResult, Executor, register


@functools.cache
def _codex_wants_experimental_flag() -> bool:
    try:
        proc = subprocess.run(
            ["codex", "exec", "--help"],
            capture_output=True,
            text=True,
            timeout=20,
        )
    except (OSError, subprocess.SubprocessError):
        return False
    return "--experimental-json" in (proc.stdout + proc.stderr)


def _patch_sdk_json_flag() -> None:
    """openai-codex-sdk 0.1.11 still emits `--experimental-json`; codex renamed it `--json`."""
    from openai_codex_sdk.exec import CodexExec

    if getattr(CodexExec._build_command_args, "_crewchat_patched", False):
        return
    original = CodexExec._build_command_args

    @functools.wraps(original)
    def build(self, args):  # type: ignore[no-untyped-def]
        argv = original(self, args)
        if "--experimental-json" in argv:
            argv[argv.index("--experimental-json")] = "--json"
        return argv

    build._crewchat_patched = True  # type: ignore[attr-defined]
    CodexExec._build_command_args = build  # type: ignore[method-assign]


@register
class CodexBody(Executor):
    id = "codex"
    label = "Codex CLI"
    has_builtin_brain = True

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or Settings()
        self._codex: Codex | None = None
        self._threads: dict[str, Thread] = {}

    @classmethod
    def available(cls) -> bool:
        return shutil.which("codex") is not None

    def _client(self) -> Codex:
        if self._codex is None:
            if not _codex_wants_experimental_flag():
                _patch_sdk_json_flag()
            self._codex = Codex()
        return self._codex

    def _thread_options(self, req: ExecRequest, provider: Provider | None) -> dict[str, Any]:
        model = (provider.model if provider else None) or self.settings.codex_model
        provider_id = getattr(provider, "id", None) if provider else None
        opts: dict[str, Any] = {
            "working_directory": str(req.cwd),
            "sandbox_mode": self.settings.codex_sandbox,
            "approval_policy": self.settings.codex_approval,
            "skip_git_repo_check": True,
            "network_access_enabled": self.settings.codex_network,
        }
        if provider_id:
            opts["provider_id"] = provider_id
        if model:
            opts["model"] = model
        return opts

    async def run(self, req: ExecRequest, provider: Provider | None) -> ExecResult:
        started = time.monotonic()
        opts = self._thread_options(req, provider)
        thread = self._threads.get(req.session or "")
        if thread is None:
            thread = self._client().startThread(opts)
            if req.session:
                self._threads[req.session] = thread

        text_parts: list[str] = []
        error: str | None = None
        last_error: str | None = None

        try:
            stream = await thread.runStreamed(req.prompt)
            async for event in stream.events:
                match event:
                    case ItemUpdatedEvent() | ItemCompletedEvent():
                        self._handle_item(req, event.item)
                        if isinstance(event.item, AgentMessageItem) and event.item.text:
                            text_parts.append(event.item.text)
                            req.emit("delta", text=event.item.text)
                    case TurnCompletedEvent():
                        req.emit("tool", status="idle")
                    case TurnFailedEvent() as failed:
                        error = getattr(failed.error, "message", None) or str(failed.error)
                    case ThreadErrorEvent() as thread_error:
                        last_error = thread_error.message
                    case _:
                        pass
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"

        if not text_parts and last_error:
            error = last_error
        text = "".join(text_parts).strip()
        if not text and not error:
            error = "codex produced no output"

        return ExecResult(
            text=text,
            duration_ms=int((time.monotonic() - started) * 1000),
            session=thread.id,
            error=error,
            meta={"body": self.id, "model": opts.get("model", "codex-default")},
        )

    def _handle_item(self, req: ExecRequest, item: Any) -> None:
        match item:
            case ReasoningItem():
                req.emit("thinking", text=item.text)
            case CommandExecutionItem():
                req.emit(
                    "tool",
                    tool=item.command,
                    status=item.status,
                )
            case FileChangeItem():
                changed = ", ".join(
                    str(getattr(change, "path", change)) for change in (item.changes or [])[:3]
                )
                req.emit("tool", tool=f"write {changed}" if changed else "edit files", status=item.status)
            case McpToolCallItem():
                req.emit("tool", tool=f"{item.server}.{item.tool}", status=item.status)
            case _:
                pass

    async def close(self) -> None:
        self._threads.clear()
        self._codex = None