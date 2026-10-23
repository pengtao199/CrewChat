# cli-layer 架构：核心、运行时与解耦边界

Updated: 2026-10-23
Status: Proposed（先按此结构重构，再继续团队功能）
读者: cli-layer / AgentOS / model-layer / skill-layer / mcp-layer 开发者

---

## 1. 定位

`cli-layer` 是 **CrewChat 的核心进程**，不是“CLI 管理工具”。

它负责：

- Agent 定义、readiness、测试运行
- 单 Agent / 团队会话
- ACP 进程、事件、权限、取消
- 委派 broker、团队快照
- conversations / turns / delegations / agents 的 SQLite 持久化
- 作为 AgentOS 的 **唯一 API 入口**，代理 model / skill / MCP 命令

它不负责：

- 模型供应商、密钥、路由、额度 → `model-layer`
- 技能内容、版本库、安装更新 → `skill-layer`
- MCP server 生命周期、transport、工具发现 → `mcp-layer`
- UI、页面状态、文案 → `AgentOS`
- 外部 CLI adapter 的实现 → 外部依赖

一句话：

> **cli-layer 是“模块化核心”，不是“全家桶”；它做 API 枢纽和会话/团队域，
> 其他资源域通过 port 调用。**

---

## 2. 设计原则

1. **单一写入方**：cli-layer 是唯一写 cli-layer.db 的进程。
2. **单向依赖**：`api → app → domain`；`infra` 和 `runtime` 实现 port，不反向依赖。
3. **业务不碰框架**：domain 不 import axum / reqwest / rusqlite。
4. **API 不写业务**：handler 只做鉴权、解析、调用 app、映射错误。
5. **外部资源不内嵌**：model / skill / MCP 只通过 port 访问。
6. **事件先持久化再广播**：断线重连可回放，UI 不依赖内存。
7. **失败显式**：readiness、权限、技能缺失、模型不可用都返回结构化错误，不静默回退。
8. **可拆分但不提前拆**：团队与 ACP 共享生命周期，留在 cli-layer；只有出现独立部署/扩缩容需求才拆。

---

## 3. 上下文

```text
AgentOS
  │  HTTP + /ws/events（只调 cli-layer）
  ▼
cli-layer（本进程）
  ├─ api/        AgentOS 命令适配 + WS
  ├─ app/        用例编排
  ├─ domain/     Agent / Conversation / Team / Delegation / Turn / Permission
  ├─ runtime/    ACP host / engine / overlay / permission broker
  └─ infra/      SQLite / model client / skill client / mcp client / engine installer
       │
       ├─ model-layer  : providers / routing / keys
       ├─ skill-layer  : global skills / materialize
       ├─ mcp-layer    : MCP registry / runtime（后置）
       └─ local CLIs   : ACP adapters
```

**AgentOS 永远不直连 model-layer / skill-layer / mcp-layer。** cli-layer 作为 BFF 代理命令，
但代理层不拥有这些域的数据。

---

## 4. 分层与依赖规则

```text
api ───────────────► app ───────────────► domain
                         │                    ▲
                         │ ports              │
                         ▼                    │
infra ────────────────────────────────────────┘
runtime ──────────────────────────────────────┘
```

### 4.1 允许的依赖

| 层 | 可以依赖 | 不可以依赖 |
| --- | --- | --- |
| `api` | `app`, `domain` (DTO/错误), `config` | `infra`, `runtime`, `rusqlite`, provider 客户端 |
| `app` | `domain`, `ports` | `axum`, `reqwest`, `rusqlite`, `runtime` 具体实现 |
| `domain` | 标准库、serde | `axum`, `sqlx/rusqlite`, `reqwest`, `tokio` 业务逻辑 |
| `runtime` | `domain`, `ports`, ACP SDK, process | SQLite、HTTP handler |
| `infra` | `domain`, `ports`, SQLite/HTTP clients | `api`, `app` |
| `main` | 全部，只做 wiring | 业务逻辑 |

### 4.2 禁止的反向依赖

- `runtime` 直接读写 SQLite。
- `infra` 调用 `app`。
- `api` 直接调用 `SessionRuntime` 或 `Store`。
- `domain` 依赖 `axum` / `reqwest`。
- `app` 直接拼 SQL 或 spawn ACP 进程。

---

## 5. 目标目录

