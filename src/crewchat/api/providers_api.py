"""Models: the brain pool the UI manages under Settings → 模型."""

from __future__ import annotations

import uuid
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..core.config import get_settings
from ..core.providers import Provider, load_providers, save_providers

router = APIRouter()


def to_api(p: Provider) -> dict[str, Any]:
    return {
        "id": p.id,
        "platform": "openai",
        "name": p.name,
        "base_url": p.base_url,
        "api_key": p.api_key,
        "models": p.models or ([p.model] if p.model else []),
        "enabled": True,
    }


@router.get("/api/providers")
async def list_providers() -> list[dict[str, Any]]:
    return [to_api(p) for p in load_providers(get_settings())]


@router.post("/api/providers")
async def create_provider(request: Request) -> dict[str, Any]:
    body = await request.json()
    settings = get_settings()
    providers = load_providers(settings)
    provider = Provider(
        id=str(body.get("id") or uuid.uuid4().hex[:8]),
        name=str(body.get("name") or "新端点"),
        base_url=str(body.get("base_url") or ""),
        model=str(body.get("model") or (body.get("models") or [""])[0]),
        api_key=str(body.get("api_key") or ""),
        models=list(body.get("models") or []),
    )
    providers.append(provider)
    save_providers(settings, providers)
    return to_api(provider)


@router.put("/api/providers/{provider_id}")
async def update_provider(provider_id: str, request: Request) -> Any:
    body = await request.json()
    settings = get_settings()
    providers = load_providers(settings)
    target = next((p for p in providers if p.id == provider_id), None)
    if target is None:
        return JSONResponse({"error": "not_found"}, status_code=404)
    for field in ("name", "base_url", "api_key", "model"):
        if body.get(field) is not None:
            setattr(target, field, str(body[field]))
    if body.get("models") is not None:
        target.models = list(body["models"])
        if not body.get("model") and target.models:
            target.model = target.models[0]
    save_providers(settings, providers)
    return to_api(target)


@router.delete("/api/providers/{provider_id}")
async def delete_provider(provider_id: str) -> dict[str, Any]:
    settings = get_settings()
    providers = [p for p in load_providers(settings) if p.id != provider_id]
    save_providers(settings, providers)
    return {"success": True}


@router.post("/api/providers/fetch-models")
async def fetch_models(request: Request) -> dict[str, Any]:
    """List models from the endpoint itself, so the picker never asks the user to type them.

    Accepts either an existing provider id (the stored key is used server-side, so
    the browser never needs it) or an explicit base_url/api_key pair while editing.
    """
    body = await request.json()
    provider_id = str(body.get("provider_id") or "")
    if provider_id:
        provider = next((p for p in load_providers(get_settings()) if p.id == provider_id), None)
        if provider is None:
            return {"models": [], "success": False, "message": f"端点不存在：{provider_id}"}
        base_url = provider.base_url.rstrip("/")
        api_key = provider.api_key
    else:
        base_url = str(body.get("base_url") or "").rstrip("/")
        api_key = str(body.get("api_key") or "")
    if not base_url:
        return {"models": [], "success": False, "message": "缺少 base_url"}
    try:
        import httpx

        headers = {"Authorization": f"Bearer {api_key}"} if api_key else {}
        with httpx.Client(timeout=20) as client:
            resp = client.get(f"{base_url}/models", headers=headers)
            resp.raise_for_status()
            data = resp.json()
        models = [m.get("id") for m in (data.get("data") or []) if m.get("id")]
        return {"models": sorted(models), "success": True}
    except Exception as exc:  # noqa: BLE001
        return {"models": [], "success": False, "message": f"{type(exc).__name__}: {exc}"}


@router.post("/api/providers/detect-protocol")
async def detect_protocol(request: Request) -> dict[str, Any]:
    return {"protocol": "openai", "success": True}
