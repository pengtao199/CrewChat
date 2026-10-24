# Agents 页面与团队会话：核心设计

Updated: 2026-10-24
Status: In Progress（核心团队链路已落地，正在修 AgentOS 契约）
读者: cli-layer / AgentOS / skill-layer 开发者

---

## 1. 定位

这份文档定义 AgentOS 的 **Agent 组装页**，以及核心 **cli-layer** 如何把配置好的 Agent
用于单 Agent 会话和多 Agent 团队会话。

它取代 `docs/plans/agents-core-collaboration.md` 中面向旧核心的运行时方案；
旧核心只作为历史实现，不再作为产品核心。

一句话：

> **Agents 页负责“定义可运行的 Agent”，cli-layer 负责“验证它能跑”，
> 团队会话先做“Leader 委派成员”，不要先做“Agent 自由群聊”。**

---

## 2. 边界

### 2.1 谁负责什么

```text
AgentOS (SwiftUI)
  │  只渲染、只发命令
  ▼
cli-layer (Rust, 当前核心 / BFF)
  ├─ API 适配（AgentOS 唯一入口）
  ├─ Agent 定义 / readiness / 测试运行
  ├─ 单 Agent 与团队会话
  ├─ ACP 进程、事件、权限
  ├─ 委派 broker
  └─ SQLite 持久化
  │
  ├─ model-layer  : 供应商 / 路由 / 密钥
  └─ skill-layer  : 全局技能库 / materialize
```

- AgentOS 不拼 prompt、不 spawn CLI、不决定团队成员。
- model-layer 不知道 Agent，只按请求里的 `model` 路由。
- skill-layer 不知道 Agent，只按 `skill_ids` 为 session home 建链。
- Agent 的“引用关系”只存在 cli-layer。

### 2.2 为什么 Agent/团队不单独做 sidecar

这是本文最重要的架构取舍：**cli-layer 会变“重”，但不应该变成旧核心那样的单体；
也不应该为了“每层一个进程”再拆一个 agent-layer。**

| 方案 | 优点 | 缺点 | 结论 |
| --- | --- | --- | --- |
| A. Agent / 团队留在 cli-layer，做模块化单体 | 团队与 ACP session/事件/权限天然同进程；一个 SQLite 写入方；打包/生命周期简单 | cli-layer 责任变多，需要严格模块边界 | **当前采用** |
| B. 新建 `crew-layer` / `agent-layer` | 领域边界更纯；团队逻辑可独立演进 | 需要自己的 DB 或双写；session/event/permission 要跨进程代理；多一个 sidecar 的端口、健康、打包、故障面 | 暂不采用 |
| C. 把团队逻辑放进 AgentOS | 少一个进程 | 壳承载业务状态，违背“壳只渲染” | 不采用 |

判断标准不是“代码多不多”，而是**是否共享同一个生命周期和同一份状态**：

- Agent 定义和团队快照必须和 conversations / turns / delegations 在同一个事务边界。
- 委派必须直接操作 ACP connection、permission、cancel、事件流。
- 这些都不是独立资源，拆出去只会制造 IPC 和状态复制。

所以：**Agent / 团队是核心域，留在 cli-layer；model / skill / MCP 是独立资源域，
各自一个 sidecar。** MCP 已决定用 `mcp-layer`，与本设计不冲突。

### 2.3 cli-layer 的硬边界

为了避免重新长成旧单体，给 cli-layer 划死边界：

允许：

- Agent 定义、readiness、test run
- 单 Agent / 团队会话、委派、团队快照
- ACP 进程、事件、权限、取消
- conversations / turns / delegations / agent_roles 持久化
- 作为 AgentOS 的唯一 HTTP/WS 入口，代理 model / skill / MCP 命令

禁止：

