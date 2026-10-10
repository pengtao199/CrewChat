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
import signal
import socket
import subprocess
import tempfile
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import acp

from ..core.config import Settings
from ..core.providers import Provider, available_skills, shared_mcp_servers
from .base import ExecRequest, ExecResult, Executor, TokenUsage, register


# ---- 每个 CLI 的私有部分，只剩"怎么把它拉起来" ------------------------------

#: 反代认的占位凭据——客户端发这个，cc-switch 在转发时换成真实 key。身体因此永远拿不到
#: 明文 key：真相源只在网关那一侧。
PROXY_TOKEN = "PROXY_MANAGED"


def _proxy_base() -> str:
    s = Settings()
    return f"http://127.0.0.1:{s.gateway_proxy_port}/v1"


def _proxy_base_with_app(app_id: str = "") -> str:
    """带 app 段的反代地址：`http://127.0.0.1:15721/<app>/v1`。

    反代按路径段确定请求属于哪个 app（`chat_app_of`），路由到该 app 的当前供应商。
    """
    s = Settings()
    base = f"http://127.0.0.1:{s.gateway_proxy_port}"
    return f"{base}/{app_id}/v1" if app_id else f"{base}/v1"


def _cli_homes_root() -> Path:
    root = Settings().data_dir / "cli-homes"
    root.mkdir(parents=True, exist_ok=True)
    return root


def _fresh_home(kind: str) -> Path:
    return Path(tempfile.mkdtemp(prefix=f"{kind}-", dir=_cli_homes_root()))


def _release_homes(env: dict[str, str]) -> None:
    """删掉这一次进程的临时配置目录。

    只动 `cli-homes`，以及 Hermes 放在 `~/.hermes/crewchat-sessions` 下的那一层。
    """
    _stop_openclaw_gateway(env)
    root = _cli_homes_root().resolve()
    hermes_root = (Path.home() / ".hermes" / "crewchat-sessions").resolve()
    for key in (
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "HERMES_HOME",
        "PI_CODING_AGENT_DIR",
        "MINIMAX_DATA_DIR",
        "GROK_HOME",
        "OPENCLAW_STATE_DIR",
        "OPENCODE_TEST_HOME",
        "XDG_DATA_HOME",
    ):
        raw = env.get(key)
        if not raw:
            continue
        path = Path(raw).resolve()
        under_cli = path.is_relative_to(root)
        under_hermes = key == "HERMES_HOME" and path != hermes_root and path.is_relative_to(hermes_root)
        if under_cli or under_hermes:
            shutil.rmtree(path, ignore_errors=True)


def _scrub_secret(env: dict[str, str], secret: str) -> None:
    """子进程环境里不留真实 key。父进程已经导出的同值也去掉。"""
    if not secret:
        return
    for key in list(env):
        if env[key] == secret:
            del env[key]


def _toml_string(value: str) -> str:
    return value.replace("\\", "\\\\").replace('"', '\\"')


def _opencode_env(provider: Provider | None, req: ExecRequest, env: dict[str, str]) -> None:
    """这一次 OpenCode 用自己的 home，不读用户的 `~/.config/opencode`。

    OpenCode 2 把供应商写在 `providers`（`package` + `settings`）。目录键不能含 `/`，
    发给反代的模型名放在 `modelID`。`options.apiKey` 是旧格式，这版会丢掉。

    `OPENCODE_TEST_HOME` 换掉 home，这样不会带上用户的 MCP 和已登录供应商。
    `OPENCODE_CONFIG_DIR` 指向这次的 `opencode.json`。凭据只有占位符。

    同时清掉父进程的 `OPENCODE_*`（桌面会话会设 `OPENCODE_SERVER_PASSWORD` 等），
    否则 ACP 会连到父进程的服务器，而不是自己启动。
    """
    for key in list(env.keys()):
        if key.startswith("OPENCODE_"):
            del env[key]
    home = _fresh_home("opencode")
    config_dir = home / "config"
    data = home / "data"
    config_dir.mkdir(parents=True)
    (data / "opencode").mkdir(parents=True)
    content = _inline_config(provider, req.skills, req.mcps, req.app_id)
    (config_dir / "opencode.json").write_text(content, encoding="utf-8")
    (data / "opencode" / "auth.json").write_text(
        json.dumps({"gw": {"type": "api", "key": PROXY_TOKEN}}),
        encoding="utf-8",
    )
    env["OPENCODE_TEST_HOME"] = str(home)
    env["OPENCODE_CONFIG_DIR"] = str(config_dir)
    env["XDG_DATA_HOME"] = str(data)
    env["OPENCODE_CONFIG_CONTENT"] = content
    env["OPENCODE_DISABLE_PROJECT_CONFIG"] = "1"
    env["OPENCODE_FAST_BOOT"] = "1"
    env["OPENCODE_DISABLE_MODELS_FETCH"] = "1"
    env["OPENCODE_DISABLE_FILEWATCHER"] = "1"
    env["OPENCODE_DISABLE_AUTOUPDATE"] = "1"


