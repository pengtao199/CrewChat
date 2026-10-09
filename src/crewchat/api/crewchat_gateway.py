"""Gateway: 模型网关的状态与控制（自研 `/api/crewchat/gateway`）。

前端读这里的进程状态与每个 CLI 的通道。让某 CLI 走代理时，把它选中的**通用**
provider 交给端口 `activate`——实现（cc-switch / 将来别的）负责把它投影/编译给该 CLI。
业务只依赖 `core.gateway` 的端口，换底层实现不改这里。见 `AGENTS/gateway.md`。
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..core.config import get_settings
from ..core.gateway import get_gateway
from ..core.providers import get_provider

router = APIRouter()


@router.get("/api/crewchat/gateway")
async def gateway_status() -> dict[str, Any]:
    return get_gateway().status()


@router.post("/api/crewchat/gateway/start")
async def gateway_start() -> Any:
    try:
        return {"success": get_gateway().ensure_running()}
    except RuntimeError as exc:
        return JSONResponse({"success": False, "message": str(exc)}, status_code=503)


@router.post("/api/crewchat/gateway/stop")
async def gateway_stop() -> dict[str, Any]:
    get_gateway().stop()
    return {"success": True}


@router.post("/api/crewchat/gateway/mode")
async def gateway_set_mode(request: Request) -> Any:
    """让某个 CLI（`app`）走网关 / 回到直连。

    走网关用 `provider_id`（大脑池条目）；未指定则用默认端点（第一个带 key 的）。
    """
    body = await request.json()
    app = str(body.get("app") or "")
    if not app:
        return JSONResponse({"error": "missing_app"}, status_code=400)
    gateway = get_gateway()
    try:
        if body.get("enabled", True):
            provider = get_provider(get_settings(), str(body.get("provider_id") or ""))
            if provider is None:
                return JSONResponse({"error": "no_provider"}, status_code=400)
            gateway.activate(app, provider)
        else:
            gateway.deactivate(app)
    except Exception as exc:  # noqa: BLE001
        return JSONResponse(
            {"error": "gateway_failed", "message": f"{type(exc).__name__}: {exc}"},
            status_code=502,
        )
    return {"success": True, "channels": gateway.status().get("channels", {})}


@router.get("/api/crewchat/gateway/usage")
async def gateway_usage(days: int = 30) -> list[dict[str, Any]]:
    return get_gateway().usage(days)