- 供应商密钥、模型路由、额度计算（归 model-layer）
- 技能内容、版本库、安装/更新（归 skill-layer）
- MCP server 生命周期、transport、工具发现（归 mcp-layer）
- SwiftUI / 页面状态 / 文案布局（归 AgentOS）
- 为每个外部 CLI 重写 adapter（adapter 是外部依赖）

模块边界：

```text
cli-layer/src/
├─ api/          AgentOS 命令适配；不写业务规则
├─ app/
│  ├─ roster/    Agent 定义 / readiness / test run
│  ├─ sessions/  conversations / turns / events
│  └─ team/      team snapshot / delegation broker / member lifecycle
├─ domain/      Agent / Conversation / Team / Delegation / Permission
├─ runtime/     ACP host / engines / overlay / permissions
└─ infra/store/ 唯一 SQLite 写入方
```

如果未来团队演进成**跨机器、长时间调度、council/roundtable 平台**，再按稳定接口
把 `team/` 抽成独立层；触发条件是“需要独立进程/独立部署/独立扩缩容”，不是
“文件行数变多”。

### 2.4 什么时候才拆

只有满足以下任一条件才考虑拆 `crew-layer`：

1. 团队编排需要在多台机器/远程 worker 上运行；
2. 需要独立于 App 生命周期长期驻留；
3. 需要独立的资源配额/多租户隔离；
4. cli-layer 的团队模块开始反向依赖 UI 或 provider 逻辑，边界已经失控。

否则保持一个进程、一个数据库、清晰模块。

### 2.5 当前不做

- MCP 的扫描/写入/按角色挂载（字段预留，核心命令后置）。
- Agent 自由群聊 / roundtable（后续可选）。
- 成员之间的递归委派（v1 深度固定 1）。
- 技能市场、Agent 市场、跨设备同步。
- 记忆系统的完整实现（字段预留）。

---

## 3. 当前基础与缺口

### 3.1 已具备

- `AgentRole`：名字、soul、rules、engine、provider、model、skills、mcp、
  permission_mode、auto_approve。
- Agents 页：身份、人格、能力、权限四段表单；模型面锁定不支持的引擎。
- `agent_roles_*`、`agent_role_model_face`。
- `skill_list` / `skill_read` / `agent_skills_set` / `agent_skill_status`。
- 会话时按 role 注入模型；role 有技能时调用 skill-layer `materialize`。
- 10 个引擎支持 per-role 模型注入；其余 5 个明确 `supported:false`。

### 3.2 必须补的缺口

| 缺口 | 影响 | 状态 |
| --- | --- | --- |
| AgentOS 仍用 `Int` 表示 folder / conversation id，cli-layer 用 UUID 字符串 | 新建会话、列表、详情、prompt 在壳与核心之间无法对齐 | **P0，阻塞** |
| AgentOS 只解码 `teamLeaderRoleID` | 成员状态、委派卡、取消入口没有数据 | P1 |
| 成员事件没有 `conversation_id` / `speaker_role_id` | 团队会话看不到成员实时过程 | P1 |
| 委派 usage 未汇总，artifacts / 重试 / 最大轮数未接 | 团队成本与失败恢复不可见 | P2 |
| 工具级 allow/ask/deny 未做 | 只能整模式放行，不能按工具收紧 | P2 |
| 团队成员池重启后按需重建 | 重启不能无缝恢复成员会话 | P2 |

---

## 4. Agent 领域模型

Agent 只引用资源，不内嵌资源实现。

```text
Agent
├─ identity
│  ├─ id / name / description / avatar
│  ├─ enabled / archived / version
│  └─ tags
├─ runtime
│  ├─ engine (cli)
│  ├─ model_provider + model
│  └─ limits (max_turns / timeout / budget，后续)
├─ persona
│  ├─ soul
│  └─ rules
├─ capabilities
│  ├─ skill_ids[]
│  ├─ skill_collection_ids[]
│  └─ mcp_ids[]（预留）
├─ permissions
│  ├─ permission_mode
│  ├─ auto_approve
│  └─ tool_rules[]（后续）
└─ memory
   └─ mode（off / session / project，后续）
```

