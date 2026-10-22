# NOTICE

`crewchat-cli` 是 CrewChat 自有的 Rust 实现，许可 Apache-2.0。

## 参考 / 复制来源

本目录当前是 Phase 0/1 骨架。代码形状参考了以下项目；如果后续直接复制具体文件，
必须保留原版权头，并把对应 commit 记录在这里。

| 项目 | 许可 | 用途 |
| --- | --- | --- |
| [agentclientprotocol/rust-sdk](https://github.com/agentclientprotocol/rust-sdk) | Apache-2.0 | ACP client 骨架（`examples/yolo_one_shot_client.rs`、`v2_one_shot_client.rs`） |
| [openabdev/openab](https://github.com/openabdev/openab) | MIT | `crates/openab-core/src/acp/{connection,pool,protocol}.rs` 的进程/会话池设计 |
| [codeg](https://github.com/spacering-net/codeg)（本仓库 `codeg/`） | Apache-2.0 | `acp/session_state.rs`、`event_stream.rs`、`question.rs`、`plan_approval.rs`、`delegation/`、`agent_host/*`、roster/store 形状 |

## 协议

ACP 协议由 [agentclientprotocol/agent-client-protocol](https://github.com/agentclientprotocol/agent-client-protocol)
（Apache-2.0）定义；运行时依赖其 `agent-client-protocol` crate，不复制协议源码。

## 未 vendor 的第三方

- `claude-code-acp` / `codex-acp` / `opencode acp` 作为外部 CLI 适配器安装/启动，
  不把它们的源码放进本目录。
- `zed-industries/codex-acp` 许可证未明确（NOASSERTION），暂不复制。
