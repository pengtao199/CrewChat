"""Skills: the ~/.agents convention shared with codex, claude and opencode."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from fastapi import APIRouter

from ..core.providers import available_skills

router = APIRouter()


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
        "import_limits": {"max_file_size": 10 * 1024 * 1024, "max_files": 200},
    }


@router.post("/api/skills/detect-paths")
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