```text
cli-layer/src/
├─ main.rs                 // 只做 wiring
├─ config.rs
├─ api/
│  ├─ mod.rs
│  ├─ http.rs              // 路由 + handler 薄层
│  ├─ ws.rs                // /ws/events
│  ├─ auth.rs
│  └─ dto.rs               // wire DTO，不混 domain
├─ app/
│  ├─ mod.rs
│  ├─ roster.rs            // Agent CRUD / clone / readiness / test
│  ├─ sessions.rs          // create/open/send/cancel
│  ├─ team.rs              // team snapshot / delegation / member lifecycle
│  ├─ engines.rs           // list/install/diagnostics
│  ├─ skills.rs            // skill proxy 用例
│  ├─ models.rs            // model proxy 用例
│  └─ events.rs            // event replay / fanout
├─ domain/
│  ├─ mod.rs
│  ├─ agent.rs             // Agent / AgentId / readiness 结果
│  ├─ conversation.rs      // Conversation / Turn / Speaker
│  ├─ team.rs              // TeamSnapshot / TeamMember / TeamMode
│  ├─ delegation.rs        // Delegation / status / result
│  ├─ permission.rs        // PermissionPolicy / Decision
│  ├─ engine.rs            // EngineId / Capability
│  ├─ event.rs             // DomainEvent
│  ├─ ports.rs             // 所有 trait
│  └─ error.rs             // DomainError
├─ runtime/
│  ├─ mod.rs
│  ├─ acp/
│  │  ├─ host.rs           // spawn / initialize / session/new / prompt
│  │  ├─ events.rs         // ACP update -> DomainEvent
│  │  └─ permission.rs     // permission broker
│  ├─ engine/
│  │  ├─ registry.rs       // EngineSpec
│  │  └─ manager.rs        // install / version / diagnostics
│  ├─ overlay/
│  │  ├─ mod.rs            // overlay 分派
│  │  ├─ claude.rs
│  │  ├─ codex.rs
│  │  ├─ opencode.rs
│  │  ├─ grok.rs
│  │  ├─ hermes.rs
│  │  ├─ kimi.rs
│  │  ├─ openclaw.rs
│  │  ├─ pi.rs
│  │  ├─ cline.rs
│  │  └─ deepseek.rs
│  └─ session/
│     ├─ manager.rs        // connection lifecycle / lazy start / idle close
│     └─ control_mcp.rs    // 团队控制 MCP（见 §9）
└─ infra/
   ├─ mod.rs
   ├─ store/
   │  ├─ mod.rs
   │  ├─ agents.rs
   │  ├─ conversations.rs
   │  ├─ turns.rs
   │  ├─ delegations.rs
   │  └─ events.rs
   ├─ model_layer.rs
   ├─ skill_layer.rs
   ├─ mcp_layer.rs          // 后置
   └─ engine_install.rs
```

当前文件迁移：

| 当前 | 目标 |
| --- | --- |
| `http.rs` | `api/http.rs` + `app/*` |
| `store.rs` | `infra/store/*` |
| `roles.rs` | `runtime/overlay/*` |
| `engines.rs` | `runtime/engine/registry.rs` |
| `engine_manager.rs` | `runtime/engine/manager.rs` + `infra/engine_install.rs` |
| `model_proxy.rs` | `infra/model_layer.rs` |
| `skill_proxy.rs` | `infra/skill_layer.rs` |
| `acp/host.rs` | `runtime/acp/host.rs` |
| `acp/events.rs` | `runtime/acp/events.rs` |
| `ws.rs` | `api/ws.rs` |

迁移时先移动代码、保持行为不变，再逐步替换调用点；不做一次性重写。

---

## 6. Ports

`domain/ports.rs` 定义接口，`infra` / `runtime` 实现。

### 6.1 仓储

```rust
trait AgentRepository {
    fn list(&self) -> Result<Vec<Agent>>;
    fn get(&self, id: &AgentId) -> Result<Option<Agent>>;
    fn save(&self, agent: &Agent) -> Result<Agent>;
    fn delete(&self, id: &AgentId) -> Result<()>;
}

trait ConversationRepository {
    fn create(&self, conversation: &Conversation, team: Option<&TeamSnapshot>) -> Result<ConversationId>;
    fn get(&self, id: &ConversationId) -> Result<Option<Conversation>>;
    fn list(&self, folder_ids: &[FolderId]) -> Result<Vec<ConversationSummary>>;
    fn append_turn(&self, turn: &Turn) -> Result<()>;
}

trait DelegationRepository {
    fn create(&self, delegation: &Delegation) -> Result<DelegationId>;
    fn update(&self, delegation: &Delegation) -> Result<()>;
    fn list(&self, conversation_id: &ConversationId) -> Result<Vec<Delegation>>;
}
```