规则：

- 不存 API key；key 只在 model-layer / Keychain。
- 不存 CLI 安装路径；只存 engine id。
- 不存 skill 内容；只存 skill id。
- 不存团队关系；团队关系属于 conversation。
- `version` 用于团队快照和变更提示，不要求每次编辑自增到全局版本号。

### 4.1 建议字段

在现有 `agent_roles` 基础上增加：

| 字段 | 用途 |
| --- | --- |
| `description` | 列表/卡片副标题，也给 Leader 选人 |
| `avatar` / `color` | 团队 UI 识别 |
| `enabled` | 停用但不删除 |
| `archived` | 有历史会话时软删除 |
| `version` | 团队快照对比 |
| `limits_json` | max_turns / timeout / budget |
| `memory_mode` | 预留 |

`skills_json` 继续存 skill id 数组；`mcp_json` 继续预留。

---

## 5. Agents 页面设计

### 5.1 列表

每行显示：

- 头像 + 名字
- 引擎图标 / 名称
- 当前模型
- 技能数量
- readiness 点：绿（可用）/ 黄（有警告）/ 红（不可用）
- 是否被团队引用

操作：

- 新建、复制、归档、删除
- “设为 Leader”（只是创建会话时的默认选择，不是全局状态）
- 导出/导入 JSON（后续）

### 5.2 编辑器

不要做一张无限长的表单，按“组装顺序”分段：

#### 是谁

- 名字
- 描述
- 头像/颜色
- 标签
- 启用/归档

#### 怎么跑

- 引擎
- provider / model
- 引擎安装状态
- 是否支持 per-role 模型注入
- 不支持的组合：显示原因，不静默回退

#### 人格

- soul
- rules
- Markdown 预览
- 可选的 role / goal / backstory

#### 能用什么

- 技能多选 + 搜索
- 技能集合一键选
- 已选 N / 可用 N / 缺失 N
- 缺失技能可以保存，但启动前必须报错
- MCP：占位，标“后续”

#### 怎么管

- 审批模式
- 自动批准开关
- 说明自动批准的代价
- 后续：工具级 allow/ask/deny、沙箱、网络、命令规则

#### 能不能用

固定 Readiness 面板：

```text
✅ 引擎已安装
✅ 模型可用
✅ 技能 10/10 可用
⚠️ 该引擎不支持 per-role 模型，将使用 CLI 默认模型
❌ 技能 pack-ios-dev 缺失
[测试运行]
```

### 5.3 Test Run

`agent_role_test_run`：

- 用当前 Agent 配置启动临时 ACP 会话
- 发一句最小提示
- 返回：成功/失败、实际模型、耗时、usage、错误
- 不写会话历史
- 不产生团队副作用
- 失败原因区分：引擎、模型、技能、权限、协议

这个功能优先于继续增加字段。

---

## 6. Readiness 协议

`agent_role_readiness(role_id)` 返回：

```json
{
  "ready": false,
  "blockers": [
    {"code": "skill_missing", "message": "pack-ios-dev 不存在", "fix": "重新安装或取消选择"}
  ],
  "warnings": [
    {"code": "model_fallback", "message": "该引擎不支持 per-role 模型，将使用 CLI 默认模型"}
  ],
  "resolved": {
    "engine": "claude_code",
    "model": "OpenCode Go/mimo-v2.6-flash",
    "skills": ["swiftui-expert-skill"],
    "mcp": []
  }
}
```

检查项：

1. 引擎是否存在、已安装、未禁用。
2. model 是否在该引擎的 `agent_role_model_face` 中且可用。
3. 技能是否都在 skill-layer registry 中。
4. MCP 后续。
5. 权限配置是否合法。
6. 是否有 blocking 的版本/能力冲突。

