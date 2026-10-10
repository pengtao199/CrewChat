"""Usage: 经我们 app 发起的身体消耗（自研 `/api/crewchat/usage`）。

数据来自我们自己的记账（`store.usage_events`），**不读 cc-switch 的库**：只统计由我们
app 驱动的身体运行，天然排除用户在终端里裸跑 CLI 的用量。见 `AGENTS/truth-sources.md`。
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter

from .store import store

router = APIRouter()


@router.get("/api/crewchat/usage")
async def usage(days: int = 30) -> dict[str, Any]:
    """按天 / 身体 / 模型聚合的 token 与成本。`days` 为回溯窗口。"""
    return store().usage_summary(days)
