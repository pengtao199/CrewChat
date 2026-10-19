"""Projects: 工作区（一个文件夹）= 一等公民，单/多 agent 的工作目录。

自研命名空间 `/api/crewchat/projects`。项目是会话的归属与执行 cwd：`Project.path`
就是 agent 的工作目录（agent 定义是全局的，不归项目）。
"""

from __future__ import annotations

from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..core.config import get_settings
from ..core.projects import (
    Project,
    create_project,
    delete_project,
    load_projects,
    update_project,
)

router = APIRouter()


def _to_api(project: Project) -> dict[str, Any]:
    # agent 全局，不属于项目 —— 项目只表达「工作区目录」。
    return {
        "id": project.id,
        "name": project.name,
        "path": str(project.root()),
        "exists": project.exists(),
    }


@router.get("/api/crewchat/projects")
async def list_projects() -> list[dict[str, Any]]:
    return [_to_api(p) for p in load_projects(get_settings())]


@router.post("/api/crewchat/projects")
async def create(request: Request) -> Any:
    body = await request.json()
    path = str(body.get("path") or "").strip()
    if not path:
        return JSONResponse({"error": "path_required"}, status_code=400)
    try:
        project = create_project(get_settings(), str(body.get("name") or ""), path)
    except ValueError as exc:
        return JSONResponse({"error": str(exc)}, status_code=400)
    return _to_api(project)


@router.get("/api/crewchat/projects/{project_id}")
async def detail(project_id: str) -> Any:
    for project in load_projects(get_settings()):
        if project.id == project_id:
            return _to_api(project)
    return JSONResponse({"error": "not_found"}, status_code=404)


@router.put("/api/crewchat/projects/{project_id}")
async def update(project_id: str, request: Request) -> Any:
    body = await request.json()
    try:
        project = update_project(get_settings(), project_id, **body)
    except ValueError as exc:
        return JSONResponse({"error": str(exc)}, status_code=400)
    if project is None:
        return JSONResponse({"error": "not_found"}, status_code=404)
    return _to_api(project)


@router.delete("/api/crewchat/projects/{project_id}")
async def delete(project_id: str) -> Any:
    if not delete_project(get_settings(), project_id):
        return JSONResponse({"error": "not_found"}, status_code=404)
    return {"success": True}
