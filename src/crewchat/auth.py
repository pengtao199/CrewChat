from __future__ import annotations

import chainlit as cl

from .config import get_settings
from .overrides import current_settings

DEFAULT_PASSWORD = "crewchat"


def configured_password() -> str:
    return current_settings().local_password or DEFAULT_PASSWORD


@cl.password_auth_callback
async def password_auth(username: str, password: str) -> cl.User | None:
    """本地单用户登录：只校验共享密码，用户名随意。

    这不是安全边界——服务只监听 127.0.0.1。Chainlit 用「是否配置了认证回调」
    来决定是否渲染原生左栏（会话历史 / 搜索 / 新建 / 折叠），所以这里必须注册它。
    """
    if password != configured_password():
        return None

    return cl.User(
        identifier="local",
        display_name="本机用户",
        metadata={"source": "local", "home": str(get_settings().workspace)},
    )