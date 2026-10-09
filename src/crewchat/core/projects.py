from __future__ import annotations

import json
import os
import shutil
import tempfile
import uuid
from pathlib import Path
from typing import Any

from .config import Settings


class Project:
    """一个工作区（文件夹）。agent 是全局的，不属于项目。"""

    def __init__(self, id: str, name: str, path: str) -> None:
        self.id = id
        self.name = name
        self.path = path

    def root(self) -> Path:
        return Path(self.path).expanduser()

    def workspace(self) -> Path:
        return self.root()

    def exists(self) -> bool:
        return self.root().is_dir()

    def as_dict(self) -> dict[str, Any]:
        return {"id": self.id, "name": self.name, "path": self.path}

    @classmethod
    def from_dict(cls, raw: dict[str, Any]) -> Project:
        return cls(
            id=str(raw.get("id") or raw.get("name") or "default"),
            name=str(raw.get("name") or raw.get("id") or "default"),
            path=str(raw.get("path") or "."),
        )


def projects_path(settings: Settings) -> Path:
    return settings.data_dir / "projects.json"


def load_projects(settings: Settings) -> list[Project]:
    """项目列表。默认**没有**项目（不是自动拿 cwd 当默认项目）。"""
    path = projects_path(settings)
    if path.exists():
        try:
            raw = json.loads(path.read_text(encoding="utf-8"))
            items = raw.get("projects") if isinstance(raw, dict) else raw
            if isinstance(items, list):
                return [Project.from_dict(p) for p in items if isinstance(p, dict)]
        except (OSError, ValueError):
            pass
    return []
def save_projects(settings: Settings, projects: list[Project]) -> None:
    """原子写回 `.data/projects.json`。"""
    path = projects_path(settings)
    path.parent.mkdir(parents=True, exist_ok=True)
    handle, tmp = tempfile.mkstemp(dir=str(path.parent), suffix=".tmp")
    try:
        with os.fdopen(handle, "w", encoding="utf-8") as fh:
            json.dump(
                {"projects": [p.as_dict() for p in projects]}, fh, ensure_ascii=False, indent=2
            )
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise


def create_project(settings: Settings, name: str, path: str) -> Project:
    """新建项目（工作区 = `path` 指向的文件夹）。agent 全局，不属于项目。"""
    root = Path(path).expanduser()
    if not root.is_dir():
        raise ValueError(f"工作区不存在或不是文件夹：{root}")
    projects = load_projects(settings)
    project = Project(id=uuid.uuid4().hex[:8], name=name.strip() or root.name, path=str(root.resolve()))
    projects.append(project)
    save_projects(settings, projects)
    return project


def update_project(settings: Settings, project_id: str, **fields: Any) -> Project | None:
    projects = load_projects(settings)
    for project in projects:
        if project.id != project_id:
            continue
        if name := str(fields.get("name") or "").strip():
            project.name = name
        if path := str(fields.get("path") or "").strip():
            root = Path(path).expanduser()
            if not root.is_dir():
                raise ValueError(f"工作区不存在或不是文件夹：{root}")
            project.path = str(root.resolve())
        save_projects(settings, projects)
        return project
    return None


def delete_project(settings: Settings, project_id: str) -> bool:
    projects = load_projects(settings)
    remaining = [p for p in projects if p.id != project_id]
    if len(remaining) == len(projects):
        return False
    save_projects(settings, remaining)
    return True


def agents_dir_for(settings: Settings, project_id: str | None = None) -> Path:
    """全局 agent 目录（`<data>/agents`）——agent 是「公司的人」，不归工作区。

    参数 `project_id` 保留只为兼容旧调用，不再影响结果。首次访问时把旧的仓库内
    `config/agents` 迁移过来（幂等，只做一次），避免既有 agent 消失。
    """
    target = settings.agents_home
    target.mkdir(parents=True, exist_ok=True)
    _migrate_legacy_agents(settings, target)
    return target


def _migrate_legacy_agents(settings: Settings, target: Path) -> None:
    marker = settings.data_dir / ".agents-migrated"
    if marker.exists():
        return
    # 旧位置：仓库内 config/agents（架构改版前）；一次性把既有 agent 搬过来。
    legacy = Path("config/agents")
    if legacy.is_dir():
        for source in sorted(legacy.glob("*.jsonc")) + sorted(legacy.glob("*.json")):
            destination = target / source.name
            if not destination.exists():
                shutil.copy2(source, destination)
    marker.write_text("1", encoding="utf-8")


def get_project(settings: Settings, project_id: str | None) -> Project | None:
    if not project_id:
        return None
    for project in load_projects(settings):
        if project.id == project_id:
            return project
    return None