**启动会话和创建团队前必须调用 readiness。** 有 blocker 直接拒绝，有 warning
允许但要在 UI 明确显示。

---

## 7. 团队会话模型

### 7.1 三种模式

| 模式 | 说明 | 首版 |
| --- | --- | --- |
| `single` | 一个 Agent 一个连接 | ✅ 已有 |
| `team` | Leader 中介，成员被委派 | ✅ 首选 |
| `mention` | 用户 @成员直聊，仍写入同一会话 | 增强 |
| `roundtable` | 多 Agent 看共享上下文，自动选下一个发言者 | 后续 |

### 7.2 Leader 中介流程

```text
用户
  ↓
Leader Agent（一个 ACP session）
  ├─ 规划任务
  ├─ delegate_to_agent(member, task, context)
  │     ├─ Member A 独立 ACP session
  │     ├─ Member B 独立 ACP session
  │     └─ 结果回传 Leader
  └─ 汇总、验收、回复用户
```

规则：

- 用户只和 Leader 对话。
- Leader 拥有完整上下文。
- 成员只拿任务 + 必要上下文，不拿整段对话（控制成本与噪声）。
- 成员各自使用自己的 CLI / model / skills。
- 成员不能继续委派（v1 深度 = 1）。
- 成员会话懒启动，第一次被委派时才 connect。
- 取消向所有成员传播。
- 团队人数首版 2–5 人。
- 同一会话内成员连接可复用；会话关闭/空闲超时回收。

### 7.3 为什么先不做 Roundtable

Roundtable 需要：

- 共享上下文
- 发言选择器
- 终止条件
- 防连续发言/循环
- 更高的 token 成本

AutoGen 的 `SelectorGroupChat` 已经证明这套模式可行，但它适合“讨论/评审”，
不适合作为第一个团队执行模型。先做 Leader 中介更容易调试、计量和回滚。

### 7.4 团队快照

创建会话时冻结：

```json
{
  "leader_role_id": "role-manager",
  "members": [
    {"role_id": "role-frontend", "version": 3},
    {"role_id": "role-backend", "version": 2}
  ]
}
```

运行期角色被修改：

- 不热改当前会话
- 在会话头提示“成员配置已变化，重开会话生效”
- 新建会话使用最新版本

---

## 8. 数据模型

### 8.1 conversations

```sql
ALTER TABLE conversations ADD COLUMN kind TEXT NOT NULL DEFAULT 'single';
ALTER TABLE conversations ADD COLUMN leader_role_id TEXT;
ALTER TABLE conversations ADD COLUMN team_json TEXT;
```

`team_json` 存 7.4 的快照。空 = 单 Agent。

### 8.2 conversation_agents

```sql
CREATE TABLE conversation_agents (
  conversation_id TEXT NOT NULL,
  role_id TEXT NOT NULL,
  seat TEXT NOT NULL,          -- leader | member
  state TEXT NOT NULL,         -- idle | connecting | running | waiting | done | failed
  connection_id TEXT,
  joined_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (conversation_id, role_id)
);
```

### 8.3 delegations

```sql
CREATE TABLE delegations (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL,
  leader_role_id TEXT NOT NULL,
  member_role_id TEXT NOT NULL,
  child_conversation_id TEXT,
  task TEXT NOT NULL,
  context TEXT,
  status TEXT NOT NULL,        -- queued | running | done | failed | cancelled
  result_json TEXT,
  usage_json TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
```

### 8.4 turns

```sql
ALTER TABLE turns ADD COLUMN speaker_role_id TEXT;
```

- `speaker_role_id` 为空 = 用户或系统
- 非空 = 对应 Agent
- UI 用它渲染成员名/头像

---

## 9. 核心 API

### 9.1 Agent 定义

```text
agent_roles_list
agent_roles_save
agent_roles_delete
agent_role_readiness      新增
agent_role_test_run       新增
agent_roles_clone         新增
```

### 9.2 技能

