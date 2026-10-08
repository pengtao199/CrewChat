from __future__ import annotations

import functools
import json
import os
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any

from ..config import Settings
from ..providers import Provider
from .base import ExecRequest, ExecResult, Executor, register


def _opencode_model(provider: Provider | None, raw: str | None) -> str:
    """opencode 用 `provider/model#variant` 命名；显式给了就用，否则用 CLI 自己的配置。"""
    if raw and "/" in raw:
        return raw
    if provider is None:
        return raw or ""
    name = provider.model
    if "#" in name:
        model, variant = name.split("#", 1)
        return f"{provider.id}/{model}#{variant}"
    return f"{provider.id}/{name}" if provider.id else name


@functools.cache
def _cli_available() -> bool:
    return shutil.which("opencode") is not None


@register
class OpencodeBody(Executor):
    """`opencode run` 是官方稳定的非交互入口（比 alpha 的 Python SDK 更可靠）：

    - `--format json` 逐事件流式输出
    - `--session` 续接会话 = agent 记忆
    - `--model provider/model` 每个 agent 可以用不同大脑
    """

    id = "opencode"
    label = "OpenCode"
    has_builtin_brain = True

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or Settings()
        self.remote_sessions: dict[str, str] = {}

    @classmethod
    def available(cls) -> bool:
        return _cli_available()

    async def run(self, req: ExecRequest, provider: Provider | None) -> ExecResult:
        started = time.monotonic()
        model = _opencode_model(provider, self.settings.opencode_model or None)

        cmd = ["opencode", "run", "--format", "json", "--auto"]
        remote = self.remote_sessions.get(req.session or "")
        if remote:
            cmd += ["--session", remote]
        if model:
            cmd += ["--model", model]
        cmd.append(req.prompt)

        env = dict(os.environ)
        if provider is not None:
            if provider.api_key:
                env.setdefault("OPENCODE_API_KEY", provider.api_key)
                env.setdefault(provider.env_key(), provider.api_key)
            env.setdefault("OPENCODE_BASE_URL", provider.base_url)

        text_parts: list[str] = []
        tools: list[str] = []
        error: str | None = None
        session_id = req.session

        stderr_tail: list[str] = []
        try:
            proc = await _spawn(cmd, req, env)
            assert proc.stdout is not None and proc.stderr is not None
            async for raw in proc.stdout:
                line = raw.decode("utf-8", errors="replace")
                event = _parse(line)
                if event is None:
                    continue
                kind, payload = event
                if kind == "text" and payload:
                    text_parts.append(payload)
                    req.emit("delta", text=payload)
                elif kind == "tool" and payload:
                    tools.append(payload)
                    req.emit("tool", tool=payload, status="completed")
                elif kind == "session" and payload:
                    session_id = payload
                    if req.session:
                        self.remote_sessions[req.session] = payload
                elif kind == "error" and payload:
                    error = payload
            code = await proc.wait()
            err_text = (await proc.stderr.read()).decode("utf-8", errors="replace").strip()
            if code != 0 and not error:
                error = err_text[-1500:] or f"opencode exited with code {code}"
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"

        return ExecResult(
            text="".join(text_parts).strip(),
            exit_code=0 if not error else 1,
            duration_ms=int((time.monotonic() - started) * 1000),
            session=session_id,
            error=error,
            meta={"body": self.id, "model": model or "opencode-default", "tools": str(len(tools))},
        )


def _parse(line: str) -> tuple[str, str] | None:
    line = line.strip()
    if not line.startswith("{"):
        return None
    try:
        data = json.loads(line)
    except ValueError:
        return None
    if not isinstance(data, dict):
        return None

    part = data.get("part") or {}
    ptype = part.get("type") or data.get("type")

    if ptype == "text":
        return "text", str(part.get("text") or "")
    if ptype in {"tool", "tool-invocation", "tool-use"}:
        name = part.get("tool") or part.get("name") or part.get("toolName") or ptype
        state = part.get("state") or {}
        status = "completed" if state.get("status") in {"completed", "success"} else "in_progress"
        return "tool", f"{name}"
    if ptype in {"error", "step-error"}:
        detail = part.get("error") or part.get("message") or data.get("error") or data.get("message")
        return "error", _msg(detail)
    if data.get("type") == "error":
        return "error", _msg(data.get("error") or data.get("message"))
    sid = data.get("sessionID")
    if sid:
        return "session", str(sid)
    return None



def _msg(detail: Any) -> str:
    if isinstance(detail, dict):
        return str(detail.get("message") or detail)
    return str(detail or "opencode error")


async def _spawn(cmd: list[str], req: ExecRequest, env: dict[str, str]):  # type: ignore[no-untyped-def]
    import asyncio

    proc = await asyncio.create_subprocess_exec(
        *cmd,
        cwd=str(req.cwd),
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        env=env,
    )
    return proc