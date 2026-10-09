"""版本号单一真相源。

后端使用递增格式 ``v1`` / ``v2`` / ``v3``；前端使用语义化版本 ``1.0.0``。
两者独立演进，不互相绑定。

后端版本号只在这里定义一次。所有对外暴露（API、诊断、启动日志）都必须读取
本模块，禁止在其他文件硬编码版本字符串。
"""

from __future__ import annotations

import re
from typing import Any

# 后端当前版本。递增格式：发布新版本时只改这一个数字。
# 0.x 视为 v0（开发/预览期），首个正式版本为 v1。
_BACKEND_VERSION = 1

# 校验用：递增格式只允许 v + 正整数。
_VERSION_RE = re.compile(r"^v(\d+)$")

# 前端语义化版本。仅供后端在版本接口里回显，方便前端做一致性核对；
# 前端自身以 ui/packages/desktop/package.json 为真相源。
_FRONTEND_VERSION = "1.0.0"


def backend_version() -> str:
    """返回后端版本号，形如 ``v1``。"""
    return f"v{_BACKEND_VERSION}"


def backend_version_number() -> int:
    """返回后端版本号整数，便于比较与递增。"""
    return _BACKEND_VERSION


def frontend_version() -> str:
    """返回前端语义化版本号，形如 ``1.0.0``。"""
    return _FRONTEND_VERSION


def is_valid_backend_version(value: str) -> bool:
    """校验字符串是否为合法的后端版本号（``v`` + 正整数）。"""
    match = _VERSION_RE.match(value)
    return bool(match and int(match.group(1)) >= 1)


def version_info() -> dict[str, Any]:
    """聚合版本信息，供 ``/api/crewchat/version`` 使用。"""
    backend = backend_version()
    return {
        "backend": {
            "version": backend,
            "number": _BACKEND_VERSION,
            "scheme": "increment",
        },
        "frontend": {
            "version": _FRONTEND_VERSION,
            "scheme": "semver",
        },
    }