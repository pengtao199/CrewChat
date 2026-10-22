# Swift 原生运行时迁移

Updated: 2026-10-22 (Asia/Shanghai)
Status: Proposed（Phase 0 未开工）
关系：`agentos-shell.md` 的下一阶段。壳计划约束的是「Swift UI + codeg 核心」；本计划的目标是让 Swift 自己成为核心。

## 1. 目标与边界

### 目标

AgentOS 不再启动 `codeg-server`，也不再启动 CCR；会话引擎、持久化、模型网关、权限、技能/MCP、团队编排全部由 Swift 进程承担。

### 边界（必须先说清楚）

**不依赖 codeg/CCR ≠ 不依赖任何外部进程。**

Claude Code、Codex、Gemini CLI、OpenCode 以及它们的 ACP adapter 本身是 Node/Rust 二进制。Swift 只能做宿主：定位、启动、注入环境、通过 stdio 说 ACP。把「外部 CLI」也去掉，不在本计划范围内。

### 与现有规则的关系

当前 `AGENTS/agentos-boundary.md` 规定「壳不实现 ACP / 不 spawn CLI / 不碰模型」。这是**当前阶段**的规则，不是终态规则。迁移到 Phase 3 切换默认运行时之前，必须同步改写：

| 文件 | 迁移后的含义 |
| --- | --- |
| `AGENTS/agentos-boundary.md` | Swift 拥有 ACP、进程、持久化、模型网关；CLI 是外部身体 |
| `AGENTS/agentos-layout.md` | 新增 `Runtime/`、`Store/`、`ModelGateway/`、`Capabilities/`、`Orchestration/` |
| `AGENTS/truth-sources.md` | 业务库改为 Swift 自己的 SQLite；密钥进 Keychain；CCR 从清单移除 |
| `AGENTS/gateway.md` | 重写为 Swift 网关的 provider/路由/额度规则 |
| `AgentOS/engine-api.md` | 从「对核心的 HTTP 调用」改为「进程内 Service 接口」；只保留外部 CLI/网关边界 |

### Definition of Done

- 干净机器上只有 `AgentOS.app` 和用户已安装的 CLI，没有 `codeg-server`、没有 CCR。
- 选工作区 → 选 Agent/团队 → 发任务 → 流式文本/工具/权限 → 取消 → 重启后恢复，全部走 Swift。
- 模型页的供应商、凭据池、额度、每角色模型全部由 Swift 读写；CLI 只认 Swift 网关。
- 旧 `codeg.db` 的会话、花名册完成一次性导入，旧会话可读。
- 签名 + 公证的非沙盒 App 能首次启动；无孤儿进程。

## 2. 现状与体量

| 层 | 现有实现 | 实测规模 | 迁移策略 |
| --- | --- | --- | --- |
| ACP / 会话 | codeg `acp` + `commands/acp` | ~148k 行 | 重写所需子集，约 8–12k |
| CLI transcript 解析 | codeg `parsers` | ~66k 行 | **不迁**：自己记 ACP 事件日志，只写旧数据导入器 |
| 持久化 | codeg `db` | ~27k 行 | GRDB 新 schema，约 3–5k |
| HTTP/WS 服务 | codeg `web` | ~21k 行 | **删除**：UI 与引擎同进程 |
| 模型网关 | CCR TS | ~154k 行 | Swift 网关，约 8–12k（只做用到的协议） |
| 编排/权限/团队 | codeg 部分 | — | Swift，约 4–6k |
| UI | AgentOS | ~9k 行 | 保留，继续长 |

结论：这不是「把壳补完」，是重写一个 agent runtime。有效迁移量约 **20–30k Swift + 测试**，不是 30 万行逐行翻译。

## 3. 目标架构

```
AgentOS.app (SwiftUI)
├─ Runtime/
│  ├─ ACP/            JSON-RPC over stdio、session/update、permission
│  ├─ Process/        spawn、env overlay、SIGTERM、孤儿清理
│  ├─ Session/        coordinator、turn reducer、cancel、resume
│  └─ Events/         EventBus（进程内）
├─ Store/             GRDB：workspaces / conversations / events / turns / roster
├─ ModelGateway/      Hummingbird/NIO：/v1/messages、/v1/responses、/v1/models…
├─ Capabilities/      skills / MCP / engines（文件 I/O）
└─ Orchestration/     team、delegation、permission policy
```

