"""自研命名空间 `/api/crewchat/permissions`：唯一的全局开关「自动批准所有权限请求」。

判定只有一处：`core/permissions.py` 的 `decision()`。本文件只做读写与 HTTP 形状。
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter

from ..core import permissions

router = APIRouter()


def _state() -> dict[str, Any]:
    return {"auto_approve": permissions.auto_approve()}


@router.get("/api/crewchat/permissions")
async def get_permissions() -> dict[str, Any]:
    return _state()


@router.put("/api/crewchat/permissions")
async def update_permissions(body: dict[str, Any]) -> dict[str, Any]:
    """只认布尔字段 `auto_approve`，其余忽略。返回改完之后的完整状态。"""
    if "auto_approve" in body:
        permissions.set_auto_approve(bool(body.get("auto_approve")))
    return _state()
