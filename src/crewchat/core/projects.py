from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .config import Settings


class Project:
    def __init__(self, id: str, name: str, path: str, agents_dir: str = "config/agents") -> None:
        self.id = id
        self.name = name
        self.path = path
        self.agents_dir = agents_dir

    def root(self) -> Path:
        return Path(self.path).expanduser()

    def agents_path(self) -> Path:
        agents = Path(self.agents_dir).expanduser()
        return agents if agents.is_absolute() else self.root() / agents

    def workspace(self) -> Path:
        return self.root()

    def exists(self) -> bool:
        return self.root().is_dir()

    def as_dict(self) -> dict[str, Any]:
        return {"id": self.id, "name": self.name, "path": self.path, "agents_dir": self.agents_dir}

    @classmethod
    def from_dict(cls, raw: dict[str, Any]) -> Project:
        return cls(
            id=str(raw.get("id") or raw.get("name") or "default"),
            name=str(raw.get("name") or raw.get("id") or "default"),
            path=str(raw.get("path") or "."),
            agents_dir=str(raw.get("agents_dir") or "config/agents"),
        )


def projects_path(settings: Settings) -> Path:
    return settings.data_dir / "projects.json"


def load_projects(settings: Settings) -> list[Project]:
    path = projects_path(settings)
    if path.exists():
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
            items = raw.get("projects") if isinstance(raw, dict) else raw
            if isinstance(items, list) and items:
                return [Project.from_dict(p) for p in items if isinstance(p, dict)]
        except (OSError, ValueError):
            pass
    return [Project(id="default", name=Path.cwd().name, path=str(Path.cwd()))]
def get_project(settings: Settings, project_id: str | None) -> Project:
    projects = load_projects(settings)
    if project_id:
        for project in projects:
            if project.id == project_id:
                return project
    return projects[0]