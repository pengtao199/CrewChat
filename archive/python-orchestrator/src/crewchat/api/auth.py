"""Local single user — authentication is intentionally absent.

CrewChat ships as a local desktop app (loopback only), so there is no login:
every request is the one local user. `/api/auth/user` is kept because the forked
UI reads it during startup.
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter

from .store import user_payload

router = APIRouter()


def current_user(request: Any = None) -> dict[str, Any]:
    return user_payload()


@router.get("/api/auth/user")
async def auth_user() -> dict[str, Any]:
    return {"success": True, "user": user_payload()}
