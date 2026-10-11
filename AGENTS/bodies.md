# 身体（执行器）

## 适用范围

新增或修改执行器（`codex` / `claude` / `opencode`），或处理 CLI 版本兼容、流式事件、会话记忆时。

## 目标

每个身体都以最小适配层复用厂商官方能力，对外暴露同一套事件流，新增身体不改编排层。

## 契约

```python
class Executor(ABC):
    id: ClassVar[str]                  # 身体标识，与 AgentSpec.body 一致
    label: ClassVar[str]               # 界面展示名
    has_builtin_brain: ClassVar[bool]  # 是否自带大脑

    @classmethod
    def available(cls) -> bool: ...     # 本机是否可用

    async def run(self, req: ExecRequest, provider: Provider | None) -> ExecResult: ...

    async def close(self) -> None: ...  # 释放长连接（可选）
```

- 输入 `ExecRequest`：`prompt` / `cwd` / `timeout` / `session`（记忆键）/ `on_event`。
- 输出 `ExecResult`：`text` / `error` / `session` / `cost_usd` / `num_turns` / `meta`。
- 过程通过 `req.emit(kind, ...)` 上报：`delta`（增量文本）、`thinking`、`tool`、`notice`、`permission`、`error`。

## 原则

- **优先官方 SDK**：能用厂商官方库就不用 subprocess 解析标准输出。已采用的官方件：`claude-agent-sdk`、`openai-codex-sdk`、`openai`。
- **官方件优先于自研适配**：只有当官方件不可用（alpha、版本不匹配、违背协议）时才回退到官方 CLI，并把原因写在文件头。
- **流式是默认**：所有身体都应上报 `delta`，让界面能逐字渲染；一次性返回是降级方案。
- **记忆用厂商的会话机制**：`session` 参数传给 CLI/SDK 自己的续接能力，不自建对话历史重放。
- **身体不含权限策略**：`request_permission` 里不写「哪些工具危险」这类判断，只把请求交给 `core/permissions.py` 的 `decision()`。开关的取舍不该要求改每个身体。

## 权限：管道已通，但 CLI 默认不问

2026-10-10 实测的边界，改权限相关代码前先知道：

- **我们的管道是通的。** `_AcpClient.request_permission` → `permission` 事件 → WS
  `permission` 帧 → `POST .../confirmations/{call_id}/confirm` → 唤醒挂起的身体。
  这条链有用例覆盖（`tests/test_permissions.py`），且闸门本身可独立跑通。
- **但没有任何 CLI 真的发来过 `session/request_permission`。** 实测：`claude-agent-acp`
  二进制里根本没有这个方法的痕迹；`codex-acp` 有（`session/request_permission` /
  `request_permissions`），但在当前配置下也不发——它的审批策略没设，默认
  不问；claude 同理。所以关掉「自动批准」目前**不会**在真实对话里弹窗。
- **要让某个 CLI 开始问，要改它自己的用户配置**（codex 的审批策略、claude 的权限模式），
  这是逐个 CLI 的事，属于本文件「每个 CLI 的私有部分」。**不要**用「反正点了也白点」
  的理由把管道删掉——管道是 CLI 一支持就能用的。
- `AllowedOutcome` 必须同时给 `outcome="selected"` 和 `option_id`。只给 `option_id`
  会在构造时 pydantic 报错；这行以前写错一直没暴露，正是因为从没被调用过。

## 具体规则

- 每个身体一个文件，命名 `<name>_body.py`，用 `@register` 注册。
- 内部实现细节（参数拼装、事件解析）留在该文件内，不泄漏到 `base.py` 或编排层。
- 与版本相关的兼容处理必须写在文件内并注明依据，例如 codex 的 `--experimental-json` 与 `--json` 差异。
- 错误必须转成可读文本放进 `ExecResult.error`，不抛给编排层（编排层只做转发）。
- 新增身体时同步更新：`AGENTS/agent-model.md` 的身体清单、CLI 身体页面的探测项。

## 现有身体

**所有 CLI 身体统一走 ACP**（`executors/acp_body.py`）。加一个 CLI = 在 `ACP_CLIS`
加一行它的 acp 启动命令，不写适配器、不逐个实测。bespoke 的 `claude/codex/opencode`
适配器已归档到 `executors/archive/`（各自解析一个 CLI 的私有 JSON，违反本文件"不自行
发明协议"的禁止项）。

| id | acp 启动命令 | 记忆 |
| --- | --- | --- |
| `opencode` | `opencode acp` | ACP session |
| `openclaw` | `openclaw acp` | ACP session |
| `claude` | `claude-agent-acp` | ACP session |
| `codex` | `codex-acp` | ACP session |
| `gemini` | `gemini --acp` | ACP session |
| `grok` | `grok agent stdio` | ACP session |
| `hermes` | `hermes acp` | ACP session |
| `pi` | `pi-acp` | ACP session |
| `mcode` | `mcode acp` | ACP session |

命令取自 ACP 官方注册表（`agentclientprotocol.com/get-started/agents`）与各 CLI 文档。
没装的 CLI 显示为不可用，装了即可选——`available()` 只做 `which(命令[0])`。

cc-switch 网关已移除：身体直接用各自 CLI 的登录与用户配置（`~/.claude`、`~/.codex`、
`~/.gemini`、`~/.grok`、`~/.hermes/config.yaml` 等），不再注入模型、不再建临时家。
模型统一管理由 CCR 承担（见 `AGENTS/gateway.md`）。`Executor.run(req, provider)` 的
`provider` 参数保留为契约接缝，留给未来的 Agent Host（由 CCR 注入）。

## 禁止项

- 不在 `executors/` 里读会话数据库、项目配置或写文件到仓库。
- 不自行发明协议（例如自己解析 CLI 的私有 JSON）；遇到不兼容优先查厂商是否提供开关。
- 不让多个身体共享可变全局状态（SDK 客户端按实例持有）。
- 不在 `run()` 里吞异常后返回空文本；必须写入 `error`。

## 检查

- 这个身体是否复用了厂商官方能力？自研部分是否最小？
- 失败时用户能否从 `error` 看出是哪一层出的问题？
- 换掉这个身体，Agent 的其他配置是否需要改？理想是只改 `body` 一行。