### 6.2 会话运行时

```rust
trait SessionRuntime {
    async fn connect(&self, request: ConnectRequest) -> Result<SessionHandle>;
    async fn prompt(&self, handle: &SessionHandle, text: String) -> Result<()>;
    async fn cancel(&self, handle: &SessionHandle) -> Result<()>;
    async fn respond_permission(&self, handle: &SessionHandle, request_id: &str, option: Option<String>) -> Result<bool>;
    async fn close(&self, handle: &SessionHandle);
}
```

`SessionRuntime` 只认识 ACP 和 connection，不读写 Agent/Conversation 表。

### 6.3 外部资源

```rust
trait ModelCatalog {
    async fn models_for_engine(&self, engine: &EngineId) -> Result<Vec<ModelOption>>;
    async fn gateway_key(&self) -> Result<Option<String>>;
    async fn normalize_provider(&self, provider: &str, model: &str) -> Result<String>;
}

trait SkillCatalog {
    async fn list(&self) -> Result<Vec<SkillSummary>>;
    async fn read(&self, id: &SkillId) -> Result<SkillDetail>;
    async fn materialize(&self, home: &Path, engine: &EngineId, skills: &[SkillId]) -> Result<Materialized>;
}

trait McpCatalog {
    async fn list(&self) -> Result<Vec<McpServerSummary>>;
    async fn materialize(&self, home: &Path, engine: &EngineId, servers: &[McpId]) -> Result<MaterializedMcp>;
}

trait EngineCatalog {
    fn list(&self) -> Vec<EngineSpec>;
    fn find(&self, id: &EngineId) -> Option<EngineSpec>;
    fn install(&self, id: &EngineId, version: Option<&str>) -> Result<String>;
}
```

`app` 只依赖 trait；测试里用 fake。

---

## 7. 领域模型

### 7.1 Agent

```text
Agent
├─ identity: id, name, description, avatar, tags
├─ state: enabled, archived, version
├─ runtime: engine, provider, model
├─ persona: soul, rules
├─ capabilities: skills, skill_collections, mcps
├─ permissions: mode, auto_approve, rules
└─ limits: max_turns, timeout, budget
```

业务规则在 `domain/agent.rs`：

- `validate()`：必填、id、版本。
- `can_run(engine, model, skills, mcp)` → readiness。
- `snapshot()`：写团队快照时使用。
- 不包含 key、路径、进程。

### 7.2 Conversation / Team

```text
Conversation
├─ id, folder_id, title, kind
├─ leader_role_id
├─ team_snapshot (可选)
├─ status
└─ timestamps

TeamSnapshot
├─ leader_role_id
├─ members: [{role_id, version, engine, model}]
└─ created_at

Delegation
├─ id, conversation_id
├─ leader_role_id, member_role_id
├─ task, context, expected_output
├─ status, result, usage
└─ timestamps
```

团队快照在创建会话时冻结；运行期角色变化不热改。

---

## 8. 用例边界

### 8.1 `app/roster.rs`

- list/save/delete/clone
- `readiness(role_id)` → 调 EngineCatalog / ModelCatalog / SkillCatalog / McpCatalog
- `test_run(role_id)` → 用 `SessionRuntime` 跑临时会话，不写 conversation

### 8.2 `app/sessions.rs`

- create conversation（单 Agent 或团队）
- open/resume
- send prompt
- cancel
- replay events

### 8.3 `app/team.rs`

- validate team（readiness、成员数、leader 必须在团队）
- create team snapshot
- delegation 状态机
- member lifecycle（lazy connect / idle close）
- 只有这里可以调 `delegate_to_agent`

### 8.4 `app/engines.rs`

- list / status
- install / update / diagnostics

### 8.5 `app/skills.rs` / `app/models.rs`

- 代理 model-layer / skill-layer
- 不把返回数据写进 cli-layer DB（除必填缓存）

---

## 9. 团队委派通道

这是架构里最容易含糊的一点：**Leader 需要一个 `delegate_to_agent` 工具。**

ACP 本身没有“客户端向 agent 注入自定义工具”的标准字段。可选方案：