def _claude_env(provider: Provider | None, req: ExecRequest, env: dict[str, str]) -> None:
    """这一次 Claude 进程用自己的 `CLAUDE_CONFIG_DIR`，不写用户的 `~/.claude`。

    `provider.model` 已是发布 id（`ccs-claude-…`）。凭据只放占位符，真实 key 留在网关。
    """
    del req  # 配置不靠 app 段：Claude 走 `/v1/messages`，反代按 Claude 处理。
    home = _fresh_home("claude")
    base = _proxy_base().removesuffix("/v1")
    model = provider.model if provider is not None and provider.model else ""
    settings = {
        "env": {
            "ANTHROPIC_BASE_URL": base,
            "ANTHROPIC_AUTH_TOKEN": PROXY_TOKEN,
            "ANTHROPIC_MODEL": model,
        }
    }
    (home / "settings.json").write_text(json.dumps(settings), encoding="utf-8")
    env["CLAUDE_CONFIG_DIR"] = str(home)
    env["ANTHROPIC_BASE_URL"] = base
    env["ANTHROPIC_AUTH_TOKEN"] = PROXY_TOKEN
    if model:
        env["ANTHROPIC_MODEL"] = model


def _codex_env(provider: Provider | None, req: ExecRequest, env: dict[str, str]) -> None:
    """这一次 Codex 进程用自己的 `CODEX_HOME`，不写用户的 `~/.codex`。

    `config.toml` 的 `model` 是发布 id（`ccs-<key>/<model>`），`base_url` 指反代 `/v1`
    （Responses）。凭据是占位符，转发时由网关换成真实 key。
    """
    del req
    home = _fresh_home("codex")
    model = provider.model if provider is not None and provider.model else ""
    toml = (
        f'model = "{_toml_string(model)}"\n'
        'model_provider = "custom"\n'
        "\n"
        "[model_providers.custom]\n"
        'name = "CrewChat"\n'
        f'base_url = "{_proxy_base()}"\n'
        'wire_api = "responses"\n'
        f'experimental_bearer_token = "{PROXY_TOKEN}"\n'
        "requires_openai_auth = false\n"
    )
    (home / "config.toml").write_text(toml, encoding="utf-8")
    env["CODEX_HOME"] = str(home)


def _model_of(provider: Provider | None) -> str:
    return provider.model if provider is not None and provider.model else ""


