from __future__ import annotations

import time
from typing import Any

from openai import AsyncOpenAI

from ..core.config import Settings
from ..core.providers import Provider
from .base import ExecRequest, ExecResult, Executor, register


@register
class LLMBody(Executor):
    id = "llm"
    label = "OpenAI 兼容 LLM"
    has_builtin_brain = False
    kind = "api"

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or Settings()
        self._client: AsyncOpenAI | None = None

    def _resolve(self, provider: Provider) -> dict[str, Any]:
        return {
            "base_url": provider.base_url,
            "model": provider.model,
            "temperature": provider.temperature if provider.temperature is not None else 0.7,
            "max_tokens": provider.max_tokens or 4096,
            "api_key": provider.api_key,
        }

    def _get_client(self, base_url: str, api_key: str, timeout: float) -> AsyncOpenAI:
        if self._client is None:
            self._client = AsyncOpenAI(base_url=base_url, api_key=api_key or "not-set", timeout=timeout)
        return self._client

    async def run(self, req: ExecRequest, provider: Provider | None) -> ExecResult:
        started = time.monotonic()
        if provider is None:
            return ExecResult(
                error="该 Agent 没有大脑：请在 Agent 编排页为它选择模型端点",
                meta={"body": self.id},
            )
        cfg = self._resolve(provider)
        client = self._get_client(cfg["base_url"], cfg["api_key"], req.timeout)

        parts: list[str] = []
        error: str | None = None
        try:
            stream = await client.chat.completions.create(
                model=cfg["model"],
                messages=[{"role": "user", "content": req.prompt}],
                temperature=cfg["temperature"],
                max_tokens=cfg["max_tokens"],
                stream=True,
            )
            async for chunk in stream:
                for choice in chunk.choices:
                    delta = choice.delta.content
                    if delta:
                        parts.append(delta)
                        req.emit("delta", text=delta)
        except Exception as exc:
            error = f"{type(exc).__name__}: {exc}"

        text = "".join(parts).strip()
        return ExecResult(
            text=text,
            duration_ms=int((time.monotonic() - started) * 1000),
            error=error or (None if text else "empty completion"),
            meta={"body": self.id, "model": cfg["model"], "base_url": cfg["base_url"]},
        )

    async def close(self) -> None:
        if self._client is not None:
            await self._client.close()
            self._client = None