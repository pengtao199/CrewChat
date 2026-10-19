# AgentOS 传输与事件

## 适用范围

新增 `AgentOS/` 对核心的接口调用、订阅事件、处理流式 / 重连 / 取消 / 错误时。

## 目标

AgentOS 与核心之间只有一条通道、一套契约；流式与重连的正确性只在 `Core/` 里管一次。

## 命令调用

- 形状：`POST http://127.0.0.1:<port>/api/<command>`，JSON 进 JSON 出；`Authorization: Bearer <token>`。
- 命令名与核心的 Tauri command 同名（snake_case）；不改大小写、不加前缀。
- 超时默认 60s（核心的 ACP 探测最长约 60s），按调用可覆盖。
- 错误统一解成壳内错误类型：`code` / `message` / `detail?` / `i18n_key?` / `i18n_params?`；有 `i18n_key` 时优先本地化文案，否则退回 `message`。
- `turn_in_progress`（HTTP 409）是预期状态（同一会话已有轮次在跑），按其语义呈现，不弹错误。
- 每条调用的用途与注意点（幂等 / 超时 / 409）写进 `AgentOS/engine-api.md` 登记行。

## 事件订阅（attach 协议）

- 连接 `ws://127.0.0.1:<port>/ws/events`，通过 `Sec-WebSocket-Protocol` 带 token（见 `agentos-boundary.md`）。
- 一个 WS 多路复用多个订阅：发 `attach { subscription_id, connection_id, cursor? }` / `detach { subscription_id }`。
- 服务端帧：`snapshot`（初始状态）/ `replay`（按游标补发）/ `event`（单条增量，带 `envelope.seq`）/ `detached`（需重新 attach）/ `pong`。
- **`seq` 是去重与陈旧判定的唯一锚点**：`seq ≤ 已应用值` 丢弃；同一连接单调递增。
- **`snapshot` 覆盖本地累计状态**：以它为基准重建，不做增量合并猜测。
- 不订阅全局 firehose（旧通道，勿用）。

## 重连

- 掉线视为暂时性：指数退避 1s → 2s → 4s → … 上限 32s，无限重试，不丢 token、不跳登录。
- 连接恢复后重新发起 attach（带游标），由核心决定回 `replay` 还是 `snapshot`。
- 收到 `detached` 立即重新 attach，不用本地状态猜核心是否还有数据。

## 流式与取消

- 文本增量按 `event` 顺序累加到当前轮次；未完成内容标记为进行中，不写入任何持久化（壳不持久化会话）。
- 工具调用、用量、状态变化都是事件，渲染成对应卡片；不得在壳里重算用量或成本。
- 取消调核心的取消命令；取消后以核心回传的最终状态为准，不本地直接置为已取消。

## 禁止项

- 不做协议重试之外的逻辑重试（如「这条命令失败就换一条再试」）。
- 不在 `Features/` 里开 socket 或发请求；一律经 `Domain` 端口 → `Core` 实现。
- 不自行聚合跨会话数据后再判定「可用 / 不可用」。

## 检查

- 新增的每个调用都登记进 `AgentOS/engine-api.md` 了吗？
- 断线重连后状态靠 snapshot / replay 自愈，而不是靠界面猜吗？
- 取消、超时、409 三条路径都处理了吗？
