"""CrewChat backend: implements the HTTP/WS contract the forked UI expects.

Endpoints are grouped the same way the UI's bridge groups them, so a missing
feature is a missing router rather than a misplaced one. Unimplemented paths
return a structured 501 so the UI logs something actionable instead of a bare
404 while we fill them in.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from fastapi import FastAPI, Request, WebSocket, WebSocketDisconnect
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import FileResponse, JSONResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

from . import (
    agents_api,
    crewchat_agents,
    chat,
    assistants_api,
    auth,
    conversations,
    mcp_api,
    providers_api,
    skills_api,
)
from .store import store

UI_DIST = Path(__file__).resolve().parents[3] / "ui" / "dist"


class ClientPrefsBody(BaseModel):
    """PUT /api/settings/client accepts a partial map of config keys."""

    model_config = {"extra": "allow"}


def create_app() -> FastAPI:
    app = FastAPI(title="CrewChat", docs_url=None, redoc_url=None)
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
        assistants_api,
        agents_api,
        crewchat_agents,
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
            "codexProvider": cfg.codex_provider,
        }

    @app.get("/api/system/info")
    async def system_info() -> dict[str, Any]:
        import platform

        return {
            "platform": platform.system().lower(),
            "arch": platform.machine(),
            "version": "0.1.0",
            "backend": "crewchat",
        }

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
    async def not_implemented(path: str) -> JSONResponse:
        return JSONResponse(
            {"error": "not_implemented", "path": f"/api/{path}", "backend": "crewchat"},
            status_code=501,
        )

    # ----- static UI --------------------------------------------------------

    if UI_DIST.is_dir():
        app.mount("/assets", StaticFiles(directory=UI_DIST / "assets"), name="assets")

        @app.get("/{path:path}", include_in_schema=False)
        async def spa(path: str) -> Any:
            candidate = UI_DIST / path
            if path and candidate.is_file():
                return FileResponse(candidate)
            return FileResponse(UI_DIST / "index.html")

    return app


app = create_app()