数据流：

```
UI → SessionCoordinator.prompt
   → ACPClient（stdio）→ CLI / adapter
   ← session/update → TurnReducer → EventLog（GRDB）→ EventBus → UI
```

- UI 直接调用进程内 Service，没有 codeg 的 HTTP/WS。
- 唯一本地 HTTP listener 是 `ModelGateway`——CLI 需要一个 base URL。
- ACP 走 stdio，不需要服务器。
- 现有 `Core/` 的 port 名称保留，替换实现即可，`Features/` 尽量不动。

## 4. 关键技术决策

| 问题 | 决策 | 备注 |
| --- | --- | --- |
| ACP | 先确认是否有可用的 Swift SDK；没有就按 JSON-RPC 实现所需子集 | 官方 crate 是 Rust `agent-client-protocol 2.2`；Swift 侧**待验证** |
| 进程 | Foundation `Process` + `Pipe` + `AsyncStream` | ACP 是 stdio，不需要 PTY；必须有进程组/孤儿清理 |
| 持久化 | GRDB + WAL | 事件溯源；`events` 是 live 真相，`turns` 是投影 |
| HTTP 网关 | Hummingbird（SwiftNIO） | 只监听 `127.0.0.1`；可选 gateway key |
| 密钥 | Keychain | DB 只存 `keychain_ref`，不存明文 |
| 日志 | OSLog + 自有事件日志 | 不把 prompt/key 写进日志 |
| 测试 | fake ACP agent + fake provider | 先于业务代码落地 |
| 打包 | Developer ID + 公证，非沙盒 | 与现状一致；App Store 不在范围 |

### ACP 子集（第一版）

`initialize`、`session/new`、`session/prompt`、`session/cancel`、`session/update`、`session/request_permission`，以及 adapter 实际用到的 `fs/*` / `terminal/*`。先做 Claude Code 用到的 surface，再按 CLI 补。

### 模型网关范围

第一版只做 **pass-through + 少量转换**：

| CLI | 首选端点 | 说明 |
| --- | --- | --- |
| Claude Code | `/v1/messages` | Anthropic 原生；上游不是 Anthropic 时才转换 |
| Codex | `/v1/responses` | OpenAI Responses |
| OpenCode | `/v1/chat/completions` | OpenAI Chat；需要时转 Responses |
| Gemini | `/v1beta/models/{model}:streamGenerateContent` | Gemini 原生 |

不做 CCR 的全量 provider 矩阵、proxy、插件、bot gateway、virtual models。转换层只覆盖实际用到的工具调用/推理/流式边界。

### 旧解析层为什么可以不迁

codeg 的 66k 行 parsers 是为了从 CLI 自己的 JSONL 里重建历史。Swift 运行时自己产生并保存 ACP 事件，**历史由自己拥有**；只需要一个一次性的 `codeg.db` 导入器。

## 5. 迁移顺序

原则：每一步都有回退开关；生产默认路径只在 Phase 3 切一次，避免双写两套库。

### Phase 0：冻结契约与测试地基（约 2 周）

- 把现有 `Core*` port 固化为 Swift Service 协议（`SessionService`、`ModelService`、`StoreService`、`CapabilityService`）。
- 写 fake ACP agent（脚本化 text/tool/permission/cancel）与 fake provider（Anthropic/OpenAI SSE）。
- 录制 codeg/CCR 的真实请求/事件作为 conformance fixtures。
- 产出 `AGENTS/agentos-boundary.md` v2 草案（不生效，Phase 3 启用）。

**Gate**：一个假 ACP 会话能在测试里跑完 streaming + tool + permission + cancel；fake provider 能返回可解析的 SSE。

### Phase 1：Swift 模型层，移除 CCR（约 4–6 周）

1. `ModelStore`：providers、credentials（Keychain）、per-role model、额度快照。
2. `ModelGateway`：上表四条链路，先 pass-through，再补需要的转换。
3. **CCR 兼容控制面**：实现 `getConfig` / `getProviderAccountSnapshots` 的最小子集，把 codeg 的 `CCR_WEB_URL` 指到 Swift 服务；codeg 的 `agent_host` / overlay 不改。
4. AgentOS 模型页从 `ccr_call` 切到进程内 `ModelService`（UI 不动）。

