"""模型元数据适配器：能力判定与价格目录，复用 cc-switch 核心。

我们不在 Python 侧重写这些判定。cc-switch 已有成熟的视觉能力登记表
(`model_capabilities`) 与 models.dev 价格目录 (`model_pricing`)，我们通过
headless 控制口调用它们，只做数据形状转换。

控制口不可用时全部**降级为空**，绝不让模型页因为拿不到元数据而打不开。
"""

from __future__ import annotations

import threading
import time
from typing import Any

import httpx

from .config import Settings
from .gateway import get_gateway

# 价格目录变化很慢（models.dev 按天更新），缓存久一点。
_CATALOG_TTL = 12 * 3600.0
# 拉取失败后的重试间隔。失败时**不能**把空结果按正常 TTL 缓存住，
# 否则控制口晚起一会儿就会让价格整整 12 小时都是空的。
_CATALOG_RETRY_TTL = 30.0

_catalog: dict[str, Any] = {"ts": 0.0, "prices": {}}
_catalog_lock = threading.Lock()


def _refresh_catalog(settings: Settings) -> None:
    control = _ready(settings)
    with httpx.Client(timeout=15.0) as client:
        response = client.get(f"{control}/models/catalog")
        response.raise_for_status()
        entries = response.json()

    prices: dict[str, dict[str, Any]] = {}
    for entry in entries or []:
        model_id = str(entry.get("modelId") or entry.get("model_id") or "").strip()
        if not model_id:
            continue
        prices[model_id] = {
            "display_name": entry.get("displayName") or entry.get("display_name") or "",
            "input": _to_float(entry.get("inputCostPerMillion") or entry.get("input_cost_per_million")),
            "output": _to_float(entry.get("outputCostPerMillion") or entry.get("output_cost_per_million")),
            "cache_read": _to_float(entry.get("cacheReadCostPerMillion") or entry.get("cache_read_cost_per_million")),
            "cache_write": _to_float(
                entry.get("cacheCreationCostPerMillion") or entry.get("cache_creation_cost_per_million")
            ),
        }

    with _catalog_lock:
        _catalog["prices"] = prices
        _catalog["ts"] = time.monotonic()


def _to_float(value: Any) -> float | None:
    try:
        return float(value) if value not in (None, "") else None
    except (TypeError, ValueError):
        return None


def _ready(settings: Settings) -> str:
    """确保 cc-switch 控制进程在跑，返回控制口地址。"""
    gateway = get_gateway(settings)
    if not gateway.ensure_running():
        raise RuntimeError("cc-switch 控制进程未就绪")
    return gateway.control


def _load_catalog(settings: Settings) -> dict[str, Any]:
    with _catalog_lock:
        prices = _catalog["prices"]
        ts = _catalog["ts"]
    ttl = _CATALOG_TTL if prices else _CATALOG_RETRY_TTL
    if prices or (time.monotonic() - ts) < ttl:
        return prices
    try:
        _refresh_catalog(settings)
    except Exception:  # noqa: BLE001 - 元数据是锦上添花，拿不到就用缓存/空
        with _catalog_lock:
            # 只推进时间戳做退避，prices 保持为空，下个请求会再试。
            _catalog["ts"] = time.monotonic()
        return _catalog["prices"]
    with _catalog_lock:
        return dict(_catalog["prices"])


def _fetch_capabilities(settings: Settings, models: list[str]) -> dict[str, bool | None]:
    """判定模型的图像输入能力。None 表示 cc-switch 没有结论（fail-open）。"""
    ids = [m for m in models if m]
    if not ids:
        return {}
    try:
        control = _ready(settings)
        with httpx.Client(timeout=20.0) as client:
            response = client.post(
                f"{control}/models/capabilities",
                json={"models": ids},
            )
            response.raise_for_status()
            payload = response.json()
    except Exception:  # noqa: BLE001
        return {}

    out: dict[str, bool | None] = {}
    for item in payload.get("models", []):
        out[str(item.get("model") or "")] = item.get("vision")
    return out


def fetch_provider_models(settings: Settings, provider: Any) -> list[dict[str, Any]]:
    """从供应商端点拉取可用模型（手动添加模型时的辅助，不是主路径）。

    复用 cc-switch 的 `model_fetch`：它有版本段 URL 阶梯（`/v4` vs `/v1`）和按
    api_format 的鉴权头，比我们早先的 30 行实现完整得多。
    """
    control = _ready(settings)
    body: dict[str, Any] = {
        "base_url": provider.base_url,
        "api_key": provider.api_key,
        "is_full_url": provider.is_full_url,
        "api_format": provider.api_format,
    }
    if provider.models_url_override:
        body["models_url_override"] = provider.models_url_override
    if provider.extra_headers:
        body["request_headers"] = provider.extra_headers

    with httpx.Client(timeout=40.0) as client:
        response = client.post(f"{control}/models/fetch", json=body)
        response.raise_for_status()
        payload = response.json()

    prices = _load_catalog(settings)
    out: list[dict[str, Any]] = []
    for item in payload or []:
        model_id = str(item.get("id") or "").strip()
        if not model_id:
            continue
        entry: dict[str, Any] = {"id": model_id}
        if item.get("ownedBy") or item.get("owned_by"):
            entry["owned_by"] = item.get("ownedBy") or item.get("owned_by")
        if model_id in prices:
            entry["price"] = prices[model_id]
        out.append(entry)
    return out


def enrich_models(settings: Settings, model_ids: list[str]) -> dict[str, dict[str, Any]]:
    """用 cc-switch 的能力判定与价格目录补全模型元数据。

    返回 `{model_id: {"capabilities": {...}, "price": {...}, "display_name": ...}}`，
    只包含确实拿到的字段，不覆盖用户已有的标签。
    """
    if not model_ids:
        return {}

    vision = _fetch_capabilities(settings, model_ids)
    prices = _load_catalog(settings)

    out: dict[str, dict[str, Any]] = {}
    for model_id in model_ids:
        entry: dict[str, Any] = {}
        if model_id in vision:
            # 只在有明确结论时写 capabilities；None（未知）保持不写，交给 fail-open。
            if vision[model_id] is not None:
                entry["capabilities"] = {"vision": bool(vision[model_id])}
        price = prices.get(model_id)
        if price:
            entry["price"] = {k: v for k, v in price.items() if v is not None}
            if not entry["price"]:
                entry.pop("price")
        if model_id in prices and prices[model_id].get("display_name"):
            entry["suggested_display_name"] = prices[model_id]["display_name"]
        if entry:
            out[model_id] = entry
    return out