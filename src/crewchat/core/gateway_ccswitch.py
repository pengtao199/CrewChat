"""cc-switch 适配器：用它的代理核心实现 `ModelGateway`。

复用 cc-switch 的核心（`ccsw-proxy` 进程 + 其 SQLite），但数据归我们：

- 网关的 home 由 `Settings.gateway_dir` 指定（默认 `.data/gateway/`），经
  `CC_SWITCH_HOME` 注入；**绝不写用户本机的 `~/.cc-switch/`**。
- 网关库里的 provider 是**派生物**——由通用 provider 投影而来，可重建。

cc-switch 没有"通用 provider"概念（严格按 app_type 分），所以 `activate` 分两步：
把通用 provider 投影成该 CLI 的一条，再 `enter`（它据此重写 CLI 客户端配置）。
"""

from __future__ import annotations

import json
import os
import shutil
import sqlite3
import subprocess
import time
from pathlib import Path
from typing import Any

from .config import Settings, get_settings
from .providers import Provider

SUPPORTED_APPS = ("codex", "claude", "opencode")


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
    api_key = provider.api_key
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
        models = provider.models or ([model] if model else [])
        return (
            {
                "npm": "@ai-sdk/openai-compatible",
                "options": {"baseURL": base_url, "apiKey": api_key},
                "models": {m: {"name": ""} for m in models},
            },
            {},
        )
    raise ValueError(f"不支持的 app_type: {app_type}")


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
            with self._connect() as conn:
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
        settings_config, meta = _build_settings_config(app_type, provider)
        provider_id = f"crewchat-{app_type}-{provider.id}"
        with self._connect(readonly=False) as conn:
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
                    app_type,
                    provider.name,
                    json.dumps(settings_config, ensure_ascii=False),
                    json.dumps(meta, ensure_ascii=False),
                    int(time.time()),
                ),
            )
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

    def alive(self) -> bool:
        """进程是否在跑（控制面可达）。代理在无 app 走代理时会自行停止，但进程仍在。"""
        try:
            import httpx

            return httpx.get(f"{self.control}/modes", timeout=3).status_code == 200
        except Exception:  # noqa: BLE001
            return False

    def proxy_up(self) -> bool:
        try:
            import httpx

            return httpx.get(f"{self.proxy}/health", timeout=3).status_code == 200
        except Exception:  # noqa: BLE001
            return False

    def ensure_running(self, wait: float = 15.0) -> bool:
        if self.alive():
            return True
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

    def activate(self, app: str, provider: Provider) -> None:
        if not self.ensure_running():
            raise RuntimeError("网关未就绪")
        route = Store(self.settings).project_provider(app, provider)
        import httpx

        resp = httpx.post(
            f"{self.control}/enter", json={"app": app, "route": route}, timeout=30
        )
        resp.raise_for_status()

    def deactivate(self, app: str) -> None:
        import httpx

        resp = httpx.post(f"{self.control}/exit", json={"app": app}, timeout=30)
        resp.raise_for_status()

    def usage(self, days: int = 30) -> list[dict[str, Any]]:
        return Store(self.settings).usage_daily(days)