**Gate**：CCR 进程不再启动；模型页可新建/编辑 provider + 多密钥；Claude Code/Codex 各跑一轮；额度页有数据；连续 2 周无回归。

**回退**：`CCR_WEB_URL` 指回 CCR，保留二进制。

### Phase 2：Swift ACP 原型，单 CLI（约 4–6 周，dev flag）

1. `ACPClient` + `AgentProcess` + `SessionCoordinator`。
2. 只接 Claude Code；在开发开关下与 codeg 并排跑同一个 prompt。
3. 对比：事件顺序、工具调用、权限、取消、进程清理、首 token 延迟。

**Gate**：Claude Code 完整一轮；permission 批准/拒绝；cancel 后进程退出；无孤儿；首 token 延迟相对 codeg +10% 以内。

**回退**：关掉 dev flag，生产仍走 codeg。

### Phase 3：全 CLI + Swift 存储 + 切换（约 6–8 周）

1. 补齐支持列表里的 CLI（建议先 Claude Code / Codex / OpenCode / Gemini）。
2. 新 schema + `LegacyCodegImporter`；导入 `codeg.db` 的 workspaces / conversations / turns / roster。
3. 会话恢复：优先 adapter `session/load`；不支持时定义降级策略（只读历史 / 重放最近 N 轮）。
4. 生产默认路径切到 Swift runtime + Swift store；codeg 只作只读导入器/回退二进制。

**Gate**：支持列表全绿；旧会话 100% 可读；重启后可继续；不出现双写。

这是整个迁移最危险的一步：**不做双写**。开发期用 dev flag 并排，生产切换是一次性的。

### Phase 4：团队、委派、权限、能力（约 4–6 周）

- 一个角色一个连接；团队协调器负责分发、汇总、取消。
- 权限策略：按角色 `permission_mode` / `auto_approve`。
- 技能/MCP/引擎管理改为进程内 Service；沿用现有 UI。
- 用量与错误展示补齐。

**Gate**：3 人团队跑通；委派可见；批准/拒绝两条路径；技能/MCP 保存后新会话生效。

### Phase 5：移除 codeg/CCR（约 3–4 周）

- 删除 sidecar、`ccr/`、启动/探活/健康检查代码。
- 保留一次性导入器与 `runtime=codeg` 回退一个版本。
- 打包、签名、公证、干净机器验证。
- 更新所有规则文档与 `engine-api.md`。

**Gate**：干净机器只装 App + CLI 即可跑通；无 codeg/ccr 进程；签名/公证通过；回退开关可用。

## 6. 数据迁移

### 从 codeg.db 导入

| 目标表 | 来源 | 说明 |
| --- | --- | --- |
| `workspaces` | `folder` | 保留 path/name/alias/sort_order/is_open |
| `conversations` | `conversation` | 保留 workspace、title、kind、status、team、时间戳 |
| `turns` | 会话 turn/message 表 | 先做字段映射表，再写 importer |
| `agent_roles` | `custom_agent`（`kind='role'`） | 保留 body、brain、skills、mcp、permission |
| `events` | 无 | 新库从切换点开始记录；旧历史只做只读投影 |

- 旧库保持只读，不迁移后删除；导入失败可重跑。
- 导入前备份 `codeg.db`；导入后做只读校验（会话数/turn 数/角色数）。

### 从 CCR 导入

| 数据 | 去向 |
| --- | --- |
| providers（name/base_url/type/models） | `providers` |
| credentials / api_key | Keychain + `credentials`（只存引用） |
| profiles | 花名册的 per-role 模型 / 网关路由；不保留 CCR profile 概念 |
| 额度快照 | `provider_quotas`（能拉到的才存） |

## 7. 砍掉什么

不迁：

- codeg 的 web UI / server 模式 / 多用户 / 远程工作区；
- 每个 CLI 的 transcript parser（只留 importer）；
- 插件市场、science/office tools、config sync、experts、bot gateway；
- CCR 的 proxy、插件、virtual models、全量 provider 矩阵、所有协议互转。

保留：

- ACP 会话、流式、工具、权限、取消、恢复；
- 工作区、会话、花名册、团队、委派；
- 模型供应商/凭据池/额度、每角色模型；
- 技能、MCP、引擎；
- 现有 UI。

## 8. 测试与验收