| 方案 | 可靠性 | 成本 | 结论 |
| --- | --- | --- | --- |
| A. 内部 control MCP server | 高（真正的工具调用） | 中 | **推荐** |
| B. Leader 输出结构化 JSON，cli-layer 解析 | 中（模型可能写坏/包裹） | 低 | 降级方案 |
| C. 用户手动点击“派给成员” | 高 | 交互多 | 不做自动化 |
| D. 各 CLI 原生 subagent | 不可移植 | 低 | 不做 |

推荐 A：

```text
cli-layer
  └─ crewchat-cli team-mcp --token <per-connection-token>
       ├─ delegate_to_agent(roleId, task, context, expectedOutput)
       └─ team_status()
```

- 这是一个**内部 control MCP**，不是用户资源层的 `mcp-layer`。
- Leader 的 `session/new` 里只注入这一个 server。
- 工具回调到 cli-layer 的本地 HTTP/Unix socket，并带 per-connection token。
- 它只暴露团队控制工具，不暴露用户 MCP 库。
- 如果某个 adapter 不消费 `session/new.mcp_servers`，该引擎的团队模式标
  `supported:false`，不退回“解析自然语言”。

用户 MCP 资源仍归 `mcp-layer`；两者不要混。

---

## 10. 事件模型

`DomainEvent`：

```text
SessionUpdate { content | thinking | tool_call | usage }
PermissionRequested { request_id, options }
TurnCompleted { stop_reason | error }
DelegationStarted { delegation_id, member_role_id, task }
DelegationProgress { ... }
DelegationCompleted { status, summary, usage }
MemberStateChanged { role_id, state }
```

`EventEnvelope`：

```json
{
  "seq": 0,
  "type": "content_delta",
  "connection_id": "...",
  "conversation_id": "...",
  "role_id": "...",
  "speaker_role_id": "...",
  "payload": {}
}
```

顺序：

```text
ACP update
  → runtime 归一化
  → app/events
  → infra/store 持久化（分配 seq）
  → EventBus
  → WS / replay
```

先落库再广播，避免“UI 收到了但刷新丢了”。

---

## 11. 权限

`PermissionPolicy` 属于 domain：

- `mode`
- `auto_approve`
- tool rules（后续）

执行点：

- `SessionRuntime` 收到 `session/request_permission`
- `app` 查 Agent 的 policy
- 自动批准 → 记录审计 → 直接 respond
- 否则 → `PermissionRequested` 事件 → UI
- Leader 不能修改成员 policy

审计事件必须持久化。

---

## 12. 事务与一致性

- 一个 SQLite 写入方。
- `create_conversation` + team snapshot + conversation_agents 在一个事务。
- `delegation` 状态变更 + result 在一个事务。
- 事件 seq 由 store 分配。
- 外部副作用（spawn/ACP）不放在 SQLite 事务里；用状态机 + 补偿。
- 所有写操作幂等（id 已存在时更新/返回冲突）。

---

## 13. API / BFF 规则

- AgentOS 只调 `/api/*` 和 `/ws/events`。
- handler 目标 < 40 行；禁止在 handler 里写 SQL、spawn、解析团队规则。
- DTO 与 domain 分离；wire 字段 camelCase/snake_case 由 DTO 显式映射。
- 代理命令（model/skill/mcp）只做转发和错误映射，不落库。
- 新命令必须登记到 `AgentOS/engine-api.md`。

---

## 14. 错误与可观测性

- `DomainError`：`not_found / conflict / invalid / blocked / unavailable`。
- API 统一 `{code,message,details?}`。
- readiness 错误必须带 `fix`。
- 日志字段：`conversation_id / role_id / delegation_id / connection_id`。
- 不记录 key、prompt 全文、工具输出全文到错误日志。

---

## 15. 测试策略

- domain：纯单元测试（readiness、team snapshot、permission）。
- app：fake ports，测试用例编排和事务调用。
- runtime：fake ACP adapter / 进程，测试事件、取消、权限。
- infra：临时 SQLite，测试迁移、事务、回放。
- API：`curl`/集成测试，覆盖状态码和 wire shape。
- 端到端：一个真实 CLI 单 Agent；一个 Leader + 2 成员团队。

---

## 16. 代码预算

不是硬性行数 KPI，但用于防止重新长成 codeg：

