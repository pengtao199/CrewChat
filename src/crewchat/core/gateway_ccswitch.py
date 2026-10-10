"""cc-switch 适配器：用它的代理核心实现 `ModelGateway`。

复用 cc-switch 的核心（`ccsw-proxy` 进程 + 其 SQLite），但数据归我们：

- 网关的 home 由 `Settings.gateway_dir` 指定（默认 `.data/gateway/`），经
  `CC_SWITCH_HOME` 注入；**绝不写用户本机的 `~/.cc-switch/`**。
- 网关库里的 provider 是**派生物**——由通用 provider 投影而来，可重建。

cc-switch 没有"通用 provider"概念（严格按 app_type 分），所以 `activate` 分两步：
把通用 provider 投影成该 CLI 的一条，再登记 Stack。进程配置由身体写，不改用户全局配置。
"""

from __future__ import annotations

import json
import os
import shutil
import sqlite3
import subprocess
import time
from contextlib import closing
from pathlib import Path
from typing import Any

from .config import Settings, get_settings
from .providers import Provider

# cc-switch 认识的全部 app（与它的 VALID_TOOLS 一致）。每个身体都要在网关里成为
# 一条 provider 行，经反代逐请求路由——身体本身只拿发布 id，不拿明文 key。
SUPPORTED_APPS = (
    "claude",
    "codex",
    "gemini",
    "grok",
    "opencode",
    "openclaw",
    "hermes",
    "pi",
    "mcode",
)


def _binary(settings: Settings) -> Path | None:
    configured = settings.gateway_binary
    if configured.is_file():
        return configured
    found = shutil.which("ccsw-proxy")
    return Path(found) if found else None


def _db_path(settings: Settings) -> Path:
    return settings.gateway_dir / "cc-switch.db"


def _build_settings_config(app_type: str, provider: Provider) -> tuple[dict[str, Any], dict[str, Any]]:
    """把一个通用 Provider 编译成某 app_type 的 `(settings_config, meta)`。

    形状依据 cc-switch 自身的约定（参考其库里的既有条目与前端预设）。
    """
    model = provider.model or (provider.models[0] if provider.models else "")
    base_url = provider.base_url
    # 多账号时只用默认那一把，不把备注和其余 Key 拼进子进程。
    api_key = provider.default_secret()
    if app_type == "codex":
        config = (
            'model_provider = "custom"\n'
            f'model = "{model}"\n\n'
            "[model_providers.custom]\n"
            'name = "custom"\n'
            'wire_api = "responses"\n'
            "requires_openai_auth = false\n"
            f'base_url = "{base_url}"\n'
        )
        return (
            {"auth": {"OPENAI_API_KEY": api_key}, "config": config},
            {"apiFormat": provider.api_format},
        )
    if app_type == "claude":
        env = {"ANTHROPIC_BASE_URL": base_url}
        if api_key:
            env["ANTHROPIC_AUTH_TOKEN"] = api_key
            env["ANTHROPIC_API_KEY"] = api_key
        return ({"env": env}, {"apiFormat": provider.api_format})
    if app_type == "opencode":
        return _openai_compatible(provider)
    if app_type in _GENERIC_OPENAI_APPS:
        return _openai_compatible(provider)
    if app_type == "gemini":
        env = {"GOOGLE_GEMINI_BASE_URL": base_url}
        if api_key:
            env["GEMINI_API_KEY"] = api_key
        return ({"env": env}, {"apiFormat": provider.api_format})
    if app_type in ("grok", "grokbuild"):
        models = provider.model_ids() or ([model] if model else [])
        return (
            {
                "name": provider.name or provider.id,
                "model": model,
                "base_url": base_url,
                "api_key": api_key,
                "models": {m: {"name": m} for m in models},
            },
            {"apiFormat": provider.api_format},
        )
    raise ValueError(f"不支持的 app_type: {app_type}")


#: 这些 app 都是 openai 兼容：cc-switch 转发的上游形状同 codex 的 chat/completions 一族，
#: 这里存的 base_url/key 是**上游**（cc-switch 转发过去），身体只拿发布 id。
_GENERIC_OPENAI_APPS = ("openclaw", "hermes", "pi", "mcode")


def _openai_compatible(provider: Provider) -> tuple[dict[str, Any], dict[str, Any]]:
    """OpenCode / OpenClaw / Hermes / Pi / Mcode 的网关行。

    `options` 是这些 CLI 自己的配置形状。转发时它们走 Codex 适配器，适配器只读
    顶层 `base_url` 和 `api_key`，不读 `options`。两套都写上，请求才能换上真实 key。
    """
    models = provider.model_ids() or ([provider.model] if provider.model else [])
    return (
        {
            "base_url": provider.base_url,
            "api_key": provider.api_key,
            "npm": "@ai-sdk/openai-compatible",
            "options": {"baseURL": provider.base_url, "apiKey": provider.api_key},
            "models": {m: {"name": ""} for m in models},
        },
        {"apiFormat": provider.api_format},
    )


