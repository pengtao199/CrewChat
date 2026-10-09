"""Skills: the ~/.agents convention shared with codex, claude and opencode."""

from __future__ import annotations

import shutil
from pathlib import Path
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..core.providers import available_skills

router = APIRouter()

_SKILLS_DIR = Path.home() / ".agents" / "skills"
_MAX_FILE_BYTES = 10 * 1024 * 1024
_MAX_TOTAL_BYTES = 200 * 1024 * 1024


def _describe(path: str) -> str:
    """Read the SKILL.md frontmatter description — same field the CLIs use."""
    try:
        text = Path(path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return ""
    if not text.startswith("---"):
        return ""
    end = text.find("\n---", 3)
    front = text[3:end] if end > 0 else text[3:800]
    for line in front.splitlines():
        if line.strip().startswith("description:"):
            value = line.split(":", 1)[1].strip().strip('"').strip("'")
            return value[:300]
    return ""


@router.get("/api/skills")
async def list_skills() -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for item in available_skills():
        path = Path(item["path"])
        out.append(
            {
                "name": item["name"],
                "description": _describe(item["path"]),
                "location": str(path.parent),
                "relative_location": f"skills/{item['name']}",
                "is_auto_inject": False,
                "is_custom": True,
                "source": "custom",
            }
        )
    return out


@router.get("/api/skills/paths")
async def skill_paths() -> dict[str, Any]:
    home = Path.home() / ".agents"
    return {
        "paths": [str(home / "skills")],
        "external_paths": [],
        "import_limits": {"max_file_size": _MAX_FILE_BYTES, "max_files": 200},
    }


@router.get("/api/skills/detect-paths")
async def detect_paths() -> dict[str, Any]:
    home = Path.home()
    candidates = [
        home / ".agents" / "skills",
        home / ".claude" / "skills",
        home / ".codex" / "skills",
    ]
    return {"paths": [str(p) for p in candidates if p.is_dir()]}


@router.post("/api/skills/scan")
async def scan_skills() -> list[dict[str, Any]]:
    return await list_skills()


@router.get("/api/skills/import-limits")
async def import_limits() -> dict[str, int]:
    return {"max_file_bytes": _MAX_FILE_BYTES, "max_total_bytes": _MAX_TOTAL_BYTES}


@router.get("/api/skills/import-history")
async def import_history() -> list[Any]:
    """Import audit trail — not tracked yet."""
    return []


@router.get("/api/skills/detect-external")
async def detect_external() -> list[Any]:
    """External skill sources discovered on disk — none probed yet."""
    return []


@router.get("/api/skills/external-paths")
async def list_external_paths() -> list[Any]:
    return []


@router.post("/api/skills/external-paths")
async def add_external_path() -> dict[str, Any]:
    return {"success": True}


@router.delete("/api/skills/external-paths")
async def remove_external_path() -> dict[str, Any]:
    return {"success": True}


@router.post("/api/skills/info")
async def skill_info(request: Request) -> Any:
    body = await request.json()
    skill_path = Path(str(body.get("skill_path") or "")).expanduser()
    if not skill_path.is_dir():
        return JSONResponse({"error": "not_found"}, status_code=404)
    return {"name": skill_path.name, "description": _describe(str(skill_path / "SKILL.md"))}


@router.post("/api/skills/import")
async def import_skill(request: Request) -> dict[str, Any]:
    """Copy a skill directory into `~/.agents/skills` (same convention the CLIs read)."""
    body = await request.json()
    source = Path(str(body.get("skill_path") or "")).expanduser()
    if not source.is_dir():
        return {"skill_name": "", "failed": [{"source_name": source.name, "code": "not_found"}]}
    destination = _SKILLS_DIR / source.name
    if destination.exists():
        # 已存在即视为成功（幂等），不覆盖用户已有的技能。
        return {"skill_name": source.name}
    _SKILLS_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source, destination)
    return {"skill_name": source.name, "skill_names": [source.name]}


@router.delete("/api/skills/{skill_name}")
async def delete_skill(skill_name: str) -> None:
    """Remove a skill from the shared `~/.agents/skills` directory."""
    if "/" in skill_name or skill_name in {"", ".", ".."}:
        return None
    target = _SKILLS_DIR / skill_name
    if target.is_dir():
        shutil.rmtree(target)
    return None
