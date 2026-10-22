# 分层与架构

## 适用范围

新增目录或模块、调整分层、判断代码该放哪一侧、改动跨层调用时。

## 定位（2026-10-19）

产品由三条腿 + 一个归档组成：

| 腿 | 路径 | 职责 | 真相源 |
| --- | --- | --- | --- |
| 核心（运行时） | `codeg/` | 界面 + 内置 Rust 后端：ACP 会话、委派、权限、持久化、用量 | `codeg.db` |
| 模型层 | `model-layer/` | 供应商 / 路由 / 密钥 / 额度（本地网关 + 控制面） | 模型层 SQLite（`AgentOS/model/model.db`） |
| 壳（macOS） | `AgentOS/` | Swift 客户端：只渲染与交互 | 无，只持自己的设置 |
| 归档 | `archive/` | 冻结的 Python 编排层与历史，只读 | 无 |

## 边界

- **业务与执行归 `codeg/`**：谁在跑、能不能跑、花了多少，都是它的结论。
- **模型与密钥归 `model-layer/`**：其他层不持有 provider key。`ccr/` 仅为回退保留，不再是运行时依赖。
- **壳只渲染**：`AgentOS/` 不复制真相、不解析协议（见 `AGENTS/agentos-boundary.md`）。
- **归档不参与任何构建**：`codeg/`、`ccr/`、`AgentOS/` 都不得引用 `archive/`。

## 改动去向

| 需求 | 去哪 |
| --- | --- |
| 会话 / 委派 / 权限 / 持久化 / 用量行为 | `codeg/src-tauri/src/`（改法见 `AGENTS/upstream.md`） |
| 现有 Next.js 工作台界面 | `codeg/src/`（改法见 `AGENTS/ui.md`） |
| 模型接入 / 端点 / 模型注入 | `model-layer/`（见 `AGENTS/gateway.md`） |
| macOS 原生壳 | `AgentOS/`（见 `AgentOS/AgentO
| Agent 领域语义 | 先读 `AGENTS/agent-model.md`；落点是 `codeg.db` |

## Python 编排层（已归档）

`archive/python-orchestrator/` 保留 v1 的 L0–L4 契约、检查脚本与全部说明，
见 `archive/python-orchestrator/CONTRACT.md`。它**不参与运行时**，运行时分层由上面三条腿各自的规则约束。

## 文件系统陷阱

- **`AGENTS/` 与 `agents/` 在 macOS 上同一个目录**（默认大小写不敏感）。规则目录用 `AGENTS/`。
- 新增目录名前确认它与既有目录不只大小写不同；`UI/` 与 `ui/`、`Docs/` 与 `docs/` 同理。
- `.data/` 与本机运行态不进版本库；第三方参考放 `Reference/`（已忽略）。

## 禁止项

- 不在 `codeg/` 之外再养一套编排。
- 不让界面承担「是否可用 / 是否已安装」的判断（那是核心的职责）。
- 不在仓库根新建与 `AGENTS/`、`docs/`、`codeg/` 仅大小写不同的目录。
- **仅本机**：核心与壳都只监听 `127.0.0.1`，禁止暴露到 LAN / 公网——那等于把本机的 CLI 与 Agent 交给任何人。

## 检查

- 这次改动落在哪条腿？该腿的规则读了吗？
- 有没有把某个结论从核心挪到壳或界面？
- 归档里的东西有没有被新代码引用（不该有）？
