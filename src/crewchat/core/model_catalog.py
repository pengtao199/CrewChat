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
from .providers import MODEL_TAGS, Model

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

    返回 `{model_id: {"capabilities": {...}, "price": {...}, "display_name": ...,
    "context_limit": ..., "tags": [...]}}`，只包含确实拿到的字段。
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


# ---------------------------------------------------------------------------
# 自动打标签
# ---------------------------------------------------------------------------
#
# 用户不该手工给每个模型贴标签 —— 能力、价格、上下文都能从已有数据源推出来。
# 判定顺序（先命中先用，越靠前越可靠）：
#   1. cc-switch 视觉能力登记表 → vision
#   2. models.dev 价格目录     → free（两端都为 0）
#   3. 模型 id 命名约定        → 其余标签
# 命名约定只作为**兜底**：cc-switch 判定与价格目录优先，避免和真实数据打架。

# 取模型 id 的末段：`nvidia/xxx:free` → `xxx:free`
def _tail(model_id: str) -> str:
    return model_id.rsplit("/", 1)[-1].strip().lower()


def _has(model_id: str, *needles: str) -> bool:
    tail = _tail(model_id)
    return any(n in tail for n in needles)


#: 命名约定 → 标签。命中即打标签。
_NAME_RULES: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("embedding", ("embed", "bge-", "gte-", "e5-", "jina-", "nomic-embed", "minilm")),
    ("vision", ("vision", "-vl", "vl-", "image", "multimodal", "gpt-4o", "gemini-1.5", "gemini-2", "pixtral", "llava", "qwen-vl", "qwen2-vl", "internvl")),
    ("reasoning", ("o1", "o3", "o4", "-r1", "reason", "thinking", "qwq", "deepseek-r1", "magistral-medium", "gpt-5")),
    ("tool_use", ("gpt-4", "gpt-5", "o1", "o3", "o4", "claude", "sonnet", "haiku", "opus", "gemini", "qwen", "deepseek", "mistral", "command-r", "firefunction", "nemotron", "agnes")),
    ("fast", ("flash", "mini", "haiku", "nano", "turbo", "small", "lightning", "lite", "-8b", "-7b", "instant", "-fast", "faster")),
    ("long_context", ("gpt-4.1", "gemini-1.5-pro", "megatron", "128k", "200k", "1m")),
)

#: 常见模型的上下文窗口。命名约定判断不出上下文长度时用它兜底。
#: key 用末段模型名匹配，值是窗口大小（token）。
_KNOWN_CONTEXT: tuple[tuple[str, int], ...] = (
    ("gpt-4.1", 1_047_576),
    ("gpt-4o", 128_000),
    ("gpt-4-turbo", 128_000),
    ("gpt-4", 8_192),
    ("o1", 200_000),
    ("o3", 200_000),
    ("o4", 200_000),
    ("gpt-5", 400_000),
    ("claude-3", 200_000),
    ("claude-sonnet-4", 1_000_000),
    ("claude-opus-4", 200_000),
    ("claude-haiku-4", 200_000),
    ("gemini-1.5-pro", 2_097_152),
    ("gemini-2", 1_048_576),
    ("gemini-2.5", 1_048_576),
    ("deepseek-chat", 128_000),
    ("deepseek-reasoner", 128_000),
    ("qwen3", 131_072),
    ("mistral-large", 131_072),
    ("command-r-plus", 128_000),
    ("llama-3.1", 131_072),
    ("llama-3.3", 131_072),
    ("nemotron", 131_072),
    ("agnes", 131_072),
)


def known_context_limit(model_id: str) -> int | None:
    """从已知的模型族推断上下文窗口。"""
    tail = _tail(model_id)
    for name, limit in _KNOWN_CONTEXT:
        if name in tail:
            return limit
    return None


def infer_tags(model: Any, enriched: dict[str, Any] | None = None) -> list[str]:
    """给一个模型推导标签。

    `enriched` 是 cc-switch 补全的结果（capabilities / price），有它就优先采信真实
    数据源；没有则退回命名约定。用户手工打的标签由调用方合并进来，不在这里覆盖。
    """
    enriched = enriched or {}
    tags: list[str] = []

    # 0. 向量模型单独归类：它既不是文本生成，也不支持对话/工具
    if _has(model.id, "embed", "bge-", "gte-", "e5-", "jina-", "nomic-embed", "minilm"):
        return ["embedding"]

    # 1. 视觉：cc-switch 的登记表结论优先于命名
    vision = (enriched.get("capabilities") or {}).get("vision")
    if vision is None:
        vision = (model.capabilities or {}).get("vision")
    if vision is True:
        tags.append("vision")

    # 2. 免费：models.dev 目录里两端都是 0（`:free` 后缀也算）
    price = enriched.get("price") or model.price or {}
    input_cost, output_cost = price.get("input"), price.get("output")
    if _tail(model.id).endswith(":free"):
        tags.append("free")
    elif input_cost == 0 and output_cost == 0:
        tags.append("free")

    # 3. 其余按命名约定
    for tag, needles in _NAME_RULES:
        if tag == "vision" and vision is True:
            continue  # 已在第 1 步加过
        if tag == "embedding":
            continue  # 已在第 0 步返回
        if _has(model.id, *needles):
            tags.append(tag)

    # 4. 上下文长度兜底：200k 以上才算长上下文（128k 已是主流，标满就没有区分度）
    context_limit = enriched.get("context_limit") or model.context_limit or known_context_limit(model.id)
    if context_limit and context_limit >= 200_000 and "long_context" not in tags:
        tags.append("long_context")

    # 5. 文本是所有模型的基础能力；只有具备其它能力时才补，避免满屏同质标签
    if len(tags) == 1 and tags[0] == "vision":
        tags.append("text")
    elif not tags:
        tags.append("text")

    # 固定顺序输出，保证 UI 稳定
    return [t for t in MODEL_TAGS if t in tags]