from .base import ExecEvent, ExecRequest, ExecResult, Executor, get_executor, list_executors, register
from .acp_body import AcpBody, register_acp_bodies

# 所有身体都经 ACP（见 archive/README.md）。bespoke 的 claude/codex/opencode
# 已归档、不再注册；不再保留直连 API 身体。
register_acp_bodies()

__all__ = [
    "ExecEvent",
    "ExecRequest",
    "ExecResult",
    "Executor",
    "AcpBody",
    "get_executor",
    "list_executors",
    "register",
]
