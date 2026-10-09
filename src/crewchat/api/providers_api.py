"""Models: the brain pool the UI manages under Settings → 模型.

唯一真相源是 `.data/providers.jsonc`（我们的通用 provider）。网关里按 CLI 分的那几份
provider 是由这里**投影**出来的派生物，见 `core/gateway.py`。
"""

from __future__ import annotations

import uuid
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..core.config import get_settings
from ..core.model_catalog import enrich_models, fetch_provider_models, infer_tags, known_context_limit
from ..core.providers import (
    MODEL_TAGS,
    Model,
    Provider,
    load_providers,
    remove_model,
    save_providers,
    upsert_model,
)

router = APIRouter()

# 明文 key 只在两种情况下需要：新建时、或用户显式要求查看。常规列表一律掩码。
_KEY_MASK = "••••••••"


def _masked(p: Provider) -> str:
    return _KEY_MASK if p.has_key() else ""


def model_to_api(m: Model, enriched: dict[str, Any] | None = None) -> dict[str, Any]:
    """模型对外形状。

    标签是**自动推导**的（cc-switch 能力判定 + models.dev 价格 + 命名约定），
    用户手工打的标签合并进来。推导结果不落盘 —— 真相源只存用户显式设过的标签，
    这样换价格源或升级 cc-switch 时标签会自动跟着变，不会留下陈旧数据。
    """
    enriched = enriched or {}
    auto = infer_tags(m, enriched)
    merged = [t for t in MODEL_TAGS if t in set(auto) | set(m.tags)]
    return {
        "id": m.id,
        "display_name": m.display_name,
        "label": m.label,
        "tags": merged,
        "auto_tags": [t for t in auto if t not in m.tags],
        "context_limit": m.context_limit or enriched.get("context_limit") or known_context_limit(m.id),
        "enabled": m.enabled,
        "protocol": m.protocol,
        "capabilities": m.capabilities or enriched.get("capabilities", {}),
        "price": m.price or enriched.get("price", {}),
    }


def to_api(p: Provider, enriched: dict[str, dict[str, Any]] | None = None) -> dict[str, Any]:
    """供应商的对外形状。

    这里必须**完整**：页面上能写进去的字段都要能存回来，否则前端写完就丢。
    `api_key` 只给掩码与"是否有 key"，明文在需要时单取。
    """
    enriched = enriched or {}
    return {
        "id": p.id,
        "platform": "openai",
        "name": p.name,
        "base_url": p.base_url,
        "api_key": _masked(p),
        "has_key": p.has_key(),
        "key_count": p.key_count(),
        "model": p.model,
        "models": [model_to_api(m, enriched.get(m.id)) for m in p.models],
        "model_count": len(p.models),
        "api_format": p.api_format,
        "temperature": p.temperature,
        "max_tokens": p.max_tokens,
        "extra_headers": p.extra_headers,
        "enabled": p.enabled,
        "is_full_url": p.is_full_url,
        "models_url_override": p.models_url_override,
    }


def enrich_all(settings: Settings, providers: list[Provider]) -> dict[str, dict[str, Any]]:
    """对所有供应商的模型批量补全 cc-switch 元数据（供列表接口使用）。"""
    ids = [m.id for p in providers for m in p.models]
    if not ids:
        return {}
    try:
        return enrich_models(settings, ids)
    except Exception:  # noqa: BLE001 - 元数据是锦上添花，取不到就用已存的
        return {}


def _merge_provider_fields(target: Provider, body: dict[str, Any]) -> None:
    """把请求体合并进 provider。只覆盖显式给出的字段。"""
    for field in ("name", "base_url", "model", "api_format", "models_url_override"):
        if body.get(field) is not None:
            setattr(target, field, str(body[field]))
    if body.get("enabled") is not None:
        target.enabled = bool(body["enabled"])
    if body.get("is_full_url") is not None:
        target.is_full_url = bool(body["is_full_url"])
    if body.get("temperature") is not None:
        target.temperature = body["temperature"]
    if body.get("max_tokens") is not None:
        target.max_tokens = body["max_tokens"]
    if body.get("extra_headers") is not None:
        target.extra_headers = dict(body["extra_headers"] or {})
    # 掩码回传时不要把 "••••••••" 当成真 key 存回去。
    if body.get("api_key") and body["api_key"] != _KEY_MASK:
        target.api_key = str(body["api_key"])


