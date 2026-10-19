"""CrewChat backend: the Python side's HTTP/WS surface.

The endpoints were shaped for the forked UI that has since been removed; they
are kept as the Python API's own surface until it is re-purposed. Unimplemented
paths return a structured 501 so a caller logs something actionable instead of
a bare 404.

Every HTTP call must carry the API version root (``/api/v1``, see
``core.version.api_prefix``). Routes stay registered under the unversioned
``/api`` and ``ApiVersionMiddleware`` enforces the version at the ASGI
boundary, so a new router is exposed under the version root without edits and
requests that skip the version segment are rejected instead of silently served.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from fastapi import FastAPI, Request, WebSocket, WebSocketDisconnect
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from pydantic import BaseModel
from starlette.types import ASGIApp, Receive, Scope, Send

from ..core.version import api_prefix, backend_version, version_info
from . import (
    engines_api,
    crewchat_agents,
    crewchat_projects,
    crewchat_usage,
    chat,
    agent_catalog_api,
    auth,
    conversations,
    fs_api,
    mcp_api,
    providers_api,
    skills_api,
)
from .store import store

# 路由内部注册的根（未版本化）。版本段只存在于对外请求路径上。
_INTERNAL_API_ROOT = "/api"


class ApiVersionMiddleware:
    """在 ASGI 边界强制 API 版本根。

    带版本的 ``/api/v1/...`` 去掉版本段后交给已注册的 ``/api/...`` 路由；不带
    版本的 ``/api/...`` 以及已迁进版本根的旧入口（``/login``、``/logout``）
    直接返回 404 与期望的版本根，不做兼容转发。WebSocket、静态资源与前端
    路由不在版本管辖内，原样透传。
    """

    def __init__(
        self,
        app: ASGIApp,
        prefix: str,
        internal_root: str,
        legacy_paths: tuple[str, ...] = (),
    ) -> None:
        self.app = app
        self.prefix = prefix
        self.internal_root = internal_root
        self.legacy_paths = legacy_paths

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        path: str = scope.get("path", "")
        if path == self.prefix or path.startswith(self.prefix + "/"):
            rewritten = self.internal_root + path[len(self.prefix) :]
            scope = {**scope, "path": rewritten, "raw_path": rewritten.encode()}
            # 501 兜底要报出调用方真正请求的带版本路径，便于定位。
            scope["crewchat_requested_path"] = path
            await self.app(scope, receive, send)
            return

        unversioned = path == self.internal_root or path.startswith(self.internal_root + "/")
        if unversioned or path.lstrip("/") in self.legacy_paths:
            response = JSONResponse(
                {
                    "error": "api_version_required",
                    "path": path,
                    "expected_prefix": self.prefix,
                    "backend": "crewchat",
                },
                status_code=404,
            )
            await response(scope, receive, send)
            return

        await self.app(scope, receive, send)


class ClientPrefsBody(BaseModel):
    """PUT /api/v1/settings/client accepts a partial map of config keys."""

    model_config = {"extra": "allow"}


def create_app() -> FastAPI:
    app = FastAPI(title="CrewChat", docs_url=None, redoc_url=None)
    # 版本中间件先注册（位于内层），CORS 后注册（最外层），
    # 这样版本根拒绝的 404 响应也带 CORS 头。
    app.add_middleware(
        ApiVersionMiddleware,
        prefix=api_prefix(),
        internal_root=_INTERNAL_API_ROOT,
    )
    app.add_middleware(
        CORSMiddleware,
        allow_origins=["http://localhost:5173", "http://127.0.0.1:5173"],
        allow_credentials=True,
        allow_methods=["*"],
        allow_headers=["*"],
    )
    for module in (
        auth,
        chat,
        conversations,
        providers_api,
        engines_api,
        agent_catalog_api,
        crewchat_agents,
        crewchat_projects,
        crewchat_usage,
        fs_api,
        skills_api,
        mcp_api,
    ):
        app.include_router(module.router)

    # ----- settings ---------------------------------------------------------

    @app.get("/api/settings/client")
    async def get_client_settings(keys: str | None = None) -> dict[str, Any]:
        wanted = [k for k in (keys or "").split(",") if k] or None
        return store().get_prefs(wanted)

    @app.put("/api/settings/client")
    async def put_client_settings(body: dict[str, Any]) -> dict[str, Any]:
        store().set_prefs(body)
        return {"success": True}

    @app.get("/api/settings")
    async def get_settings() -> dict[str, Any]:
        from ..core.cli_overrides import current_settings

        cfg = current_settings()
        return {
            "language": cfg.language,
        }

    @app.get("/api/system/info")
    async def system_info() -> dict[str, Any]:
        import platform

        from ..core.config import get_settings

        settings = get_settings()
        return {
            "platform": platform.system().lower(),
            "arch": platform.machine(),
            "version": backend_version(),
            "backend": "crewchat",
            # 仍会消费该接口的客户端用它做文件选择器的默认起始目录。
            "work_dir": str(Path.home()),
            "cache_dir": str(settings.data_dir),
            "log_dir": str(settings.data_dir),
        }

    @app.get("/api/crewchat/version")
    async def crewchat_version() -> dict[str, Any]:
        return {"success": True, **version_info()}

    @app.get("/api/system/current-user")
    async def current_user(request: Request) -> dict[str, Any]:
        user = auth.current_user(request)
        return {"success": user is not None, "user": user}

    # ----- websocket --------------------------------------------------------

    @app.websocket("/ws")
    async def ws_endpoint(websocket: WebSocket) -> None:
        from .hub import hub

        await websocket.accept()
        await hub.add(websocket)
        try:
            while True:
                frame = await websocket.receive_json()
                name = frame.get("name") or frame.get("event")
                if name == "fs":
                    continue
        except WebSocketDisconnect:
            pass
        except Exception:  # noqa: BLE001
            pass
        finally:
            await hub.remove(websocket)

    # ----- unsupported routes ----------------------------------------------

    @app.api_route(
        "/api/{path:path}",
        methods=["GET", "POST", "PUT", "PATCH", "DELETE"],
        include_in_schema=False,
    )
    async def not_implemented(request: Request, path: str) -> JSONResponse:
        requested = request.scope.get("crewchat_requested_path") or f"/api/{path}"
        return JSONResponse(
            {"error": "not_implemented", "path": requested, "backend": "crewchat"},
            status_code=501,
        )

    return app


app = create_app()
