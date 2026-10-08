from __future__ import annotations

from pathlib import Path
from typing import Any

from crewai.tools import BaseTool
from pydantic import BaseModel, Field

from .executors import ExecRequest, get_executor


class BodyInput(BaseModel):
    task: str = Field(description="要交给这个 CLI agent 完成的完整任务描述，包含必要的上下文与验收标准")


class BodyTool(BaseTool):
    """把一个 CLI 身体包装成 crewai 工具：大脑负责决策，这个工具负责执行。

    CLI 自带工具链/沙箱/上下文，所以对外只暴露一个「把活交出去」的动作，
    避免外层大脑去做它不擅长的细粒度规划。
    """

    model_config = {"arbitrary_types_allowed": True}

    name: str
    description: str
    args_schema: type[BaseModel] = BodyInput
    body: str
    brain: Any = None
    cwd: str = ""
    executor: Any = None

    def model_post_init(self, __context: Any) -> None:
        if self.executor is None:
            self.executor = get_executor(self.body)

    def _run(self, task: str, **kwargs: Any) -> str:  # type: ignore[override]
        import asyncio

        request = ExecRequest(
            prompt=task,
            cwd=Path(self.cwd).expanduser() if self.cwd else Path.cwd(),
            timeout=1800.0,
            session=f"body:{self.body}",
        )
        result = _run_sync(self.executor.run(request, self.brain))
        if result.error:
            return f"[{self.body}] 执行失败：{result.error}"
        return result.text or f"[{self.body}] 无输出"


def _run_sync(coro: Any) -> Any:
    try:
        asyncio.get_running_loop()
    except RuntimeError:
        return asyncio.run(coro)
    import concurrent.futures

    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
        return pool.submit(asyncio.run, coro).result()


def build_body_tool(spec: Any, cwd: str) -> BodyTool:
    executor = get_executor(spec.body)
    return BodyTool(
        name=f"delegate_to_{spec.body}",
        description=(
            f"把完整任务交给 {executor.label} 执行。它自带文件读写、命令执行与沙箱，"
            f"会把完成后的结果、改动摘要和自检结论返回给你。"
            f"给它完整上下文和明确的验收标准，不要拆成细碎步骤。"
        ),
        body=spec.body,
        brain=spec.llm,
        cwd=cwd,
    )