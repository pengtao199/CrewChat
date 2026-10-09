"""Filesystem for the WebUI file picker and preview (upstream /api/fs/* contract).

The browser cannot open a native dialog on the server, so the WebUI picker
browses the backend filesystem through these endpoints. The app is a localhost
single-user tool behind the auth gate, so path scoping is the user's own machine.
"""

from __future__ import annotations

import base64
import mimetypes
from pathlib import Path
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

router = APIRouter()

_MAX_READ_BYTES = 5 * 1024 * 1024
_MAX_LIST_ENTRIES = 5000


def _dir_entry(path: Path) -> dict[str, Any]:
    """Accept both snake_case and camelCase — the picker normalizes either."""
    is_dir = path.is_dir()
    full = str(path)
    return {
        "name": path.name,
        "fullPath": full,
        "full_path": full,
        "relativePath": path.name,
        "relative_path": path.name,
        "isDir": is_dir,
        "is_dir": is_dir,
        "isFile": path.is_file(),
        "is_file": path.is_file(),
        "type": "directory" if is_dir else "file",
    }


@router.post("/api/fs/dir")
async def list_dir(request: Request) -> list[dict[str, Any]]:
    body = await request.json()
    directory = Path(str(body.get("dir") or body.get("root") or "")).expanduser()
    if not directory.is_dir():
        return []
    try:
        children = sorted(directory.iterdir(), key=lambda p: (not p.is_dir(), p.name.lower()))
    except OSError:
        return []
    return [_dir_entry(child) for child in children if not child.name.startswith(".")]


@router.post("/api/fs/list")
async def list_files(request: Request) -> list[dict[str, Any]]:
    body = await request.json()
    root = Path(str(body.get("root") or "")).expanduser()
    if not root.is_dir():
        return []
    out: list[dict[str, Any]] = []
    for path in sorted(root.rglob("*")):
        if len(out) >= _MAX_LIST_ENTRIES:
            break
        if not path.is_file():
            continue
        relative = path.relative_to(root)
        if any(part.startswith(".") for part in relative.parts):
            continue
        out.append({"name": path.name, "full_path": str(path), "relative_path": str(relative)})
    return out


@router.post("/api/fs/read")
async def read_file(request: Request) -> Any:
    body = await request.json()
    path = Path(str(body.get("path") or "")).expanduser()
    if not path.is_file():
        return None
    try:
        if path.stat().st_size > _MAX_READ_BYTES:
            return None
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None


@router.post("/api/fs/write")
async def write_file(request: Request) -> bool:
    body = await request.json()
    path = Path(str(body.get("path") or "")).expanduser()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(str(body.get("data") or ""), encoding="utf-8")
        return True
    except OSError:
        return False


@router.post("/api/fs/metadata")
async def metadata(request: Request) -> Any:
    body = await request.json()
    path = Path(str(body.get("path") or "")).expanduser()
    if not path.exists():
        return JSONResponse({"error": "not_found"}, status_code=404)
    st = path.stat()
    return {
        "name": path.name,
        "path": str(path),
        "size": st.st_size,
        "type": "directory" if path.is_dir() else "file",
        "last_modified": int(st.st_mtime * 1000),
        "is_directory": path.is_dir(),
    }


@router.post("/api/fs/image-base64")
async def image_base64(request: Request) -> Any:
    body = await request.json()
    path = Path(str(body.get("path") or "")).expanduser()
    if not path.is_file():
        return None
    try:
        data = path.read_bytes()
    except OSError:
        return None
    if len(data) > _MAX_READ_BYTES:
        return None
    mime = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
    return f"data:{mime};base64,{base64.b64encode(data).decode()}"