| 类型 | 内容 |
| --- | --- |
| 单元 | ACP 编解码、turn reducer、进程生命周期、model store、导入器 |
| 集成 | fake ACP agent 跑完整会话；fake provider 跑网关四条链路 |
| 契约 | 录制 codeg/CCR 流量，回放对比事件顺序与最终 turns |
| 手工矩阵 | CLI × {新会话, 恢复, 取消, 权限, 工具, 切模型, 技能, MCP} |
| 性能 | 首 token 延迟、空闲 CPU、内存、进程数、退出后无孤儿 |
| 打包 | 干净机器首次启动、签名、公证、升级 |

## 9. 风险

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| ACP adapter 不支持 `session/load` | 历史恢复方案复杂 | Phase 2 先验证；不支持就降级只读/重放 |
| 协议转换边界多 | 模型网关延期 | 只做实际用到的四条链路；pass-through 优先 |
| 切换时双写两套库 | 数据分裂 | 生产一次性切换；开发用 dev flag，不双写 |
| 进程/权限清理不彻底 | 孤儿进程、卡死 | 进程组 + 启动时 reaper + 测试 |
| macOS 签名/公证 | 无法分发 | 非沙盒 + Developer ID，Phase 5 真机验证 |
| 功能 scope 蔓延 | 无限期 | 按第 7 节砍；新需求进 backlog，不阻塞迁移 |
| codeg/CCR 行为暗坑 | 回归 | conformance fixtures；并排跑一个版本 |

## 10. 估算

| Phase | 人周（1 人） |
| --- | --- |
| 0 契约/测试地基 | 2 |
| 1 模型层 | 4–6 |
| 2 ACP 单 CLI | 4–6 |
| 3 全 CLI + 存储 + 切换 | 6–8 |
| 4 团队/权限/能力 | 4–6 |
| 5 清理/打包 | 3–4 |
| 合计 | 23–32 人周 |

- 1 人：约 6–8 个月（含缓冲）。
- 2 人并行（模型层 / ACP）：约 4 个月。
- 砍到只支持 2 个 CLI、不做额度/团队：约 3 个月。

## 11. 开工第一步（3 个 spike）

每个 2–3 天，产出报告 + go/no-go。

### Spike A：Swift ACP + Claude Code

- 起 `Process`，stdio 说 ACP，完成 `initialize` / `session/new` / `prompt`。
- 通过：看到流式文本、一次工具调用、一次权限请求、cancel 后进程退出。
- 失败：记录 adapter 版本、协议差异，再决定是否保留薄 Rust helper。

### Spike B：Swift 网关 `/v1/messages`

- Hummingbird 起本地端口，把一个 provider 的响应以 Anthropic SSE 流给 Claude Code。
- 通过：文本流、工具调用、stop、错误透传。
- 失败：先做 pass-through 到原生 Anthropic provider，转换延后。

### Spike C：GRDB 导入 codeg.db

- 读 codeg 库，导入 workspaces/conversations/turns/roles，UI 只读渲染。
- 通过：会话数/turn 数与旧库一致；抽样 10 条会话内容一致。

三个 spike 全过 → 按 Phase 0 正式开工；任一失败 → 调整 scope 或保留对应薄 sidecar。

## 12. 待确认

1. Swift 是否有可用的 ACP SDK？（没有就自己实现子集）
2. 支持列表里每个 adapter 是否支持 `session/load`？
3. 切换时支持哪些 CLI？（建议 4 个：Claude Code / Codex / OpenCode / Gemini）
4. 模型网关是否必须做协议转换，还是用户的 provider 都支持原生协议？
5. 额度数据（订阅/余额）Swift 侧从哪里拉？先支持能拉到的，其余留空。
6. 密钥用 Keychain 还是加密文件？（Keychain 优先；ad-hoc 签名弹窗问题在发布签名下不存在）
7. 旧会话恢复：`session/load` 不支持时，只读历史还是重放最近 N 轮？
8. 团队/委派：沿用 codeg 的语义，还是借迁移重新设计？

## 13. 维护

- 每个 Phase 完成后更新本文件的 Status 与 Current Facts；完成/放弃/被取代时移入 `archive/`。
- 迁移期间 `agentos-shell.md` 仍是当前执行计划；本文件只在 Phase 0 开工后转为 In Progress。
- 新增对核心的调用仍按现有规则登记；Phase 3 后改为登记进程内 Service 接口。
