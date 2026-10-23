# AgentOS 壳边界

## 适用范围

改动 `AgentOS/`、判断某能力该不该进壳、或壳与核心的边界 / 范围 / 失败降级时。**进壳先读这条。**

## 定位

`AgentOS` 是 macOS SwiftUI 客户端，只做渲染与交互。所有真相与执行都在本机 Rust 服务：

- `crewchat-cli`：会话、权限、花名册、持久化、引擎管理
- `crewchat-model`：模型控制面 + 网关
- `crewchat-skill`：技能库
- `crewchat-mcp`：MCP 注册表与每 Agent proxy

## 壳可以做什么

- 渲染会话 / 流式事件 / 工具调用 / 审批 / 设置页
- 调核心 HTTP 命令，订阅 `/ws/events`
- 保存纯界面偏好（窗口、选中项等）
- 启动/停止四个 sidecar

## 壳不可以做什么

- 不直接读写 `cli-layer.db`、`~/.agents/`、`~/.claude`、`~/.codex`
- 不解析 ACP / 模型 / MCP 协议
- 不持有 provider key
- 不自己实现技能 / MCP 的安装、版本、权限
- 不维护第二份 Agent / 会话 / 模型真相

## 启动

AgentOS 默认启动：

```
crewchat-model (3456/3458)
crewchat-skill (3460)
crewchat-mcp   (3461)
crewchat-cli   (3081)
```

失败语义：

- 技能 / MCP 层失败：聊天仍可用，对应设置页显示不可用。
- 模型层缺失：cli-layer 仍启动，CLI 用自身模型配置。
- cli-layer 失败：壳进入 failed，不再回退到任何旧 core。

## 接口

所有对核心的调用必须登记在 `AgentOS/engine-api.md`。未登记视为越界。

## 检查

- 这份状态是否应该由核心给出，而不是壳缓存？
- 壳是否又解析了一份协议或拼了一份配置？
- 失败时是否明确显示，而不是静默回退？
