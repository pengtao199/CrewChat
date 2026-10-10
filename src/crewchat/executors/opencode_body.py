from __future__ import annotations

import functools
import json
import os
import shutil
import time
from pathlib import Path
from typing import Any

from ..core.config import Settings
from ..core.providers import Provider, available_skills, shared_mcp_servers
from .base import ExecRequest, ExecResult, Executor, TokenUsage, _int, register


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


def _mcp_entry(spec: dict[str, Any]) -> dict[str, Any]:
    """`~/.agents/mcp.json` 的一条 → opencode 的 `mcp` 条目。

    opencode 用自己的形状（`type`/`command` 数组/`environment`），和 MCP 生态通用的
    `command`+`args`+`env` 不一样，这里做一次转换。
    """
    url = spec.get("url")
    if url:
        return {"type": "remote", "url": url, "enabled": True}
    command = spec.get("command")
    if not command:
        return {}
    args = [str(a) for a in (spec.get("args") or [])]
    entry: dict[str, Any] = {
        "type": "local",
        "command": [str(command), *args],
        "enabled": True,
    }
    env = spec.get("env") or spec.get("environment")
    if env:
        entry["environment"] = {str(k): str(v) for k, v in env.items()}
    return entry


def _inline_config(provider: Provider | None, skills: list[str], mcps: list[str]) -> str:
    """为这一次运行生成 opencode 配置，**不回退到用户全局配置**。

    以前 opencode 继承 `~/.config/opencode/opencode.jsonc`，于是每个 agent 都要背负
    用户全部的 MCP（codegraph、xcode、npx 拉的一堆……），一次简单回复要 47 秒，而且
    用户改一个字就影响全部 agent。这里只装该 agent 的：它的供应商、它声明的技能与 MCP。
    """
    cfg: dict[str, Any] = {"$schema": "https://opencode.ai/config.json"}

    if provider is not None and provider.id:
        options: dict[str, Any] = {"baseURL": provider.base_url}
        options["apiKey"] = f"{{env:{provider.env_key()}}}"
        models = {mid: {"name": mid} for mid in (provider.model_ids() or [provider.model])}
        cfg["provider"] = {
            provider.id: {
                "npm": "@ai-sdk/openai-compatible",
                "name": provider.name or provider.id,
                "options": options,
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


@functools.cache
def _cli_available() -> bool:
    return shutil.which("opencode") is not None


@register
class OpencodeBody(Executor):
    """`opencode run` 是官方稳定的非交互入口（比 alpha 的 Python SDK 更可靠）：

    - `--format json` 逐事件流式输出
    - `--standalone` 用私有 server，不碰用户全局的后台服务；`opencode` 的后台服务
      固定占一个端口，进程崩溃后会留下占着端口的孤儿，之后每次 `opencode run` 都会
      「等后台服务启动」直到超时。私有 server 把这个共享状态整个绕开——同「自己拥有
      进程」的原则，和网关、claude/codex 的端点归属一致。
    - `--session` 续接会话 = agent 记忆
    - `--model provider/model` 每个 agent 可以用不同大脑
    """

    id = "opencode"
    label = "OpenCode"
    has_builtin_brain = True
    kind = "cli"

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or Settings()
        self.remote_sessions: dict[str, str] = {}

    @classmethod
    def available(cls) -> bool:
        return _cli_available()

    async def run(self, req: ExecRequest, provider: Provider | None) -> ExecResult:
        started = time.monotonic()
        model = _opencode_model(provider, self.settings.opencode_model or None)

        cmd = ["opencode", "run", "--format", "json", "--standalone", "--auto"]
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
        # 这次运行自己的配置，盖过用户全局那份（`OPENCODE_CONFIG_CONTENT` 优先级最高）。
        env["OPENCODE_CONFIG_CONTENT"] = _inline_config(provider, req.skills, req.mcps)
        # 私有 server 不需要联网拉模型表、不需要文件监视、不需要自动更新。
        env.setdefault("OPENCODE_FAST_BOOT", "1")
        env.setdefault("OPENCODE_DISABLE_MODELS_FETCH", "1")
        env.setdefault("OPENCODE_DISABLE_FILEWATCHER", "1")
        env.setdefault("OPENCODE_DISABLE_AUTOUPDATE", "1")

        text_parts: list[str] = []
        tools: list[str] = []
        usage = TokenUsage()
        cost_usd: float | None = None
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
                kind, payload, call_id = event
                if kind == "text" and payload:
                    text_parts.append(payload)
                    req.emit("delta", text=payload)
                elif kind == "tool" and payload:
                    tools.append(payload)
                    req.emit("tool", tool=payload, status="completed", call_id=call_id)
                elif kind == "usage" and isinstance(payload, dict):
                    _accumulate_usage(usage, payload)
                    if payload.get("cost") is not None:
                        cost_usd = (cost_usd or 0.0) + float(payload["cost"] or 0.0)
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
            cost_usd=cost_usd,
            usage=None if usage.is_empty() else usage,
            error=error,
            meta={"body": self.id, "model": model or "opencode-default", "tools": str(len(tools))},
        )


def _accumulate_usage(usage: TokenUsage, payload: dict[str, Any]) -> None:
    """opencode 的 `step-finish` 分步上报，一次运行可能多步，累加。"""
    tokens = payload.get("tokens") or {}
    cache = tokens.get("cache") or {}
    usage.input_tokens += _int(tokens.get("input"))
    usage.output_tokens += _int(tokens.get("output"))
    usage.reasoning_tokens += _int(tokens.get("reasoning"))
    usage.cache_read_tokens += _int(cache.get("read"))
    usage.cache_creation_tokens += _int(cache.get("write"))


def _parse(line: str) -> tuple[str, Any, str] | None:
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

    part_id = str(part.get("id") or "")

    if ptype in {"step-finish", "step_finish"} or data.get("type") == "step_finish":
        return "usage", {"cost": part.get("cost"), "tokens": part.get("tokens") or {}}, ""
    if ptype == "text":
        return "text", str(part.get("text") or ""), ""
    if ptype in {"tool", "tool-invocation", "tool-use"}:
        name = part.get("tool") or part.get("name") or part.get("toolName") or ptype
        state = part.get("state") or {}
        status = "completed" if state.get("status") in {"completed", "success"} else "in_progress"
        return "tool", f"{name}", part_id
    if ptype in {"error", "step-error"}:
        detail = part.get("error") or part.get("message") or data.get("error") or data.get("message")
        return "error", _msg(detail), ""
    if data.get("type") == "error":
        return "error", _msg(data.get("error") or data.get("message")), ""
    sid = data.get("sessionID")
    if sid:
        return "session", str(sid), ""
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