def _model_from_body(body: dict[str, Any]) -> Model:
    """从请求体构造 Model。

    标签**不接受客户端传入** —— 标签一律由 `infer_tags` 推导（cc-switch 能力判定 +
    价格目录 + 命名约定）。用户只能覆盖显示名与上下文长度。
    """
    context_limit = body.get("context_limit")
    return Model(
        id=str(body.get("id") or "").strip(),
        display_name=str(body.get("display_name") or "").strip(),
        tags=[],
        context_limit=int(context_limit) if isinstance(context_limit, (int, float)) else None,
        enabled=bool(body.get("enabled", True)),
        protocol=str(body["protocol"]) if body.get("protocol") else None,
    )


@router.get("/api/providers")
async def list_providers() -> list[dict[str, Any]]:
    settings = get_settings()
    providers = load_providers(settings)
    enriched = enrich_all(settings, providers)
    return [to_api(p, enriched) for p in providers]


@router.post("/api/providers")
async def create_provider(request: Request) -> Any:
    """新增供应商，并**自动拉取该端点的全部模型**。

    模型没有可靠的人工枚举方式，所以新增即全量抓取：用户填好端点与密钥后不必再
    去翻服务商文档挑模型 id。抓取失败（网络 / 端点 / 密钥问题）直接报错，不静默
    建一个空供应商 —— 否则用户会以为配好了，实际一个模型都用不了。
    """
    body = await request.json()
    settings = get_settings()
    providers = load_providers(settings)

    provider = Provider(
        id=str(body.get("id") or uuid.uuid4().hex[:8]),
        name=str(body.get("name") or "新端点"),
        base_url=str(body.get("base_url") or ""),
        model=str(body.get("model") or ""),
        api_key=str(body.get("api_key") or ""),
        models=list(body.get("models") or []),
        api_format=str(body.get("api_format") or "openai_chat"),
        extra_headers=dict(body.get("extra_headers") or {}),
        enabled=bool(body.get("enabled", True)),
        is_full_url=bool(body.get("is_full_url", False)),
        models_url_override=str(body.get("models_url_override") or ""),
    )

    # 全量抓取。已有模型（迁移/导入场景）不覆盖。
    if not provider.models and provider.base_url:
        try:
            discovered = fetch_provider_models(settings, provider)
        except Exception as exc:  # noqa: BLE001
            return JSONResponse(
                {
                    "error": "fetch_models_failed",
                    "message": f"{type(exc).__name__}: {exc}",
                },
                status_code=502,
            )
        if not discovered:
            return JSONResponse(
                {"error": "fetch_models_empty", "message": "端点未返回任何模型"},
                status_code=502,
            )
        ids = [str(item["id"]) for item in discovered]
        enriched = enrich_models(settings, ids)
        provider.models = [
            Model(
                id=model_id,
                display_name=str(enriched.get(model_id, {}).get("suggested_display_name") or ""),
                tags=[],
                enabled=True,
            )
            for model_id in ids
        ]

    if not provider.model and provider.models:
        provider.model = provider.models[0].id
    providers.append(provider)
    save_providers(settings, providers)
    return {**to_api(provider), "fetched": len(provider.models)}


@router.put("/api/providers/{provider_id}")
async def update_provider(provider_id: str, request: Request) -> Any:
    body = await request.json()
    settings = get_settings()
    providers = load_providers(settings)
    target = next((p for p in providers if p.id == provider_id), None)
    if target is None:
        return JSONResponse({"error": "not_found"}, status_code=404)
    _merge_provider_fields(target, body)
    if body.get("models") is not None:
        target.models = [m for m in (Model.from_raw(raw) for raw in body["models"]) if m]
        if not target.model and target.models:
            target.model = target.models[0].id
    save_providers(settings, providers)
    return to_api(target)


@router.delete("/api/providers/{provider_id}")
async def delete_provider(provider_id: str) -> dict[str, Any]:
    settings = get_settings()
    providers = [p for p in load_providers(settings) if p.id != provider_id]
    save_providers(settings, providers)
    return {"success": True}


@router.get("/api/providers/{provider_id}/secret")
async def reveal_provider_key(provider_id: str) -> Any:
    """按需取明文 key。默认列表永远只给掩码。"""
    provider = next((p for p in load_providers(get_settings()) if p.id == provider_id), None)
    if provider is None:
        return JSONResponse({"error": "not_found"}, status_code=404)
    return {"api_key": provider.api_key}


# ----- 模型 ------------------------------------------------------------------


