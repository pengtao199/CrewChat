# 分层与架构

## 适用范围

新增目录或模块、调整分层、判断代码该放哪一侧、改动跨层调用时。

## 定位（2026-10-23）

当前运行时由五层组成：macOS 壳 **AgentOS**、核心 **cli-layer**、模型层 **model-layer**、
技能层 **skill-layer**、MCP 层 **mcp-layer**。MCP 的最终形态是完整本地网关：控制面 + 每 Agent 一个 Rust proxy 实例，见
[`docs/plans/mcp-layer.md`](../docs/plans/mcp-layer.md)。

`archive/` 是历史归档，不参与构建。

| 层 | 路径 | 职责 | 真相源 |
| --- | --- | --- | --- |
| 核心（运行时） | `cli-layer/` | ACP 会话、权限、花名册、引擎管理、持久化 | `~/Library/Application Support/AgentOS/cli/cli-layer.db` |
| 模型层 | `model-layer/` | 供应商 / 路由 / 密钥 / 额度（本地网关 + 控制面） | SQLite 元数据 + macOS Keychain |
| 技能层 | `skill-layer/` | 全局技能库、版本、安装/更新、会话 materialize | `~/.agents/skill-store` + `~/.agents/skill-layer/registry.sqlite` |
| MCP 层 | `mcp-layer/` | MCP 注册表、Agent proxy 实例、会话配置 | `~/.agents/mcp` + 每 Agent proxy 运行时 |
| 壳（macOS） | `AgentOS/` | Swift 客户端：只渲染与交互 | 无，只持自己的设置 |
| 归档 | `archive/` | 冻结的 Python 编排层与历史，只读 | 无 |

## 边界

- **业务与执行归 `cli-layer/`**：谁在跑、能不能跑、花了多少，都是它的结论。
- **模型与密钥归 `model-layer/`**：其他层不持有 provider key。
- **技能归 `skill-layer/`**：全局库与版本由它管理；`cli-layer` 只做代理与 Agent 绑定。
- **MCP 归 `mcp-layer/`**：注册表、每 Agent proxy 实例、会话配置；不把 MCP 逻辑塞进 `skill-layer`。
- **壳只渲染**：`AgentOS/` 不复制真相、不解析协议（见 `AGENTS/agentos-boundary.md`）。
- **归档不参与任何构建**：任何运行层都不得引用 `archive/`。

## 改动去向

| 需求 | 去哪 |
| --- | --- |
| 会话 / 委派 / 权限 / 持久化 / 用量行为 | `cli-layer/src/` |
| macOS 原生壳 / 设置页 / Agents 页 | `AgentOS/`（见 `AGENTS/agentos-boundary.md`、`AGENTS/ui.md`） |
| 模型接入 / 端点 / 模型注入 | `model-layer/`（见 `AGENTS/gateway.md`） |
| 技能库 / 版本 / 安装 / materialize | `skill-layer/`（见 `docs/plans/skill-layer.md`） |
| MCP 网关 / Agent MCP 授权 | `mcp-layer/`（见 `docs/plans/mcp-layer.md`） |
| Agent 领域语义 | 先读 `AGENTS/agent-model.md`；落点是 `cli-layer.db` |

## Python 编排层（已归档）

`archive/python-orchestrator/` 保留 v1 的 L0–L4 契约、检查脚本与全部说明，
见 `archive/python-orchestrator/CONTRACT.md`。它**不参与运行时**，运行时分层由上面各层规则约束。

## 文件系统陷阱

- **`AGENTS/` 与 `agents/` 在 macOS 上同一个目录**（默认大小写不敏感）。规则目录用 `AGENTS/`。
- 新增目录名前确认它与既有目录不只大小写不同；`UI/` 与 `ui/`、`Docs/` 与 `docs/` 同理。
- `.data/` 与本机运行态不进版本库；第三方参考放 `Reference/`（已忽略）。

## 禁止项

- 不在 `cli-layer/` 之外再养一套编排。
- 不让界面承担「是否可用 / 是否已安装」的判断（那是核心的职责）。
- 不在仓库根新建与 `AGENTS/`、`docs/`、`cli-layer/` 仅大小写不同的目录。
- **仅本机**：核心与壳都只监听 `127.0.0.1`，禁止暴露到 LAN / 公网——那等于把本机的 CLI 与 Agent 交给任何人。

## 检查

- 这次改动落在哪一层？该层的规则读了吗？
- 有没有把某个结论从核心挪到壳或界面？
- 归档里的东西有没有被新代码引用（不该有）？
