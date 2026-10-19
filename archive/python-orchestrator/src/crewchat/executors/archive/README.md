# 归档：手写的逐个 CLI 适配器

这里放的是**已停用**的 bespoke 适配器，保留作参考，不参与运行。

## 为什么归档

它们各自解析一个 CLI 的私有输出格式：

| 文件 | 私有格式 |
| --- | --- |
| `claude_body.py` | `claude-agent-sdk` + claude 的 `--stream-json` 事件 |
| `codex_body.py` | `openai-codex-sdk` + codex `exec` 的 thread 事件 |
| `opencode_body.py` | `opencode run --format json` 的私有 JSON |

问题不在于写得好不好，而在于**每加一个 CLI 就要再写一份**：私有旗标、私有事件、
私有会话机制、再实测一遍。`AGENTS/bodies.md` 也明确反对"自己解析 CLI 的私有 JSON"。

## 现在用什么

统一的 [ACP（Agent Client Protocol）](../acp_body.py)：协议只有一份，任何讲 ACP 的
CLI 都直接是身体。加一个 CLI = 在 `acp_body.py` 的 `ACP_CLIS` 加一行启动命令，
不写解析、不写会话、不逐个测。

这些文件不再被 `executors/__init__.py` 导入，`test_usage.py` 里针对它们的单测
一并停用（测的是已归档的实现）。
