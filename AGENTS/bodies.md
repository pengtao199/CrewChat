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
    needs_gateway: ClassVar[bool]      # 大脑经反代；Claude/Codex 写进程临时配置，见 gateway.md

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

| id | acp 启动命令 | 经网关 | 记忆 |
| --- | --- | --- | --- |
| `opencode` | `opencode acp` | **是**（隔离的 `opencode.json`：`providers.gw.settings`，目录键 `main`，`modelID` 为发布 id） | ACP session |
| `openclaw` | `openclaw acp` | **是**（`OPENCLAW_STATE_DIR`，另起一次本机 Gateway） | ACP session |
| `claude` | `claude-agent-acp` | **是**（临时 `CLAUDE_CONFIG_DIR`） | ACP session |
| `codex` | `codex-acp` | **是**（临时 `CODEX_HOME`，`ccs-<key>/<model>`） | ACP session |
| `gemini` | `gemini --acp` | **是**（`GEMINI_MODEL`，`ccs-gemini-<key>--<model>`） | ACP session |
| `grok` | `grok agent stdio` | **是**（`GROK_HOME`） | ACP session |
| `hermes` | `hermes acp` | **是**（`~/.hermes/crewchat-sessions` 下的 `HERMES_HOME`，不改用户的 `config.yaml`） | ACP session |
| `pi` | `pi-acp` | **是**（`PI_CODING_AGENT_DIR` 的 `models.json`，`api: openai-completions`，`apiKey` 为 `PROXY_MANAGED`；`settings.json` 设 `quietStartup`，开场白不进对话） | ACP session |
| `mcode` | `mcode acp` | **是**（`MINIMAX_DATA_DIR`） | ACP session |

命令取自 ACP 官方注册表（`agentclientprotocol.com/get-started/agents`）与各 CLI 文档。
没装的 CLI 显示为不可用，装了即可选——`available()` 只做 `which(命令[0])`。

**经网关**（`needs_gateway=True`）的身体由 `Session` 先 `Gateway.activate` 拿到这次的发布
id，写进这一次进程自己的配置（`PROXY_MANAGED`，不含真实 key）。同一个 CLI 的两个会话
可以同时用两家供应商。不改用户的 `~/.claude`、`~/.codex`、`~/.gemini`、`~/.grok`、
`~/.hermes/config.yaml`。Hermes 只在 `~/.hermes/crewchat-sessions/` 下建临时家。
子进程退出（含失败）时删除临时目录。

模型选择：会话建好后，从 ACP 的 `config_options` 里找出提供了该 agent 大脑的那个项并
设过去——对 `opencode` 这类把大脑列进选项的 CLI 生效；`claude/codex` 的选项只有自家
模型别名，所以改由上面的 env 注入。

## 禁止项

- 不在 `executors/` 里读会话数据库、项目配置或写文件到仓库。
- 不自行发明协议（例如自己解析 CLI 的私有 JSON）；遇到不兼容优先查厂商是否提供开关。
- 不让多个身体共享可变全局状态（SDK 客户端按实例持有）。
- 不在 `run()` 里吞异常后返回空文本；必须写入 `error`。

## 检查

- 这个身体是否复用了厂商官方能力？自研部分是否最小？
- 失败时用户能否从 `error` 看出是哪一层出的问题？
- 换掉这个身体，Agent 的其他配置是否需要改？理想是只改 `body` 一行。