@router.post("/api/providers/{provider_id}/models")
async def add_model(provider_id: str, request: Request) -> Any:
    """新增 / 编辑单个模型。`id` 已存在则覆盖。

    标签始终自动推导，不接受客户端传入；用户能覆盖的是显示名与上下文长度。
    """
    body = await request.json()
    settings = get_settings()
    providers = load_providers(settings)
    target = next((p for p in providers if p.id == provider_id), None)
    if target is None:
        return JSONResponse({"error": "not_found"}, status_code=404)

    model = _model_from_body(body)
    if not model.id:
        return JSONResponse({"error": "model_id_required"}, status_code=400)

    # 保留已有模型上已确认的元数据；新模型从 cc-switch 补全。
    existing = target.get_model(model.id)
    enriched = enrich_models(settings, [model.id]).get(model.id, {})
    if existing:
        if not model.display_name:
            model.display_name = existing.display_name
        if model.context_limit is None:
            model.context_limit = existing.context_limit
        model.capabilities = existing.capabilities or enriched.get("capabilities", {})
        model.price = existing.price or enriched.get("price", {})
    else:
        if "capabilities" in enriched:
            model.capabilities = enriched["capabilities"]
        if "price" in enriched:
            model.price = enriched["price"]
        if not model.display_name and enriched.get("suggested_display_name"):
            model.display_name = str(enriched["suggested_display_name"])

    upsert_model(target, model)
    if not target.model:
        target.model = model.id
    save_providers(settings, providers)
    return {"success": True, "model": model_to_api(model, enriched), "provider": to_api(target)}


@router.patch("/api/providers/{provider_id}/models")
async def bulk_toggle_models(provider_id: str, request: Request) -> Any:
    """批量启停模型。`ids` 为空则作用于该供应商的全部模型。

    自动抓取会带来上百个模型，逐个点开关不现实，所以必须能一次批量控制。
    """
    body = await request.json()
    settings = get_settings()
    providers = load_providers(settings)
    target = next((p for p in providers if p.id == provider_id), None)
    if target is None:
        return JSONResponse({"error": "not_found"}, status_code=404)

    enabled = bool(body.get("enabled", True))
    ids = {str(i) for i in (body.get("ids") or [])}
    for model in target.models:
        if not ids or model.id in ids:
            model.enabled = enabled

    save_providers(settings, providers)
    return {"success": True, "changed": len(target.models) if not ids else len(ids), "provider": to_api(target)}


@router.delete("/api/providers/{provider_id}/models/{model_id}")
async def delete_model(provider_id: str, model_id: str) -> Any:
    settings = get_settings()
    providers = load_providers(settings)
    target = next((p for p in providers if p.id == provider_id), None)
    if target is None:
        return JSONResponse({"error": "not_found"}, status_code=404)
    if not remove_model(target, model_id):
        return JSONResponse({"error": "model_not_found"}, status_code=404)
    if target.model == model_id:
        target.model = target.models[0].id if target.models else ""
    save_providers(settings, providers)
    return {"success": True, "provider": to_api(target)}


@router.post("/api/providers/fetch-models")
async def fetch_models(request: Request) -> dict[str, Any]:
    """从供应商端点拉取可用模型（手动添加模型时的辅助，不是主路径）。

    接受已存在的 provider id（key 在服务端用，浏览器永远拿不到），或编辑态下的
    显式 base_url/api_key。抓取逻辑复用 cc-switch 的 `model_fetch`。
    """
    body = await request.json()
    settings = get_settings()
    provider_id = str(body.get("provider_id") or "")

    if provider_id:
        provider = next((p for p in load_providers(settings) if p.id == provider_id), None)
        if provider is None:
            return {"models": [], "success": False, "message": f"端点不存在：{provider_id}"}
        if body.get("api_key"):
            # 编辑态换了 key，用新的试拉，但不落盘。
            provider = Provider.from_dict({**provider.as_dict(reveal=True), "api_key": body["api_key"]})
        if body.get("base_url"):
            provider.base_url = str(body["base_url"])
    else:
        base_url = str(body.get("base_url") or "").rstrip("/")
        if not base_url:
            return {"models": [], "success": False, "message": "缺少 base_url"}
        provider = Provider(
            id="__probe__",
            name="__probe__",
            base_url=base_url,
            model="",
            api_key=str(body.get("api_key") or ""),
            api_format=str(body.get("api_format") or "openai_chat"),
            is_full_url=bool(body.get("is_full_url", False)),
            extra_headers=dict(body.get("extra_headers") or {}),
        )

    try:
        models = fetch_provider_models(settings, provider)
    except Exception as exc:  # noqa: BLE001
        return {"models": [], "success": False, "message": f"{type(exc).__name__}: {exc}"}
    return {"models": models, "success": True}


@router.get("/api/model-tags")
async def list_model_tags() -> dict[str, Any]:
    """合法标签枚举，供前端渲染标签选择器（前端不硬编码这份列表）。"""
    return {"tags": list(MODEL_TAGS)}


@router.post("/api/providers/detect-protocol")
async def detect_protocol(request: Request) -> dict[str, Any]:
    return {"protocol": "openai", "success": True}