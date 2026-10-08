from __future__ import annotations

import json
import re
import shutil
import subprocess
from dataclasses import asdict, dataclass, field
from typing import Any
from urllib.parse import urlparse

from .config import Settings
from .providers import load_providers

_TIMEOUT = 20


@dataclass(slots=True)
class Probe:
    id: str
    label: str
    installed: bool = False
    version: str | None = None
    path: str | None = None
    logged_in: bool | None = None
    auth_method: str | None = None
    detail: str | None = None

    @property
    def ready(self) -> bool:
        return self.installed and self.logged_in is not False

    def as_dict(self) -> dict[str, Any]:
        return asdict(self)


def _run(cmd: list[str]) -> tuple[int, str]:
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as exc:
        return 127, str(exc)
    return proc.returncode, (proc.stdout or "") + (proc.stderr or "")


def _cli_version(name: str, args: list[str]) -> str | None:
    code, out = _run([name, *args])
    if code != 0:
        return None
    return out.strip().splitlines()[0] if out.strip() else None


def probe_codex() -> Probe:
    path = shutil.which("codex")
    probe = Probe(id="codex", label="Codex CLI", path=path)
    if not path:
        probe.detail = "未在 PATH 中找到 codex"
        return probe
    probe.installed = True
    probe.version = _cli_version("codex", ["--version"])
    code, out = _run(["codex", "login", "status"])
    probe.logged_in = code == 0
    match = re.search(r"Logged in using (.+?)(?:\s+-|\n|$)", out)
    if match:
        probe.auth_method = _CODEX_AUTH.get(
            match.group(1).strip().lower(),
            match.group(1).strip().removeprefix("an ").strip(),
        )
    elif probe.logged_in:
        probe.auth_method = "Unknown"
    else:
        probe.detail = "未登录 · 运行 `codex login`"
    return probe


def probe_claude() -> Probe:
    path = shutil.which("claude")
    probe = Probe(id="claude", label="Claude Code", path=path)
    if not path:
        probe.detail = "未在 PATH 中找到 claude"
        return probe
    probe.installed = True
    probe.version = _cli_version("claude", ["--version"])
    code, out = _run(["claude", "auth", "status"])
    try:
        data = json.loads(out)
    except ValueError:
        probe.detail = "无法解析 `claude auth status` 输出"
        return probe
    probe.logged_in = bool(data.get("loggedIn"))
    probe.auth_method = _AUTH_LABELS.get(str(data.get("authMethod")))
    if probe.logged_in:
        probe.detail = data.get("apiProvider")
    else:
        probe.detail = "未登录 · 运行 `claude auth login`"
    return probe


def probe_llm(settings: Settings) -> Probe:
    """The llm body is driven by the brain pool, so availability follows the pool
    (any provider with a key), not the legacy single-endpoint settings."""
    pool = load_providers(settings)
    usable = next((p for p in pool if p.has_key()), None)
    base_url = usable.base_url if usable else settings.llm_base_url
    model = usable.model if usable else settings.llm_model
    has_key = usable is not None or bool(settings.llm_api_key.get_secret_value())

    host = urlparse(base_url).hostname or "unknown"
    probe = Probe(
        id="llm",
        label="OpenAI 兼容端点",
        installed=True,
        path=base_url,
        logged_in=has_key,
        auth_method="API key" if has_key else None,
        detail=f"{host} · {provider_name(base_url)}" if has_key else "大脑池里没有带 key 的端点",
    )
    probe.version = f"{model} ({usable.name})" if usable else model
    return probe


_CODEX_AUTH = {
    "an api key": "API key",
    "api key": "API key",
    "chatgpt": "ChatGPT 账号",
}

_AUTH_LABELS = {
    "none": None,
    "oauth_console": "OAuth 登录",
    "api_key": "API key",
    "firstparty": None,
}

_PROVIDER_HINTS: tuple[tuple[str, str], ...] = (
    ("api.openai.com", "OpenAI"),
    ("deepseek", "DeepSeek"),
    ("moonshot", "Moonshot / Kimi"),
    ("dashscope", "阿里云百炼 / 通义"),
    ("siliconflow", "SiliconFlow"),
    ("openrouter", "OpenRouter"),
    ("volces", "火山引擎 / 方舟"),
    ("bigmodel", "智谱 GLM"),
    ("anthropic", "Anthropic"),
    ("localhost", "本地端点"),
    ("127.0.0.1", "本地端点"),
)


def provider_name(base_url: str) -> str:
    host = (urlparse(base_url).hostname or "").lower()
    for needle, name in _PROVIDER_HINTS:
        if needle in host:
            return name
    return "自定义端点"


def provider_options() -> list[tuple[str, str]]:
    return [
        ("https://api.openai.com/v1", "OpenAI"),
        ("https://api.deepseek.com/v1", "DeepSeek"),
        ("https://api.moonshot.cn/v1", "Moonshot / Kimi"),
        ("https://dashscope.aliyuncs.com/compatible-mode/v1", "阿里云百炼"),
        ("https://api.siliconflow.cn/v1", "SiliconFlow"),
        ("https://openrouter.ai/api/v1", "OpenRouter"),
    ]


def snapshot(settings: Settings) -> dict[str, Probe]:
    return {
        "codex": probe_codex(),
        "claude": probe_claude(),
        "llm": probe_llm(settings),
    }


@dataclass(slots=True)
class RuntimeReport:
    probes: dict[str, Probe] = field(default_factory=dict)

    def rows(self) -> list[dict[str, Any]]:
        out: list[dict[str, Any]] = []
        for probe in self.probes.values():
            out.append(
                {
                    "id": probe.id,
                    "label": probe.label,
                    "installed": probe.installed,
                    "version": probe.version,
                    "auth": probe.auth_method,
                    "logged_in": probe.logged_in,
                    "ready": probe.ready,
                    "detail": probe.detail,
                }
            )
        return out
