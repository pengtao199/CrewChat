"""网关兜底：把 OpenAI 兼容上游不认的工具定义剥掉。

codex 会把 MCP 服务器（type:"mcp"）和内置工具组（type:"namespace"）放进请求。
LiteLLM 的 Responses→Chat 转换对 "mcp" 原样透传，第三方端点直接 400。
在 acompletion 层（转换之后）过滤一次，所有 Responses API 客户端同时受益。
"""

from typing import Any

from fastapi import Request
from litellm.integrations.custom_logger import CustomLogger
from litellm.proxy._types import UserAPIKeyAuth

ALLOWED_TOOL_TYPES = {"function"}
DROP_TOOL_TYPES = {"mcp", "namespace", "web_search_preview", "custom"}
stripped_total = 0


def _clean_tools(tools: Any, where: str) -> Any:
    global stripped_total
    if not isinstance(tools, list):
        return tools
    kept = [t for t in tools if isinstance(t, dict) and t.get("type") in ALLOWED_TOOL_TYPES]
    dropped = len(tools) - len(kept)
    if dropped:
        stripped_total += dropped
        print(f"[crewchat-gateway] {where}: dropped {dropped}/{len(tools)} tool(s)")
    return kept


class ToolSanitizer(CustomLogger):
    async def async_pre_call_hook(
        self,
        user_api_key_dict: UserAPIKeyAuth | None,
        cache: Any,
        data: dict,
        call_type: str,
    ) -> dict:
        if isinstance(data, dict) and "tools" in data:
            data["tools"] = _clean_tools(data.get("tools"), f"pre_call[{call_type}]")
        return data

    async def async_transform_request(
        self, request: Request, kwargs: Any, user_api_key_dict: Any
    ) -> Any:
        return request


tool_sanitizer = ToolSanitizer()