class Store:
    """cc-switch 的 SQLite：读 + 投影写。schema 由 cc-switch 拥有。"""

    def __init__(self, settings: Settings) -> None:
        self.settings = settings
        self.path = _db_path(settings)

    def _connect(self, readonly: bool = True) -> sqlite3.Connection:
        mode = "ro" if readonly else "rw"
        conn = sqlite3.connect(f"file:{self.path}?mode={mode}", uri=True, timeout=10)
        conn.row_factory = sqlite3.Row
        return conn

    def enabled(self) -> bool:
        return self.path.is_file()

    def _rows(self, sql: str, params: tuple[Any, ...] = ()) -> list[dict[str, Any]]:
        if not self.enabled():
            return []
        try:
            with closing(self._connect()) as conn:
                return [dict(row) for row in conn.execute(sql, params)]
        except sqlite3.Error:
            return []

    def providers(self, app_type: str | None = None) -> list[dict[str, Any]]:
        sql = "SELECT id, app_type, name, settings_config, meta, is_current FROM providers"
        params: tuple[Any, ...] = ()
        if app_type:
            sql += " WHERE app_type = ?"
            params = (app_type,)
        sql += " ORDER BY app_type, sort_index"
        out = []
        for row in self._rows(sql, params):
            for field in ("settings_config", "meta"):
                try:
                    row[field] = json.loads(row.get(field) or "{}")
                except (TypeError, ValueError):
                    row[field] = {}
            out.append(row)
        return out

    def project_provider(self, app_type: str, provider: Provider) -> str:
        """写/更新网关里 `app_type` 下对应这个通用 provider 的一条，返回其 id。"""
        if app_type not in SUPPORTED_APPS:
            raise ValueError(f"不支持的 app_type: {app_type}")
        if not self.enabled():
            raise RuntimeError(f"网关库不存在：{self.path}（先启动网关）")
        stored = "grokbuild" if app_type == "grok" else app_type
        settings_config, meta = _build_settings_config(app_type, provider)
        provider_id = f"crewchat-{stored}-{provider.id}"
        with closing(self._connect(readonly=False)) as conn:
            conn.execute(
                """
                INSERT INTO providers
                    (id, app_type, name, settings_config, meta, is_current,
                     in_failover_queue, created_at, sort_index)
                VALUES (?, ?, ?, ?, ?, 0, 0, ?, 0)
                ON CONFLICT(id, app_type) DO UPDATE SET
                    name = excluded.name,
                    settings_config = excluded.settings_config,
                    meta = excluded.meta
                """,
                (
                    provider_id,
                    stored,
                    provider.name,
                    json.dumps(settings_config, ensure_ascii=False),
                    json.dumps(meta, ensure_ascii=False),
                    int(time.time()),
                ),
            )
            conn.commit()
        return provider_id

    def usage_daily(self, days: int = 30) -> list[dict[str, Any]]:
        return self._rows(
            "SELECT * FROM usage_daily_rollups ORDER BY date DESC LIMIT ?", (days,)
        )


