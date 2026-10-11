"""模型元数据：静态知识 + 降级说明。

历史上这里通过 cc-switch 的控制口（`ccsw-proxy`）拉取视觉能力登记表与
models.dev 价格目录。**cc-switch 网关已随模型层统一到 CCR 而移除**，因此：

- 价格与实时能力判定不再由 Python 侧提供（改由 CCR / Codeg 侧承担）；
- 本模块只保留与端点无关的**静态知识**：命名约定标签、常见上下文窗口；
- 两个历史入口保留签名并显式降级，调用方（`providers_api`）无需改动。
"""

from __future__ import annotations

from typing import Any

from .config import Settings

# ---------------------------------------------------------------------------
# 兼容入口（显式降级）
# ---------------------------------------------------------------------------


def fetch_provider_models(settings: Settings, provider: Any) -> list[dict[str, Any]]:
    """历史入口：曾复用 cc-switch 的 `model_fetch` 拉取供应商模型。

    cc-switch 已移除；模型发现请使用 CCR 的探活 / Codeg 的「获取可用模型」。
    显式抛错让调用方把原因转成 502 / `success: false` 的消息，而不是静默空。
    """
    del settings, provider
    raise RuntimeError(
        "cc-switch 网关已移除；模型发现请改用 CCR（探活 / Agent Config）"
    )


def enrich_models(settings: Settings, model_ids: list[str]) -> dict[str, dict[str, Any]]:
    """历史入口：曾用 cc-switch 补全 能力 / 价格 / 显示名。

    现在只做与端点无关的静态补全（已知上下文窗口）；其余字段不再提供，
    调用方已有的 `.get(...)` 兜底会自然降级。
    """
    del settings
    out: dict[str, dict[str, Any]] = {}
    for model_id in model_ids:
        limit = known_context_limit(model_id)
        if limit:
            out[model_id] = {"context_limit": limit}
    return out


# ---------------------------------------------------------------------------
# 自动打标签（静态知识，与端点无关）
# ---------------------------------------------------------------------------
#
# 用户不该手工给每个模型贴标签。判定顺序（先命中先用）：
#   1. 命名约定（向量 / 视觉 / 推理 / 工具 / 快速 / 长上下文）
#   2. 已知上下文窗口兜底
#   3. 文本是所有模型的基础能力

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

    `enriched` 是历史补全结果的形状（capabilities / price），有就采信；
    没有则退回命名约定。用户手工打的标签由调用方合并进来，不在这里覆盖。
    """
    enriched = enriched or {}
    tags: list[str] = []

    # 0. 向量模型单独归类：它既不是文本生成，也不支持对话/工具
    if _has(model.id, "embed", "bge-", "gte-", "e5-", "jina-", "nomic-embed", "minilm"):
        return ["embedding"]

    # 1. 视觉：补全结论优先于命名
    vision = (enriched.get("capabilities") or {}).get("vision")
    if vision is None:
        vision = (model.capabilities or {}).get("vision")
    if vision is True:
        tags.append("vision")

    # 2. 免费：两端都为 0（`:free` 后缀也算）
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
    from .providers import MODEL_TAGS

    return [t for t in MODEL_TAGS if t in tags]
