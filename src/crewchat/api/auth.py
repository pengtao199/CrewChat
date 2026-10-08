"""Auth endpoints the forked UI expects: /api/auth/user, /login, /logout, /api/auth/refresh.

Single local user, shared password — the app only listens on 127.0.0.1, so this
is a gate, not a security boundary.
"""

from __future__ import annotations

import secrets
import time
from http.cookies import SimpleCookie
from typing import Any

from fastapi import APIRouter, Request, Response
from fastapi.responses import JSONResponse
from pydantic import BaseModel

from .store import local_password, local_username, user_payload

ACCESS_COOKIE = "access_token"
REFRESH_COOKIE = "refresh_token"
ACCESS_TTL = 60 * 60 * 24
REFRESH_TTL = 60 * 60 * 24 * 30

_sessions: dict[str, float] = {}

router = APIRouter()


class LoginBody(BaseModel):
    username: str | None = None
    password: str | None = None
    remember: bool | None = False


def _issue(response: Response, remember: bool) -> None:
    token = secrets.token_urlsafe(32)
    _sessions[token] = time.time() + (REFRESH_TTL if remember else ACCESS_TTL)
    response.set_cookie(ACCESS_COOKIE, token, max_age=ACCESS_TTL, httponly=True, samesite="lax")
    response.set_cookie(
        REFRESH_COOKIE,
        token,
        max_age=REFRESH_TTL if remember else ACCESS_TTL,
        httponly=True,
        samesite="lax",
        path="/api/auth/refresh",
    )


def _token(request: Request) -> str | None:
    raw = request.headers.get("cookie")
    if not raw:
        return None
    cookie = SimpleCookie()
    cookie.load(raw)
    morsel = cookie.get(ACCESS_COOKIE)
    return morsel.value if morsel else None


def current_user(request: Request) -> dict[str, Any] | None:
    if not _sessions:
        return None
    token = _token(request)
    if token and _sessions.get(token, 0) > time.time():
        return user_payload()
    return None


@router.get("/api/auth/user")
async def auth_user(request: Request) -> JSONResponse:
    user = current_user(request)
    if user is None:
        return JSONResponse({"success": False}, status_code=401)
    return JSONResponse({"success": True, "user": user})


@router.post("/login")
async def login(body: LoginBody) -> JSONResponse:
    if (body.password or "") != local_password():
        return JSONResponse({"success": False, "message": "用户名或密码不正确"}, status_code=401)
    response = JSONResponse({"success": True, "user": user_payload()})
    _issue(response, bool(body.remember))
    return response


@router.post("/logout")
async def logout(request: Request) -> JSONResponse:
    token = _token(request)
    if token:
        _sessions.pop(token, None)
    response = JSONResponse({"success": True})
    response.delete_cookie(ACCESS_COOKIE)
    response.delete_cookie(REFRESH_COOKIE, path="/api/auth/refresh")
    return response


@router.post("/api/auth/refresh")
async def refresh(request: Request) -> JSONResponse:
    raw = request.headers.get("cookie") or ""
    cookie = SimpleCookie()
    cookie.load(raw)
    morsel = cookie.get(REFRESH_COOKIE) or cookie.get(ACCESS_COOKIE)
    token = morsel.value if morsel else None
    if not token or _sessions.get(token, 0) <= time.time():
        return JSONResponse({"success": False}, status_code=401)
    response = JSONResponse({"success": True})
    response.set_cookie(ACCESS_COOKIE, token, max_age=ACCESS_TTL, httponly=True, samesite="lax")
    return response


def reset_sessions() -> None:
    _sessions.clear()
