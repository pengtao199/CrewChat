# NOTICE

`crewchat-model` 是 CrewChat 自有的 Rust 实现（MIT）。

## 参考实现

协议映射与 provider quirk 的写法参考了以下 MIT 项目，**本目录没有直接复制其源码**；
如果后续 vendor 具体文件，必须把对应 LICENSE 与 commit 记录在这里。

| 项目 | 许可 | 参考内容 |
| --- | --- | --- |
| [musistudio/claude-code-router](https://github.com/musistudio/claude-code-router)（本仓库 `ccr/`） | MIT | CCR 的 RPC 形状、`Providers` / `APIKEYS` / `configRevision` 契约、Claude 虚拟 ID 与 OpenCode quirk |
| [raine/claude-code-proxy](https://github.com/raine/claude-code-proxy) | MIT | Anthropic Messages ↔ OpenAI 的边界（工具调用、SSE、`x-opencode-session`、OpenCode Go Responses/Chat 端点） |

## 第三方依赖

Rust crates 的许可见 `Cargo.lock`；主要依赖：`axum`、`tokio`、`reqwest`、`rusqlite`、
`serde`。全部为 MIT / Apache-2.0 兼容许可。