class CcSwitchGateway:
    """cc-switch 代理核心的适配器。"""

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or get_settings()
        self.proxy = f"http://127.0.0.1:{self.settings.gateway_proxy_port}"
        self.control = f"http://127.0.0.1:{self.settings.gateway_control_port}"
        self._log = self.settings.data_dir / "gateway.log"

    # ---- 生命周期 ----------------------------------------------------------

    def available(self) -> bool:
        return _binary(self.settings) is not None

    def identity(self) -> dict[str, Any] | None:
        """端口上那个代理的身份（它用的数据目录、库、进程号）。"""
        try:
            import httpx

            resp = httpx.get(f"{self.control}/identity", timeout=3)
            if resp.status_code != 200:
                return None
            data = resp.json()
            return data if isinstance(data, dict) else None
        except Exception:  # noqa: BLE001
            return None

    def _owns_endpoint(self) -> bool:
        """控制面上那个代理**是不是我们起的**。

        只探端口会把别人的进程当自己的：端口上可能是用默认数据目录（`~/.cc-switch`）
        起的另一个 cc-switch，连上去读的是别人的供应商表——表现就是「刚投影进去的供应商
        说不存在」。所以按身份校验：对不上就当它不存在，宁可自己起一个。
        """
        ident = self.identity()
        if ident is None:
            return False
        home = str(ident.get("home") or "")
        return Path(home).resolve() == self.settings.gateway_dir.resolve()

    def _foreign_endpoint(self) -> bool:
        """控制面可达、但身份不是我们的。"""
        ident = self.identity()
        return ident is not None and not self._owns_endpoint()

    def alive(self) -> bool:
        """**我们自己的**代理是否在跑（控制面可达且身份对得上）。"""
        try:
            import httpx

            if httpx.get(f"{self.control}/modes", timeout=3).status_code != 200:
                return False
        except Exception:  # noqa: BLE001
            return False
        return self._owns_endpoint()

    def proxy_up(self) -> bool:
        try:
            import httpx

            return httpx.get(f"{self.proxy}/health", timeout=3).status_code == 200
        except Exception:  # noqa: BLE001
            return False

    def ensure_running(self, wait: float = 15.0) -> bool:
        if self.alive():
            return True
        # 端口被**别的** cc-switch 占着：不能接着用，更不能悄悄把自己那份数据写进
        # 别人的库。明确报出来，让上层决定是换端口还是先停掉那个实例。
        if self._foreign_endpoint():
            raise RuntimeError(
                f"端口 {self.settings.gateway_control_port} 上是另一个 cc-switch 实例"
                f"（数据目录 {self.identity().get('home')}，进程 {self.identity().get('pid')}），"
                f"不是 CrewChat 自己的（{self.settings.gateway_dir}）。"
                "请先停掉它，或改 CREWCHAT_GATEWAY_CONTROL_PORT。"
            )
        binary = _binary(self.settings)
        if binary is None:
            raise RuntimeError(
                "找不到 ccsw-proxy 二进制；请设置 CREWCHAT_GATEWAY_BINARY 或构建 cc-switch 核心"
            )
        self.settings.ensure_dirs()
        env = {**os.environ, "CC_SWITCH_HOME": str(self.settings.gateway_dir)}
        with open(self._log, "ab") as log:
            subprocess.Popen(
                [str(binary), "serve", "--control-port", str(self.settings.gateway_control_port)],
                stdout=log,
                stderr=log,
                start_new_session=True,
                env=env,
            )
        deadline = time.monotonic() + wait
        while time.monotonic() < deadline:
            if self.alive():
                return True
            time.sleep(0.3)
        return False

    def stop(self) -> None:
        try:
            import httpx

            httpx.post(f"{self.control}/shutdown", timeout=3)
        except Exception:  # noqa: BLE001
            pass

    # ---- 控制面 ------------------------------------------------------------

    def channels(self) -> dict[str, bool]:
        try:
            import httpx

            resp = httpx.get(f"{self.control}/modes", timeout=5)
            resp.raise_for_status()
            return dict(resp.json().get("proxy_apps", {}))
        except Exception:  # noqa: BLE001 — 控制面未起时降级为全 false
            return {}

    def status(self) -> dict[str, Any]:
        return {
            "available": self.available(),
            "alive": self.alive(),
            "running": self.proxy_up(),
            "channels": self.channels(),
        }

    STACK_APPS = ("claude", "codex")

    def activate(self, app: str, provider: Provider) -> str | None:
        """把通用 provider 投影进网关，返回这次进程要用的发布 id。

        Claude / Codex 走 `/stack/route`，其余 CLI 走 `/stack/register`。两条都只登记
        Stack，不改用户的全局配置。发布 id 写进这一次进程的配置，网关按 id 选供应商，
        所以同一个 CLI 的两个会话可以用两家。
        """
        if app not in SUPPORTED_APPS:
            return None
        if not self.ensure_running():
            raise RuntimeError("网关未就绪")
        gateway_app = "grokbuild" if app == "grok" else app
        route = Store(self.settings).project_provider(app, provider)
        import httpx

        path = "/stack/route" if app in self.STACK_APPS else "/stack/register"
        resp = httpx.post(
            f"{self.control}{path}",
            json={"app": gateway_app, "provider_id": route, "model": provider.model},
            timeout=60,
        )
        resp.raise_for_status()
        return resp.json().get("model_id")

    def ensure(self, app: str) -> None:
        """把盘上该 CLI 的配置收敛到当前契约——**不改模式、不改路由**。

        `activate` 已经会把配置写对，但 CLI 自己会刷新配置、别的程序也会改它。起进程
        之前再收敛一次，配置被谁改回去都能补回来——这才是「说用哪一家就用哪一家」的前提。
        """
        if not self.alive():
            return
        import httpx

        httpx.post(f"{self.control}/ensure", json={"app": app}, timeout=60).raise_for_status()

    def deactivate(self, app: str) -> None:
        import httpx

        resp = httpx.post(f"{self.control}/exit", json={"app": app}, timeout=30)
        resp.raise_for_status()

    def usage(self, days: int = 30) -> list[dict[str, Any]]:
        return Store(self.settings).usage_daily(days)
