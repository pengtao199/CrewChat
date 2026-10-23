# 身体（执行器）

## 定位

产品里的「身体」是一个 CLI 引擎。运行时由 **`cli-layer/`** 负责：

- 引擎注册与版本管理：`cli-layer/src/runtime/engine/`
- ACP 会话与权限：`cli-layer/src/runtime/acp/`
- 每角色 CLI home / 模型注入：`cli-layer/src/runtime/overlay/`
- 花名册的 `agent.agent_type` 引用引擎 id。

要新增或调整一个引擎，改 `cli-layer` 的引擎注册表与 overlay；不要新增第二套编排。

## 原则

- **优先官方适配器**：已有 ACP adapter 直接接，不手写 CLI 私有 JSON 解析。
- **流式是默认**：会话通过 ACP 事件流上报文本、thinking、工具调用、用量。
- **身体不含权限策略**：权限请求由 ACP 发给 cli-layer，再转给 AgentOS 审批。
- **模型由会话注入**：每个角色有私有 CLI home；不写全局 CLI 配置。
- **不复制 CLI 源码**：外部 CLI 作为用户安装的依赖运行。

## 现有引擎

Claude Code、Codex、OpenCode、Gemini、Claw、Cline、Hermes、CodeBuddy、Kimi Code、Pi、Grok、Cursor、DeepSeek、Qoder、Antigravity。具体启动命令、版本 pin 与 overlay 支持见 `cli-layer/src/runtime/engine/registry.rs`。

## 验证

改引擎后至少：

1. `cd cli-layer && cargo test`
2. `acp_list_agents` 能看到该引擎
3. 真实 `acp_connect` + 一轮 prompt 完成
4. 若改 overlay：验证会话实际模型来自角色配置，而不是 CLI 全局配置

历史 Python 执行器契约已归档在 `archive/python-orchestrator/`，不参与运行时。
