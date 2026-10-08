from __future__ import annotations

import time
from typing import Any

from openai import AsyncOpenAI

from ..config import Settings
from ..providers import Provider
from .base import ExecRequest, ExecResult, Executor, register


@register
class LLMBody(Executor):
    id = "llm"
    label = "OpenAI 兼容 LLM"
    has_builtin_brain = False

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or Settings()
        self._client: AsyncOpenAI | None = None

    def _resolve(self, provider: Provider | None) -> dict[str, Any]:
        s = self.settings
        return {
            "base_url": (provider.base_url if provider else None) or s.llm_base_url,
            "model": (provider.model if provider else None) or s.llm_model,
            "temperature": (
                provider.temperature if provider and provider.temperature is not None else s.llm_temperature
            ),
            "max_tokens": (provider.max_tokens if provider else None) or s.llm_max_tokens,
            "api_key": (provider.api_key if provider else None) or s.llm_api_key.get_secret_value(),
        }

    def _get_client(self, base_url: str, api_key: str, timeout: float) -> AsyncOpenAI:
        if self._client is None:
            self._client = AsyncOpenAI(base_url=base_url, api_key=api_key or "not-set", timeout=timeout)
        return self._client

    async def run(self, req: ExecRequest, provider: Provider | None) -> ExecResult:
        started = time.monotonic()
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