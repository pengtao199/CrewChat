# 多 Agent 单会话

Updated: 2026-10-10 (Asia/Shanghai)
Status: Superseded（2026-10-11）。代码已落地（group.py / chat.py 保留）；协作界面迁到 codeg/，Guid 走查作废。若 Python 编排以 Codeg 自定义 ACP agent 复活，另开计划。

## Goal

用户在 Guid 选择一个项目工作区和多个 Agent 后，进入同一个 `/conversation/:id`。内置管理员 Agent 自动加入并担任 Leader：每轮先拆解，按依赖委派给成员，最后汇总；消息仍按 Agent 分气泡显示并可刷新恢复。

## Current Facts

- 后端核心已具备：`src/crewchat/orchestration/group.py` 有共享 `prior`、`handoff()`、`plan()`、依赖排序和步数上限；`src/crewchat/api/chat.py` 已串行执行多个 Agent，并为每个发言者发独立消息。
- 会话表已有 `project_id` 和 JSON `agent_ids`；`src/crewchat/api/conversations.py` 创建时只读取 `extra.team_agent_ids`，Guid 当前只发送 `agent.id`，所以多选链路尚未接通，单选也可能回退到任意第一个 Agent。
- 前端 Guid 目前是单选；`useGuidAgentSelection.ts` 同时承担单 Agent 的模型、模式和能力覆盖。
- 同一时间线的多发送者渲染已存在；需要修正 ACP 流处理中「任意 `finish` 都结束整轮」的行为，并统一流式与落库 `msg_id`。
- 上游 `/team/:id` 代表「多个独立 conversation」，且 `/api/teams/*` 仍是 501；它不符合本功能的「一个会话、一条时间线」。
- 协作循环本身（共享上下文、handoff、plan）已在 `docs/plans/archive/crewchat.md` 验收，本计划不再重做那几刀。

## Product Decisions

- 多 Agent 模式必须绑定一个 `project_id`；所有成员共享该项目路径，执行保持串行，避免同时写同一工作区。
- 固定可见 Leader id 为 `crewchat-manager`。它是普通 `AgentSpec` 和普通 `.data/agents/*.jsonc` 文件，不新增独立 Agent 类型。
- 管理员模板默认使用 CLI（`opencode`），但不做隐式大脑回退。首次使用前必须明确选择 provider/model 并写入管理员 Agent 文件；缺配置时入口给出修复动作。
- 管理员可在 Agents 设置中编辑或删除；删除后多 Agent 入口提供「恢复管理员」而不是每次启动强制重建。
- 首版成员在创建后固定；会话内增删成员、并行执行和 `@Agent` 路由留到后续。

## Non-Goals

- 不实现 `/api/teams/*`、`/team/:id`、Team board 或每成员独立 conversation。
- 不把现有文本 `plan()` / `handoff()` 改成 MCP 工具，不引入 crewai 执行循环。
- 不做并行工作区写入，不做运行中改成员，不让 Agent 自己启动另一个进程。
- 不把会话级模型覆盖批量套到多个 Agent；每个 Agent 使用自身大脑和引擎。

## Approach

在现有单会话协作循环上打通创建入口，并把默认「所有人依次回答」改成管理员先规划。继续复用 `/conversation/:id` 和 `message.stream`。

## Work Slices

### Slice 1：内置管理员与会话契约

- Intent：创建时能稳定绑定 Leader + 成员 + 项目；单聊读 `agent.id`，不再静默选花名册第一个。
- Files：`src/crewchat/core/agent_presets.py`（新）、`src/crewchat/api/crewchat_agents.py`、`src/crewchat/api/store.py`、`src/crewchat/api/conversations.py`、`src/crewchat/api/chat.py`、`ui/.../ipcBridge.ts`。
- Steps：管理员模板、状态、恢复；会话表加 `leader_agent_id`；创建校验项目、Leader、可用成员；发送时绑定失效返回 400。
- Verify：API 测试覆盖单聊映射、多 Agent+项目落库、未知/禁用成员、缺工作区、管理员未配置和重启后读回。

### Slice 2：Leader 驱动的协作状态机

- Intent：群聊默认由管理员规划，只跑计划内成员，最后由管理员汇总。
- Files：`src/crewchat/orchestration/group.py`、`src/crewchat/api/chat.py`。
- Steps：管理员先跑；无有效 `plan()` 则直答收束；成员失败仍进入汇总。
- Verify：无计划直答、按依赖执行、未知 owner、成员失败后汇总、handoff、取消。

### Slice 3：Guid 多选创建体验

- Intent：用户能选多个 Agent 并进入同一会话。
- Files：`GuidPage.tsx`、`AgentSelectionArea.tsx`、新建 `useGuidTeamSelection.ts`、页面私有多选组件、`useGuidSend.ts`。
- Steps：单/多 Agent 切换；管理员固定可见；必须选项目和工作成员才能进入。
- Verify：模式切换、去重、管理员不可移除、缺工作区禁用、payload；浏览器确认。

### Slice 4：会话内身份与流式状态

- Intent：中间成员的 finish 不结束整轮；直播与历史用同一个 `msg_id`。
- Files：`hub.py`、`chat.py`、`useAcpMessage.ts`、runtime store、会话头。
- Steps：`persist_turn` 接受流式 id；ACP 只在最终 finish 或 `turn.completed` 时解锁发送框。
- Verify：流序列测试断言气泡不合并、发送框不中途恢复、刷新后身份不丢。

### Slice 5：端到端验收与文档对齐

- Intent：规则与实现一致，真机能走完一轮。
- Files：测试、`AGENTS/agent-model.md`、`AGENTS/api-contract.md`。
- Verify：`uv run pytest -q`、分层检查、前端构建、浏览器走查。

## Risks

- 管理员必须有显式大脑；不能为了「开箱即用」恢复已经禁止的默认端点。
- 文本 `plan()` 写错时必须退化成管理员直答，不能退化成所有成员无条件重复回答。
- 现有前端 `finish` 处理会过早结束群聊；Slice 4 是上线前阻断项。

## Done Criteria

- Guid 可在多 Agent 模式选择项目和至少一个成员，管理员自动加入，创建后进入单一 `/conversation/:id`。
- 会话重启后仍保留 `project_id`、`leader_agent_id`、成员顺序和消息身份。
- 每轮由管理员先规划；只运行计划内成员；最后由管理员汇总。无有效计划时只返回管理员回答。
- 中间 finish 不会提前解锁发送框。取消后不再启动排队成员。
- 单 Agent 创建与对话保持兼容，不再随机回退到第一个 Agent。
