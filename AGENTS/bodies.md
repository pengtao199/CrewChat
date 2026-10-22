# 身体（执行器）

> **定位（2026-10-19）**：本文件描述 **Python 编排层（现归档在 `archive/python-orchestrator/src/crewchat/`）的执行器抽象**，该层已
> **冻结为参考**，不再是产品运行时。**产品里「身体」= Codeg 的一个 ACP agent 类型**
> （`claude_code` / `codex` / `open_code` / `gemini` / `hermes` / …），花名册的角色用
> `body` 引用它。要**新增 / 调整一个身体**，改的是 Codeg 侧两处：
> ① ACP agent 注册与设置页（`codeg/src-tauri/src/acp/`、`settings/acp-agent-settings*`）；
> ② 按角色绑定模型（`codeg/src-tauri/src/agent_host/overlay.rs` + `faces.rs`，见
> `AGENTS/agent-model.md`）——**不是在 Python 里加 `Executor` 子类**。
> 下文保留 Python 时代的契约与取舍，仅作历史与设计参考。

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
- 过程通过 `req.emit(kind, ...)` 上报：`delta`（增量文本）、`thinking`、`tool`、`notice`、`error`。

## 原则

- **优先官方 SDK**：能用厂商官方库就不用 subprocess 解析标准输出。已采用的官方件：`claude-agent-sdk`、`openai-codex-sdk`、`openai`。
- **官方件优先于自研适配**：只有当官方件不可用（alpha、版本不匹配、违背协议）时才回退到官方 CLI，并把原因写在文件头。
- **流式是默认**：所有身体都应上报 `delta`，让界面能逐字渲染；一次性返回是降级方案。
- **记忆用厂商的会话机制**：`session` 参数传给 CLI/SDK 自己的续接能力，不自建对话历史重放。
- **身体不含权限策略**：Python 侧不做审批（`request_permission` 兜底放行，见下）；产品面的权限模型在 Codeg。

## 权限：Python 侧已退役，产品面在 Codeg

2026-10-11 变更：自建的权限管道整体移除（`core/permissions.py`、`crewchat_permissions`
API、`permission` 帧与确认端点、`tests/test_permissions.py`）。原因：

- Python 侧从来没有 CLI 真的发来过 `session/request_permission`（实测见 git 历史），
  管道对着空气，唯一消费者是已删除的旧前端；
- 产品面的权限模型在 **Codeg**：每 CLI 启动策略（claude/codex 权限模式、grok
  `--permission-mode`、cursor `--force`、opencode 的 allow/ask/deny 规则）+ 富权限卡
  （命令 / diff / 授权范围 / 默认拒绝倾向）；Python 侧无法"接进"它的卡片。

现在的行为：`_AcpClient.request_permission` 兜底放行——挑一个「允许」类选项
（`allow_once` / `allow_always`），没有允许项则拒绝（`cancelled`）。不弹卡、不发帧、不落盘。

将来 Python 复活为自定义 ACP agent 时，它只**提问**（`session/request_permission`），
答题在 Codeg——不要在这一侧重建审批 UI。
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
`provider` 参数保留为契约接缝，留给 Agent Host（由模型层与 per-role overlay 注入）。

## 禁止项

- 不在 `executors/` 里读会话数据库、项目配置或写文件到仓库。
- 不自行发明协议（例如自己解析 CLI 的私有 JSON）；遇到不兼容优先查厂商是否提供开关。
- 不让多个身体共享可变全局状态（SDK 客户端按实例持有）。
- 不在 `run()` 里吞异常后返回空文本；必须写入 `error`。

## 检查

- 这个身体是否复用了厂商官方能力？自研部分是否最小？
- 失败时用户能否从 `error` 看出是哪一层出的问题？
- 换掉这个身体，Agent 的其他配置是否需要改？理想是只改 `body` 一行。