```text
skill_list
skill_read
skill_install
agent_skills_set
agent_skill_status
```

### 9.3 团队会话

```text
create_conversation       接收 team
get_conversation          返回 team + members + speaker
conversation_agents       新增：成员实时状态
cancel_delegation         新增
cancel_conversation       新增：取消所有运行中的成员委派
retry_delegation          新增
```

`delegate_to_agent` 是 **Leader 会话内部工具**，不是 AgentOS 直接调用的 UI 命令。

---

## 10. 委派协议

`delegate_to_agent` 参数：

```json
{
  "roleId": "role-frontend",
  "task": "实现登录页的 SwiftUI 视图",
  "context": "只改 Sources/Login，保持现有设计 token",
  "expectedOutput": "代码 + 自测结果"
}
```

返回：

```json
{
  "delegationId": "dlg_...",
  "status": "done",
  "summary": "...",
  "artifacts": ["path"],
  "usage": {"input": 0, "output": 0, "durationMs": 0},
  "error": null
}
```

校验：

- 只在团队会话可用。
- 只接受团队成员。
- 只允许 Leader 调用。
- 成员不返回“我完成了”就算完成；Leader 负责验收。
- 成员失败要原样返回错误，不静默换人。

---

## 11. 权限设计

Agent 的权限在 Agents 页一次配置：

- `permission_mode`：Agent 级选择，值来自 `agent_role_permission_face` 暴露的
  ACP Session Modes；启动会话后通过 `session/set_mode` 应用。只接受当前引擎
  原生 mode id，不再做旧产品模式别名兼容；历史 `default` 在启动迁移为空字符串。
- `auto_approve`：该角色的权限请求自动批准；默认只选 `AllowOnce`，审计事件
  为 `permission_auto_approved`
- `tool_rules`：后端已支持按 tool/kind/path_prefix 匹配 allow/ask/deny；
  第一条匹配生效，无匹配时回退 auto_approve/UI
- 自动批准必须写审计日志
- 成员权限独立于 Leader
- 不允许 Leader 偷偷提升成员权限
- UI 在 Agent 编辑器和团队会话头都显示“此 Agent 自动批准”

后续工具级规则：

```text
allow / ask / deny
按工具名、命令模式、路径、网络域
```

---

## 12. 事件与 UI 归属

事件 envelope 增加：

```json
{
  "connection_id": "...",
  "conversation_id": "...",
  "role_id": "...",
  "speaker_role_id": "...",
  "type": "content_delta | tool_call | permission_request | turn_complete"
}
```

UI：

- Leader 主消息流
- 成员 chip：排队/连接中/运行/等权限/完成/失败/取消
- 委派卡：成员名、任务、状态、耗时、usage、打开子会话
- @ 面板：团队成员置顶，非成员标“不在本次团队”
- 失败成员：显示错误和“重试/移除”

---

## 13. 开源参考

