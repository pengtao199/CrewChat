from .base import ExecEvent, ExecRequest, ExecResult, Executor, get_executor, list_executors, register
from .llm_body import LLMBody
from .acp_body import AcpBody, register_acp_bodies

# 所有 CLI 身体都经 ACP（见 archive/README.md）。bespoke 的 claude/codex/opencode
# 已归档、不再注册；`llm` 是直连，不是 CLI，保留。
register_acp_bodies()

__all__ = [
    "ExecEvent",
    "ExecRequest",
    "ExecResult",
    "Executor",
    "LLMBody",
    "AcpBody",
    "get_executor",
    "list_executors",
    "register",
]