def _yaml_quote(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _gemini_env(provider: Provider | None, req: ExecRequest, env: dict[str, str]) -> None:
    """Gemini 的模型名在环境变量里，每个进程一份，不写 `~/.gemini`。

    发布 id 不含 `/`（`ccs-gemini-<key>--<model>`），才能放进 URL 路径段。
    """
    del req
    env["GOOGLE_GEMINI_BASE_URL"] = _proxy_base().removesuffix("/v1")
    env["GEMINI_API_KEY"] = PROXY_TOKEN
    model = _model_of(provider)
    if model:
        env["GEMINI_MODEL"] = model


def _grok_env(provider: Provider | None, req: ExecRequest, env: dict[str, str]) -> None:
    """这一次 Grok 进程用自己的 `GROK_HOME`，不写用户的 `~/.grok`。"""
    del req
    home = _fresh_home("grok")
    model = _model_of(provider)
    quoted = _toml_string(model)
    base = _proxy_base_with_app("grokbuild")
    text = (
        "[models]\n"
        f'default = "{quoted}"\n\n'
        f'[model."{quoted}"]\n'
        f'model = "{quoted}"\n'
        'name = "CrewChat"\n'
        f'base_url = "{base}"\n'
        f'api_key = "{PROXY_TOKEN}"\n'
        'api_backend = "chat_completions"\n'
        "context_window = 500000\n"
    )
    (home / "config.toml").write_text(text, encoding="utf-8")
    env["GROK_HOME"] = str(home)
    env["GROK_CONFIG_PATH"] = str(home / "config.toml")
    if model:
        env["GROK_DEFAULT_MODEL"] = model


def _hermes_sessions_root() -> Path:
    """临时 Hermes 家必须放在 `~/.hermes` 里面，才会和已安装的依赖共用同一份数据根。

    放在外面时，Hermes 把这次家当成另一份安装，启动就要重装依赖。`crewchat-sessions`
    不是 `profiles/`，不会出现在 Hermes 的 profile 列表里。
    """
    root = Path.home() / ".hermes" / "crewchat-sessions"
    root.mkdir(parents=True, exist_ok=True)
    return root


def _hermes_env(provider: Provider | None, req: ExecRequest, env: dict[str, str]) -> None:
    """这一次 Hermes 进程用自己的 `HERMES_HOME`。不改用户的 `~/.hermes/config.yaml`。"""
    del req
    home = Path(tempfile.mkdtemp(prefix="hermes-", dir=_hermes_sessions_root()))
    model = _model_of(provider)
    base = _proxy_base_with_app("hermes")
    text = (
        "model:\n"
        f"  default: {_yaml_quote(model)}\n"
        "  provider: gw\n"
        f"  base_url: {_yaml_quote(base)}\n"
        "  api_mode: chat_completions\n"
        "custom_providers:\n"
        "  - name: gw\n"
        f"    base_url: {_yaml_quote(base)}\n"
        f"    api_key: {PROXY_TOKEN}\n"
        f"    model: {_yaml_quote(model)}\n"
        "    discover_models: false\n"
    )
    (home / "config.yaml").write_text(text, encoding="utf-8")
    env["HERMES_HOME"] = str(home)


def _pi_env(provider: Provider | None, req: ExecRequest, env: dict[str, str]) -> None:
    """这一次 Pi 进程用自己的 `PI_CODING_AGENT_DIR`。

    自定义模型必须带 `api`（`openai-completions`）。缺了它，`modelFromJson` 直接抛掉，
    可用模型列表是空的，pi-acp 就把这报成「要 API key 或 OAuth」。`models.json` 里的
    `apiKey` 就是这版 Pi 认的凭据，不另写 `auth.json`。`quietStartup` 关掉开场白，
    否则技能清单会作为第一条助手消息发出来。
    """
    del req
    home = _fresh_home("pi")
    model = _model_of(provider)
    base = _proxy_base_with_app("pi")
    (home / "settings.json").write_text(
        json.dumps(
            {"defaultProvider": "gw", "defaultModel": model, "quietStartup": True}
        ),
        encoding="utf-8",
    )
    (home / "models.json").write_text(
        json.dumps(
            {
                "providers": {
                    "gw": {
                        "baseUrl": base,
                        "api": "openai-completions",
                        "apiKey": PROXY_TOKEN,
                        "models": [{"id": model, "name": model or "gw"}],
                    }
                }
            }
        ),
        encoding="utf-8",
    )
    env["PI_CODING_AGENT_DIR"] = str(home)


def _mcode_launcher() -> Path | None:
    """官方安装器把入口放在 `~/.minimax-code/bin`，不改用户的 shell PATH。"""
    found = shutil.which("mcode")
    if found:
        return Path(found)
    candidate = Path.home() / ".minimax-code" / "bin" / "mcode"
    return candidate if os.access(candidate, os.X_OK) else None


def _mcode_env(provider: Provider | None, req: ExecRequest, env: dict[str, str]) -> None:
    """这一次 Mcode 进程用自己的 `MINIMAX_DATA_DIR`。"""
    del req
    launcher = _mcode_launcher()
    if launcher is not None:
        bin_dir = str(launcher.parent)
        current = env.get("PATH", "")
        if bin_dir not in current.split(os.pathsep):
            env["PATH"] = bin_dir + os.pathsep + current
    home = _fresh_home("mcode")
    model = _model_of(provider)
    base = _proxy_base_with_app("mcode")
    text = (
        f"defaultModel: {_yaml_quote(f'custom_provider:gw/{model}')}\n"
        "custom_provider:\n"
        "  gw:\n"
        "    name: gw\n"
        "    kind: custom\n"
        "    enabled: true\n"
        "    api: openai-completions\n"
        "    options:\n"
        f"      baseURL: {_yaml_quote(base)}\n"
        f"      apiKey: {PROXY_TOKEN}\n"
        "    models:\n"
        f"      {_yaml_quote(model)}:\n"
        f"        name: {_yaml_quote(model)}\n"
    )
    (home / "config.yaml").write_text(text, encoding="utf-8")
    env["MINIMAX_DATA_DIR"] = str(home)


def _openclaw_env(provider: Provider | None, req: ExecRequest, env: dict[str, str]) -> None:
    """这一次 OpenClaw 进程用自己的 `OPENCLAW_STATE_DIR`，不写 `~/.openclaw`。

    `openclaw acp` 自己不调模型，它连本机的 OpenClaw Gateway。Gateway 用这份配置里的
    `apiKey`（占位符）去打反代。进程由 `run` 拉起，退出时关掉。
    """
    del req
    home = _fresh_home("openclaw")
    model = _model_of(provider)
    base = _proxy_base_with_app("openclaw")
    cfg = {
        "models": {
            "mode": "replace",
            "providers": {
                "gw": {
                    "baseUrl": base,
                    "apiKey": PROXY_TOKEN,
                    "api": "openai-completions",
                    "models": [{"id": model, "name": model or "gw"}],
                }
            },
        },
        "agents": {"defaults": {"model": {"primary": f"gw/{model}" if model else "gw"}}},
        "gateway": {"mode": "local", "bind": "loopback", "auth": {"mode": "none"}},
    }
    path = home / "openclaw.json"
    path.write_text(json.dumps(cfg), encoding="utf-8")
    env["OPENCLAW_STATE_DIR"] = str(home)
    env["OPENCLAW_CONFIG_PATH"] = str(path)


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _wait_port(port: int, timeout: float = 8) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.1)
    raise TimeoutError(f"OpenClaw gateway did not listen on {port}")