| 项目 | 许可 | 可借鉴 | 不直接使用 |
| --- | --- | --- | --- |
| [CrewAI](https://github.com/crewAIInc/crewAI) | MIT | role/goal/backstory、`allow_delegation`、hierarchical manager | Python 运行时、任务 DSL |
| [AutoGen](https://github.com/microsoft/autogen) | MIT | `SelectorGroupChat`、终止条件、发言选择、最大消息数 | 作为首版团队模型 |
| [Agentrove](https://github.com/Mng-dev-ai/agentrove) | Apache-2.0 | Channels、sub-threads、persona、每 Agent 模型、成员状态、内联审批 | Python/React 技术栈 |
| [claw-orchestrator](https://github.com/Enderfga/claw-orchestrator) | MIT | persistent session、session handoff、fanout/council、预算/ledger、验证平面 | 全套运行时 |
| 旧核心 delegation | Apache-2.0 | team JSON、Leader 团队帧、成员状态条、broker 门禁、成员模型隔离 | 整个旧核心 UI/运行时 |
| [OpenAB](https://github.com/openabdev/openab) | MIT | ACP session pool、进程生命周期 | Chat 平台假设 |
| [Agent Client Protocol](https://agentclientprotocol.com) | Apache-2.0 | session/update、permission、cancel、load | 团队语义（协议本身不提供） |

结论：**抄模式，不抄运行时。** 我们的 cli-layer 已经比这些项目更贴近当前产品边界，
只需要补团队编排和 readiness。

---

## 14. 实施顺序

### Phase A：Agent 定义 + 就绪校验（先做）

1. 扩展 `agent_roles`：description、avatar、enabled、archived、version、limits。
2. 新增 `agent_role_readiness`。
3. 新增 `agent_role_test_run`。
4. Agents 页 readiness 面板 + Test Run。
5. 启动会话/创建团队前强制 readiness。

**验收**：Agent 配置错误不会等到会话中才报；Test Run 能显示实际模型与技能。

### Phase B：团队持久化 + Leader 委派

1. ✅ `conversations` 补 kind/leader/team 快照。
2. ✅ `conversation_agents`、`delegations`。
3. ✅ `turns.speaker_role_id`。
4. ✅ `create_conversation` 真正保存 team。
5. ✅ Leader 团队帧：内部 stdio MCP。
6. ✅ `delegate_to_agent` broker。
7. ✅ 成员懒启动、独立 overlay、独立模型、连接复用与空闲回收。
8. 🟡 成员状态 / 委派卡 / 汇总：核心状态与 turn 已落库，UI 待接。
9. 🟡 取消、超时、最大轮数、最大成员数：取消/超时/最大成员数已实现，
   最大轮数与预算待补。

**验收**：一次真实任务由 Leader + 2 名成员完成；每个成员使用自己的模型；
越界委派被拒绝；刷新后团队和状态不丢。

### Phase C：权限 + 用量 + 恢复

1. ✅ `auto_approve` 已生效；`permission_mode` 走 ACP `session/set_mode`，不支持的引擎/模式在 readiness 直接阻断。
2. ✅ 自动批准写 `permission_auto_approved` 审计事件。
3. 🟡 委派 usage 已落 `delegations`，尚未汇总到会话/成员用量。
4. ✅ `acp_connect` 接收 `sessionId` 时走 ACP `session/load`。
5. 🟡 单 Agent/Leader 可恢复；团队成员的池化连接仍按需重建。

### Phase D：MCP 与其他协作模式

1. MCP 扫描/写入/按角色挂载。
2. Roundtable / fanout / council。
3. Session handoff。
4. 技能市场 / Agent 模板。

---

## 15. 产品决策（建议默认值）

| # | 决策 | 建议 |
| --- | --- | --- |
| 1 | 团队默认模式 | Leader 中介 |
| 2 | 成员能否再委派 | 不能，深度 1 |
| 3 | 成员上下文 | 任务 + 必要上下文，不拿完整 transcript |
| 4 | 团队快照 | 创建会话时冻结，运行期改动提示重开 |
| 5 | 成员权限 | 继承 Agent 配置；Leader 不能偷偷提升 |
| 6 | 团队人数 | 2–5 |
| 7 | Test Run | 临时会话，不写历史 |
| 8 | Agent 删除 | 有历史会话时归档，不硬删 |
| 9 | Roundtable | 不进首版 |
| 10 | MCP | 后置，字段/readiness 先预留 |

---

## 16. 当前落地状态与下一步（2026-10-24）

### 已落地

- `agent_role_readiness` / `agent_role_test_run`
- 团队 schema：`conversations.kind/leader_role_id/team_json`、
  `conversation_agents`、`delegations`、`turns.speaker_role_id`、`agent_roles.version`
- 团队快照、Leader 控制 MCP、`delegate_to_agent`、成员连接复用/取消/空闲回收
- `acp_connect` 的 `session/load` 与 `conversations.external_id`
- Agent 权限：`agent_role_permission_face`、ACP `session/set_mode`、
  `auto_approve` 只选 `AllowOnce` 并审计

### P0：先修 AgentOS 与 cli-layer 的 ID 契约

现在壳仍按旧 codeg 的 Int ID 编码：

```text
Workspace.id / Conversation.id / folderID: Int
CoreCreateConversationBody.folderID: Int
CorePromptBody.folderId / conversationId: Int
CoreFolderConversationBody.conversationID: Int
```

cli-layer 的真相源是 UUID 字符串，且 `field()` 只接收字符串。结果是：
新建会话不会绑定项目，列表/详情解析失败，prompt 不会落用户 turn。

决策：**不做数值兼容层，AgentOS 全部改成 String UUID**。
验收：新建 → 列表 → 详情 → prompt → 刷新后 turn 仍在。

### P1：团队读模型与实时事件

- ✅ 核心 `summary` 已返回完整 `team`、`members[].state`、`members[].usage`、
  `delegations`、会话级 `usage`
- 🟡 AgentOS 解码完整团队读模型
- 🟡 会话头成员 chip、委派卡、取消按钮接 `/api/cancel_delegation`
- ✅ 事件 envelope 补 `conversation_id` / `speaker_role_id`；连接建立时登记角色/会话
- ✅ `delegation_started` / `delegation_completed` / `delegation_cancelled` 事件落库
- ✅ `/ws/events` attach 支持按 `conversation_id` 回放和过滤
- ✅ Leader 连接的 `conversation_agents.state/connection_id` 随连接更新
- ✅ 成员委派遇到 permission request 不再立即失败，等待审批；Test Run 仍快速失败
- 🟡 前端消费团队事件流

### P2：团队执行质量

- ✅ usage 汇总到会话/成员（`summary.usage`、`members[].usage`）
- ✅ artifacts 从 ACP tool call locations 提取并写入 delegation result
- ✅ 失败/取消委派可重试（`retry_delegation`）；最大轮数、预算未做
- ✅ 成员 ACP session id 持久化到 `conversation_agents.external_session_id`；
  池为空/重启后下次委派尝试 `session/load` 恢复

### P3：工具级权限

- 🟡 后端 `tool_rules` 已支持 allow / ask / deny；UI 尚未提供编辑入口
- 🟡 当前自动拒绝使用 `RejectOnce`；`RejectAlways` 留给后续规则记忆
- 不改变“权限归 Agent”的产品边界

---

## 17. 验收标准

- [ ] Agent 编辑页能看到引擎、模型、技能、权限的真实可用性。
- [ ] AgentOS 的 folder / conversation 全部使用 String UUID。
- [ ] 新建会话 → 列表 → 详情 → prompt → 刷新后 turn 仍在。
- [ ] 有 blocker 的 Agent 不能启动会话，也不能加入团队。
- [ ] Test Run 能在不写历史的情况下跑一轮。
- [ ] 创建团队会话后重启 App，Leader/成员/状态仍能恢复。
- [ ] Leader 只能委派团队成员；越界被拒绝。
- [ ] 每个成员使用自己的 CLI、模型、技能。
- [ ] 团队成员消息在 transcript 中带 speaker。
- [ ] 成员失败/取消不会静默换人。
- [ ] 权限设置真正生效，自动批准有审计。
- [ ] 团队结束后成员会话和 sidecar 被回收。

---

## 18. 与现有计划的关系

- `cli-layer-migration.md`：本文件是 Phase 3 之后 Agents/团队部分的细化。
- `skill-layer-api.md`：技能选择、readiness、materialize 的契约来源。
- `agentos-shell.md`：AgentOS 只负责 UI 和命令调用。
- `archive/agents-core-collaboration.md`：旧核心时代的历史方案，保留作实现参考，
  不再作为产品运行时依据。

后续实现若改变本文的 API 或数据表，必须同步更新本文件。
