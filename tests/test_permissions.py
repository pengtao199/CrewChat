"""权限闸门：唯一的全局开关「自动批准所有权限请求」。

1. **默认打开**——保持本地桌面 app「不打扰」的默认行为。
2. **关闭时每个询问都弹卡**——不再有档位、强制询问或记忆层。
3. **挂起 / 唤醒**——确认端点能唤醒身体；过期点击不算错误。

用隔离的 data dir，绝不碰真实的 `.data/permissions.jsonc`。
"""

from __future__ import annotations

import asyncio

import pytest

from crewchat.core import config, permissions


@pytest.fixture()
def data_dir(tmp_path, monkeypatch):  # type: ignore[no-untyped-def]
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    monkeypatch.setenv("CREWCHAT_DATA_DIR", str(tmp_path / "data"))
    config.get_settings.cache_clear()
    yield tmp_path / "data"
    config.get_settings.cache_clear()


def _req(tool: str, args: dict | None = None) -> permissions.ToolRequest:
    return permissions.ToolRequest(call_id="c1", tool=tool, detail="", args=args)


# ---- 默认与开关 -------------------------------------------------------------


def test_auto_approve_defaults_on(data_dir) -> None:  # type: ignore[no-untyped-def]
    assert permissions.auto_approve() is True
    assert permissions.decision(_req("Bash", {"command": "rm -rf /"})) == "allow"


def test_switch_off_asks_for_every_request(data_dir) -> None:  # type: ignore[no-untyped-def]
    permissions.set_auto_approve(False)
    assert permissions.auto_approve() is False
    assert permissions.decision(_req("Bash", {"command": "ls"})) == "ask"
    assert permissions.decision(_req("Read", {"path": "/x"})) == "ask"


def test_switch_round_trips_through_the_file(data_dir) -> None:  # type: ignore[no-untyped-def]
    permissions.set_auto_approve(False)
    assert (data_dir / "permissions.jsonc").exists()
    permissions.set_auto_approve(True)
    assert permissions.auto_approve() is True


# ---- 选项分类 ---------------------------------------------------------------


def test_reject_classification() -> None:
    assert permissions.is_reject("reject_once") is True
    assert permissions.is_reject("cancel") is True
    assert permissions.is_reject("allow_once") is False
    assert permissions.is_reject("allow_always") is False


# ---- 挂起 / 唤醒 ------------------------------------------------------------


def test_pending_confirmation_round_trip() -> None:
    async def scenario() -> None:
        future: asyncio.Future[str] = asyncio.get_running_loop().create_future()
        permissions.register_pending("call-1", future)

        assert permissions.resolve_pending("call-1", "allow_once") is True
        assert await future == "allow_once"
        # 只能唤醒一次：第二次是过期点击。
        assert permissions.resolve_pending("call-1", "reject_once") is False

    asyncio.run(scenario())


def test_resolve_unknown_call_id_is_not_an_error() -> None:
    assert permissions.resolve_pending("nope", "allow_once") is False


# ---- 损坏的开关文件不该拦住对话 ---------------------------------------------


def test_corrupt_switch_file_degrades_to_default_on(data_dir) -> None:  # type: ignore[no-untyped-def]
    path = data_dir / "permissions.jsonc"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{ not json at all", encoding="utf-8")

    assert permissions.auto_approve() is True