def _ensure_openclaw_gateway(env: dict[str, str]) -> None:
    """拉起这一次自己的 OpenClaw Gateway。`openclaw acp` 只是连它的桥。"""
    raw = env.get("OPENCLAW_CONFIG_PATH")
    if not raw or env.get("OPENCLAW_GATEWAY_PID"):
        return
    path = Path(raw)
    cfg = json.loads(path.read_text(encoding="utf-8"))
    port = _free_port()
    gateway = cfg.setdefault("gateway", {})
    gateway.update({"mode": "local", "bind": "loopback", "port": port, "auth": {"mode": "none"}})
    path.write_text(json.dumps(cfg), encoding="utf-8")
    proc = subprocess.Popen(
        [
            "openclaw",
            "gateway",
            "--port",
            str(port),
            "--auth",
            "none",
            "--bind",
            "loopback",
            "--allow-unconfigured",
        ],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    env["OPENCLAW_GATEWAY_PID"] = str(proc.pid)
    try:
        _wait_port(port, timeout=20)
    except Exception:
        _stop_openclaw_gateway(env)
        raise


def _stop_openclaw_gateway(env: dict[str, str]) -> None:
    pid = env.pop("OPENCLAW_GATEWAY_PID", "")
    if not pid.isdigit():
        return
    try:
        os.kill(int(pid), signal.SIGTERM)
    except OSError:
        return


#: 每个 CLI 一次进程一份配置，模型名是这次的发布 id。两个会话因此可以同时用两家供应商。
ACP_CLIS: dict[str, dict[str, Any]] = {
    "opencode": {"label": "OpenCode", "command": ["opencode", "acp"], "needs_gateway": True, "env": _opencode_env},
    "openclaw": {"label": "OpenClaw", "command": ["openclaw", "acp"], "needs_gateway": True, "env": _openclaw_env},
    "claude": {"label": "Claude Code", "command": ["claude-agent-acp"], "needs_gateway": True, "env": _claude_env},
    "codex": {"label": "Codex CLI", "command": ["codex-acp"], "needs_gateway": True, "env": _codex_env},
    "gemini": {"label": "Gemini CLI", "command": ["gemini", "--acp"], "needs_gateway": True, "env": _gemini_env},
    "grok": {"label": "Grok Build", "command": ["grok", "agent", "stdio"], "needs_gateway": True, "env": _grok_env},
    "hermes": {"label": "Hermes Agent", "command": ["hermes", "acp"], "needs_gateway": True, "env": _hermes_env},
    "pi": {"label": "Pi", "command": ["pi-acp"], "needs_gateway": True, "env": _pi_env},
    "mcode": {"label": "MiniMax Code", "command": ["mcode", "acp"], "needs_gateway": True, "env": _mcode_env},
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



def _opencode_wire_model(model: str) -> tuple[str, str]:
    """目录名固定为 `main`，发给网关的是这次的发布 id。

    发布 id 里有 `/`，不能当目录键。`modelID` 才是请求体里的 `model`。
    两个 OpenCode 进程各写各的 `modelID`，就能同时用两家供应商。
    """
    return "main", model


def _inline_config(provider: Provider | None, skills: list[str], mcps: list[str], app_id: str = "") -> str:
    """为这一次运行生成 opencode 配置，不继承用户全局配置。

    provider 名固定为 `gw`。OpenCode 2 读 `providers.*.settings`，目录键是 `main`，
    `modelID` 是这次的发布 id。反代按这个 id 选供应商。
    """
    cfg: dict[str, Any] = {"$schema": "https://opencode.ai/config.json"}

    if provider is not None and provider.model:
        short, wire = _opencode_wire_model(provider.model)
        cfg["model"] = f"gw/{short}"
        cfg["providers"] = {
            "gw": {
                "name": provider.name or "gw",
                "package": "aisdk:@ai-sdk/openai-compatible",
                "settings": {
                    "baseURL": _proxy_base_with_app(app_id or "opencode"),
                    "apiKey": PROXY_TOKEN,
                },
                "models": {short: {"name": short, "modelID": wire}},
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

class AcpBody(Executor):
    """一个讲 ACP 的 CLI。`command` 由注册时按 `ACP_CLIS` 填进来。"""

    kind = "cli"
    has_builtin_brain = True
    needs_gateway = False
    command: list[str] = []
    env_hook: Callable[[Provider | None, ExecRequest, dict[str, str]], None] | None = None

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

        env = dict(os.environ)
        if provider is not None:
            for key in provider.keys:
                if key.secret:
                    _scrub_secret(env, key.secret)
        if self.env_hook is not None:
            self.env_hook(provider, req, env)
        if self.id == "openclaw":
            _ensure_openclaw_gateway(env)

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
            ) as (conn, proc):
                stderr_task = asyncio.create_task(_discard_stderr(proc))
                try:
                    await conn.initialize(
                        protocol_version=acp.PROTOCOL_VERSION,
                        client_info=acp.schema.Implementation(name="CrewChat", version="1.0.0"),
                    )
                    if self.id == "claude":
                        # 这版 claude-agent-acp 不读 ANTHROPIC_BASE_URL。不调用 gateway
                        # authenticate，查询会走官方地址（日志里的 baseUrl=native）。
                        await conn.authenticate(
                            "gateway",
                            gateway={
                                "baseUrl": _proxy_base().removesuffix("/v1"),
                                "headers": {},
                            },
                        )
                    session_id, config_options = await _open_session(conn, req)
                    # ACP 把"用哪个模型"也做成一个配置项。agent 默认用它自己的那家
                    # （opencode 默认 openrouter），不选就会跑到别人的额度上。按该 agent
                    # 的大脑选出对应项——这一步对任何讲 ACP 的 CLI 都一样。
                    await _select_model(conn, session_id, config_options, provider)
                    reply = await conn.prompt(
                        session_id=session_id, prompt=[acp.text_block(req.prompt)]
                    )
                    _accumulate(reply, usage)
                finally:
                    stderr_task.cancel()
        except Exception as exc:  # noqa: BLE001 — 任何失败都要变成一次可读的 error
            error = f"{type(exc).__name__}: {exc}"
        finally:
            _release_homes(env)

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
    openrouter）。不选，请求就落到别人的额度上。候选值按优先级：`gw/<短名>`
    （OpenCode 内联目录）、`<provider>/<短名>`、发布 id。
    """
    if provider is None or not provider.model:
        return
    short, _wire = _opencode_wire_model(provider.model)
    candidates = [
        f"gw/{short}",
        f"{provider.id}/{short}",
        f"{provider.id}/{provider.model}",
        provider.model,
    ]
    for option in config_options or []:
        values = _select_values(option)
        chosen = next((c for c in candidates if c in values), None)
        if chosen:
            try:
                await conn.set_config_option(
                    config_id=getattr(option, "id", "model"), session_id=session_id, value=chosen
                )
            except Exception:  # noqa: BLE001 — 确认失败时沿用进程里已经写好的模型
                return
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
                "needs_gateway": bool(spec.get("needs_gateway", False)),
                # staticmethod：否则取 `self.env_hook` 会把它当方法绑定，多传一个 self。
                "env_hook": staticmethod(spec["env"]) if spec.get("env") else None,
            },
        )
        register(body)
