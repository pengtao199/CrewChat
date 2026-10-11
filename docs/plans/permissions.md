# 权限：一个全局开关「自动批准所有权限请求」

Updated: 2026-10-11 (Asia/Shanghai)
Status: Implemented（后端 + 设置页）。管道已通并有用例覆盖；**但没有 CLI 会真的发问**，见 Current Facts。

只做权限。不碰思考流、正文流式契约与 CLI 元数据警告（那是 [cli-output-noise.md](cli-output-noise.md)）。

## Goal

一个开关决定 Agent 调工具时要不要打扰用户：打开则不弹卡、直接放行；关闭则每次询问。

## Current Facts

- **fork 之前上游就是全自动放行**，等价于各 CLI 的 `--auto`。本设计把这个行为显式化成一个用户可见的开关，默认保持打开（不打扰）。
- **前端权限卡 UI 一直是完整的**（fork 自带）：`MessagePermission/index.tsx` + `PermissionRequestPanel.tsx`，`IConfirmation` 类型，确认端点也在 `ipcBridge.ts` 注册。
- **管道已通并验证。** `_AcpClient.request_permission` 根据 `core/permissions.py` 的 `decision()` 决定放行或挂起 → 发 `permission` 事件 → WS `permission` 帧（`data.call_id` + `data.options[].value`，value 用 ACP 自己的 kind）→ 用户 `POST .../confirmations/{call_id}/confirm` → 唤醒挂起中的身体。
- **⚠️ 但没有任何 CLI 真的发来过 `session/request_permission`。** 实测 `claude-agent-acp` 二进制里没有这个方法；`codex-acp` 有，但当前配置（`_codex_env` / `_claude_env` 没设审批策略）下不发。**关掉开关目前不会在真实对话里弹窗**，这一点必须让用户知道。

## Non-Goals

- 不按目录 / 文件路径 / 命令模式授权（已移除「总是允许」记忆层）。
- 不保留 per-agent 权限档位（`AgentSpec.permission` 已删除）。
- 不做审计日志。

## Design

- **一个开关**：`.data/permissions.jsonc` 的 `auto_approve_all`，默认 `true`。
- **唯一判定点**：`core/permissions.py` 的 `decision(request)` → 开关打开返回 `allow`，否则 `ask`。身体与 API 不再各判一次。
- **弹卡链路不变**：`_AcpClient._ask` 挂起等确认；`permission` 帧 / `reject_*` 分类 / `resolve_pending` 都在 `core/permissions.py` 与 `api/chat.py`。

## API

- `GET /api/v1/crewchat/permissions` → `{"auto_approve": bool}`
- `PUT /api/v1/crewchat/permissions` body `{"auto_approve": bool}` → 同一形状

## UI

- 设置 → 系统：一行「自动批准所有权限请求」开关（Arco `Switch`），标注保存到 `.data/permissions.jsonc`。

## Work Slices

- **策略 + 记忆 → 单开关**：`core/permissions.py` 收缩成一个开关 + 挂起表 + 选项分类。
- **ACP 闸门 + 帧 + 确认端点**：`_AcpClient` 按 `decision()` 放行或 `_ask`；`hub.permission_frame`；`chat.py` 广播与确认端点。
- **设置页**：系统页开关 + `/api/crewchat/permissions`。
- **让 CLI 真的发问（未做）**：逐个 CLI 改启动配置（codex 的审批策略等），让它在关闭开关时真的发 `session/request_permission`。

## Verification

- `uv run pytest -q` → **111 passed**（`tests/test_permissions.py` 覆盖默认打开 / 关闭询问 / 挂起唤醒 / 损坏文件降级；`tests/test_api_endpoints.py` 覆盖 permission 帧形状、确认端点、过期点击 200、设置往返）。
- `uv run python scripts/check_layers.py` → 通过。
- `cd ui && npx vite build` → 通过。
- 服务实跑：`GET/PUT /api/v1/crewchat/permissions` 落盘 `.data/permissions.jsonc` 正确。