- 单文件 > 800 行：必须拆分（当前 `http.rs` 962、`roles.rs` 782 已接近）。
- `api/http.rs` 只允许路由和薄 handler；业务必须在 `app/*`。
- `domain/*` 不出现 `axum`、`reqwest`、`rusqlite`。
- `runtime/*` 不出现 SQL。
- `store/*` 不出现业务规则。
- 新增一个外部资源域时，先问：是否有独立生命周期/独立存储？有 → 独立 layer；
  没有 → cli-layer 内模块。

---

## 17. 迁移顺序

### Step 1：无行为变化地拆目录 ✅ 2026-10-23

1. `store.rs` → `infra/store/mod.rs`
2. `roles.rs` → `runtime/overlay/mod.rs`
3. `engines.rs` + `engine_manager.rs` → `runtime/engine/{registry,manager}.rs`
4. `model_proxy.rs` / `skill_proxy.rs` → `infra/{model_layer,skill_layer}.rs`
5. `http.rs` / `ws.rs` → `api/{http,ws}.rs`
6. `acp/{host,events}.rs` → `runtime/acp/`

每步只移动代码 + 修 import；`cargo test` 18 passed、clippy 通过。
`infra/store` 与 `api/http` 的内部再拆分（agents/conversations/turns、handlers）
放在 Step 2/3，和 domain/ports 抽取一起做。

### Step 2a：拆 store 与 API handler ✅ 2026-10-23

- `infra/store/mod.rs` 拆成 `agents / conversations / folders / events /
  engine_settings / import`；`Store` 仍是唯一连接持有者。
- `api/http.rs` 从 962 行降到 ~192 行；`api/handlers/` 按
  `acp / models / roster / skills / workspace` 拆分。
- `cargo test` 18 passed、clippy、smoke 通过。

### Step 2b.1：Agent 领域类型 + roster 服务 ✅ 2026-10-23

- 新增 `domain/agent.rs`：类型化 Agent，API 不再直接操作 `Value` 字段。
- 新增 `app/roster.rs`：list/get/save/delete/model_face 用例；handler 只做
  DTO 和错误映射。
- `agent_skills_set` / `agent_skill_status` 也改走 roster service。
- `cargo test` 18 passed、clippy、smoke 通过。

### Step 2b.2：workspace / skills 用例层 ✅ 2026-10-23

- 新增 `app/workspace.rs`、`app/skills.rs`；folder/conversation 和 skill/agent
  技能绑定不再写在 handler。
- `AppState` 只暴露 `roster/workspace/skills` 服务与 ACP/model 运行时；
  handler 保持薄层。
- `cargo test` 18 passed、clippy、smoke 通过。

### Step 2c.1：Repository ports + engine service ✅ 2026-10-23

- 新增 `domain/ports.rs`：`AgentRepository` / `ConversationRepository` /
  `EngineSettingsRepository`；`infra/store/repositories.rs` 实现。
- `RosterService` / `WorkspaceService` 改为依赖 `Arc<dyn ...>`，不再直接持有
  `Store`；`EngineService` 统一引擎列表与偏好。
- `cargo test` 18 passed、clippy、smoke 通过。

### Step 2c.2：SessionRuntime port

- 把 `AgentRole` / Conversation / Team / Delegation 从 JSON Value 提升为类型。
- 给 store 定义 repository trait。
- `app` 通过 trait 调用。

### Step 3：加 readiness / test run

- `app/roster.rs`
- `SessionRuntime` 临时连接
- 只读依赖 model/skill/mcp port

### Step 4：团队

- schema + team snapshot
- `app/team.rs`
- internal control MCP
- member lifecycle

---

## 18. 验收

- [ ] `api` 不直接 import `infra` / `runtime`。
- [ ] `runtime` 不 import `infra/store`。
- [ ] `domain` 不 import axum/reqwest/rusqlite。
- [ ] `store` 不包含业务判断。
- [ ] 所有外部资源通过 port。
- [ ] 一个 SQLite 写入方。
- [ ] 文件超 800 行会触发拆分。
- [ ] 单 Agent、团队、readiness、test run 均有对应测试。
- [ ] `cargo test` / `clippy` 绿。

---

## 19. 相关文档

- `docs/plans/agents-page-and-team.md`：Agent 页面、readiness、团队产品设计。
- `docs/plans/cli-layer-migration.md`：迁移阶段与已验证能力。
- `docs/plans/skill-layer-api.md`：技能代理契约。
- `docs/plans/mcp-layer.md`：MCP 资源层边界。
- `AGENTS/agent-model.md`、`AGENTS/truth-sources.md`：领域与真相源规则。
