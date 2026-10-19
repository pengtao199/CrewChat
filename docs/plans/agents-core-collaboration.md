# Agents 核心协作（Codeg 原生委派增强）

Updated: 2026-10-19 (Asia/Shanghai)
Status: In Progress（Phase 5 平台收尾）。Phase 1 协作 v1 已收口；Phase 2 角色化（2.1 / 2.2 / 2.3）✅；Phase 3 成员模型与用量（3.1 / 3.1.1 / 3.1.2 / 3.1.3 / 3.2）✅（3.1 有 v1 边界：仅 claude 注入角色模型）；Phase 4.1 卡内内联成员流 🟡（代码完成、测试绿，真机验收待用户）；4.1.1 卡显示成员名 ✅（含一处 `DelegationLink` 成员身份修正）。**4.6 权限归属到每个 Agent ✅（5/5 代码完成：`e53a300` / `0e31a36` / `c1f1546` / `07b0059`，真机验收待用户）**。0.3 路由注入完成；**模型注入通道已删**（3.1.2：claude 运行期切模型不可用，按角色定模型只留 3.1 的隔离 home）。**5.1 CCR 托管 ✅ + 5.1.1 验证脚本按改动拆 ✅**（真机复测：`探活 → 接管 → 拉起 → 守护` 全链路待用户验收）。基线（CLI 页就绪卡、CCR 模型页 v2、花名册 v1、`ccr.ts` profile 读出/构造层）已在仓库。`codeg-ccr-integration.md` 的未完成续作并入本计划（见 0.0）。
Next: 4.4 Agent 优先（`3c199f8`）✅、4.2 团队 @ 联动（`510d45a`）✅。接下来 4.3 异步回流（补 Goal 第 3 条的「汇总」）→ Phase 5 平台收尾；4.1 卡内内联流与 5.1 CCR 托管真机复测仍待用户验收。2026-10-18 已同步真相源文档（`AGENTS/truth-sources.md`、`agent-model.md`、`architecture.md`、README）。

## Goal（终态）

工作台里：

- **CLI 页只管「能不能跑」**：就绪卡（安装 / 启用 / CCR 绑定），模型字段收在「高级」，不做选择。
- **Agents（花名册）是唯一选人入口**：用户选 1 个或多个 Agent 进项目会话。
- 多 Agent 会话：**管理员先规划 → 按依赖委派给在场成员 → 汇总**；成员共享项目工作区。
- 全过程由 Codeg 原生机制承载：流式 / 思考 / 工具 / 权限 / 取消 / 恢复 / 嵌套子会话沿用现成实现。
- 每个成员用自己的模型（经 CCR）、技能与 MCP。

路线：**Codeg 原生委派增强**（`codeg/src-tauri/src/acp/delegation/` 的 broker 已有异步任务、长轮询、取消级联、断点恢复、权限透传、用量回传）。不桥接 Python 编排；`archive/python-orchestrator/src/crewchat` 冻结为参考（产品语义沿用：管理员先计划、串行执行防同工作区互写、成员创建后固定）。

## Current Facts（2026-10-12）

- `codeg/`：就绪卡（`agent-readiness-card.tsx` / `use-agent-ccr-binding.ts`）与 profile 读出/构造层（`src/lib/ccr.ts`）已提交；全量测试 602 文件 / 8903 用例通过；server 模式（3080）可用。
- 委派通电（2026-10-12 实测）：server 带 `CODEG_MCP_BIN=target/debug/codeg-mcp` 启动、`delegation.enabled=true`（depth 2）。claude 主会话真机委派给 claude 子会话——子代理回答经 `get_delegation_status` 回传（7592ms）；委派卡（agent + task + done 徽章）、状态卡（Waiting for task）、抽屉（Open conversation → 子会话内容）全部渲染；`cancel_delegation` 实测后子会话状态 `cancelled`（error_code: canceled）。启动方式已固化进 `codeg/NOTICE.md`。
- 就绪徽章（0.2，2026-10-12）：花名册列表行按引擎显示 CCR 四态徽章；编辑器模型区在非 bound 态置灰并给引导（可绑定 → 「去 CLI 页处理」带 `?agent=`；不可达 → 「重试」；不支持 → 只读说明）。渲染实测：已绑定（Claude Code）与暂不支持（Gemini）两态 + 模型区恢复；`ccrStateForBody` / `profilesFromConfig` 纯函数用例覆盖 bindable / unreachable / loading。
- 环境纪事（2026-10-12）：① `cargo test`（默认 features，含 tauri-runtime）会把 `target/debug/codeg-server` 重建成 debug+tauri-runtime 版——该组合**按设计使用 `codeg-dev.db`**（空库），server 模式启动前必须 `cargo build --no-default-features --features server-bin --bin codeg-server`（否则静默换库）；② 同日 `~/.claude/settings.json` 的路由三件套与 apiKeyHelper 曾被 CCR 的全局清理逻辑移除（claude 报 "Not logged in · Please run /login"），已按备份恢复（含 `ANTHROPIC_MODEL`）；③ 后台进程（codeg-server / CCR）可能被会话清理带走，起法带 `nohup … & disown` 更稳。
- Agent Host 配方（0.3，2026-10-12）：`agent_host.rs` + 接线已提交（见 NOTICE）；路由注入经 request-log 验证（会话经网关 200；关注入回落全局配置）；per-session 模型受 claude 启动期确认机制限制，隔离路径已实测（见下节笔记）。

### Agent Host 实测笔记（2026-10-12，给 3.1）

- **claude 的模型是启动期配置**：`CLAUDE_CONFIG_DIR/settings.json` 的 `ANTHROPIC_MODEL` 决定会话模型；且 **settings.json 的 env 优先于进程 env**（向进程注入 `ANTHROPIC_MODEL` 不生效）。
- **运行时切换受限**：`session/set_config_option("model", …)` 只接受"当前配置模型"；切其它模型（含网关虚拟 ID）在会话建立时报 `Couldn't confirm model …`——即使该模型在 `/v1/models`（claude UA）与 `/api/claude_cli/bootstrap` 的全量列表（19 项）里。裸名注入还会直接把会话打断（connected → error ~6s，无请求到达网关）。
- **隔离方案可行（已实测）**：最小 `CLAUDE_CONFIG_DIR`（settings.json = 路由 env + `ANTHROPIC_MODEL` + `apiKeyHelper` 指向 CCR key 脚本）下 `claude -p` 以目标模型成功（request-log `mimo-v2.6-pro` 200；`unrecognized_model` 警告无害）。
- **3.1 待解**：隔离 home 的会话文件可见性（Codeg 的 claude 解析按 `~/.claude` 与进程级 env——候选：`projects/` symlink 回共享目录，或 parser 增强）；并行会话的隔离目录管理 / 清理；用户其他 claude 配置（permissions 等）的继承度。
- 0.3 保留的 config option 通道在"profile 模型 = 当前配置模型"时是 no-op；会话级显式选择（`agent_defaults`）仍走它。3.1 若采用隔离方案，可保留为兜底或移除。
- `ccr/`：v3.1.3 固化；开发实例 7 个 profile 均 `scope: ccr`，默认档 `mimo-v2.6-flash`（kilo=deepseek-v4.1-flash）。委托链路实测：仅 claude 经全局 `settings.json` 走网关；其余 CLI 由 Codeg 拉起时绕过 wrapper（`~/.claude-code-router/bin` 不在 PATH）。
- 委派：`delegation.enabled` 默认 false；codeg-mcp 二进制在 `.data/codeg/bin/`，server 模式需 `CODEG_MCP_BIN`；委派 UI（卡 / 组卡 / 抽屉 / 后台条 / 悬浮面板）齐备。
- 花名册：Codeg 侧 v1 页面（`.data/codeg/agent-roles.json`）；Python 侧 `.data/agents/*.jsonc`（10 份，冻结）。
- 近期提交均未推送，待整理。

## 开工拍板（默认值；无异议按默认执行）

| # | 决策点 | 默认 | 备选 |
| --- | --- | --- | --- |
| 1 | 团队存储 | conversations 专用 JSON 列（leader + 成员） | 复用标签系统 |
| 2 | 角色人格通道 | 自定义 agent 注册表加 `instructions` 字段 | 每角色技能包 |
| 3 | v1 接续方式 | 管理员中介（任务文本带上文） | broker 自动附兄弟结果 |
| 4 | 花名册落点 | Codeg 自定义 agent 注册表（扩展 brain/instructions）+ 一次性迁移、旧存储退役 | 保留 fork 独立表 |
| 5 | 异步回流 | 短任务轮内长轮询；长任务后台 + 完成后一键汇总 | 全自动续跑 |
| 6 | Python 编排 | 冻结为参考（不删不接） | 未来服务端/远程编排另开计划 |

## 步骤计划

### Phase 0 · 通电与收尾

- **0.0 计划对齐** ✅ 2026-10-12：`codeg-ccr-integration.md` 归档（`archive/` + 索引收敛），活跃计划只留本文件。
- **0.1 委派通电** ✅ 2026-10-12：server 启动环境带 `CODEG_MCP_BIN`（起法固化进 `codeg/NOTICE.md`）；「多智能体协同」打开（enabled，depth 2）；真机跑通一轮 delegate——子代理结果回传、卡 / 抽屉 / 状态 / 取消全部实测通过（证据见 Current Facts）。
- **0.2 就绪徽章** ✅ 2026-10-12：花名册每行按角色引擎显示 CCR 绑定四态；未就绪 → 模型区置灰 + 跳转 CLI 页（`?agent=` 预选）；渲染实测两态。
- **0.3 模型注入（Agent Host 配方 v1）** 🟡 2026-10-12（路由注入完成；模型通道就绪、受 claude 启动期确认机制限制——实测笔记见下，生产化随 3.1）：独立模块 `codeg/src-tauri/src/agent_host.rs`——env 只做路由（BASE_URL 三件套 / gateway discovery / compat / NO_PROXY），模型经 ACP `session/set_config_option("model", <网关虚拟 ID>)` 在 `spawn_agent` 统一接线（`agent_defaults` 显式值优先、`or_insert`；虚拟 ID 由配方查询网关 `/v1/models` 精确解析——profile 的 key + claude UA、hex 匹配、优先 `[1m]` 变体；不在列表 → 只保留路由注入，不打断会话）。回退开关 `CODEG_AGENT_HOST_INJECT=0`（默认开；关掉即完全恢复原行为）。→ request-log 实测：会话经网关 200（路由注入生效，关注入回落全局配置）；**per-session 模型**的可行路径已实测找到（隔离 `CLAUDE_CONFIG_DIR`，请求日志出现目标模型），其生产化并入 3.1（依赖链：3.1 → 0.3）。
- **0.4 小收尾** ✅ 2026-10-12：`zz-permtest-*` 清理（3 个测试遗留文件已删）；未推送提交整理——自 `62104f0`（vendor Codeg）起本地共 45 个提交，均按主题单独立意；抽查无密钥 / 构建产物混入（`.env` 在忽略清单；`git log -S "sk-"` 仅命中删除类提交）。

### Phase 1 · 团队闭环（协作 v1，核心）

- **1.1 团队数据契约** ✅ 2026-10-12：conversation 增 `team` JSON 列（`leader_role_id` + `member_role_ids`，存花名册角色 id；空 = 单 Agent 会话）；创建 / 读取 / 重启回读一致，损坏 JSON 降级为无团队；web + tauri 双入口接参；fork 与保留行继承 team。测试：回读一致、None 保持、损坏降级、迁移 up/down。提交 `76da706`。
- **1.2 选团队建会话** ✅ 2026-10-12（前端）：草稿欢迎屏与头部都有 `TeamSelector`（leader 单选 + 成员多选，默认建议管理员角色；空 = 单 Agent 会话）；`createConversation` 带 team 随首条消息落库。真机验证：UI 建 3 人团队会话（管理员 + 前端 + 运维），DB `team` JSON 回读一致，模型链路 request-log 200（`mimo-v2.6-flash`）。提交 `08da565` + 收尾。
- **1.3 管理员知情 + 目标约束** ✅ 2026-10-12：新增 `acp/delegation/team.rs`（`TeamLookup`；生产 `DbTeamLookup` = 会话 `team` 列 + 花名册，解析成员：名字 / 职责（soul 首行、60 字截断）/ `agent_type`；读取失败降级为「不约束」并告警）。broker `start_delegation` 前置门禁：父会话带团队时 `delegate_to_agent` 仅接受团队成员，越界返回 `not_in_team` 并列出可用名单（`名字 (agent_type)`）。提示帧新增第二种内部帧 `codeg_internal_team`（在场成员 / 职责 / 可派 `agentType`；与路由帧共用分隔符、字节级身份与剥离管线，查找器泛化为多帧 kind），仅委派开启的连接在最终边界（`prepare_agent_bound_prompt`）追加。真机证据：管理员 prompt 落帧（`~/.claude/projects/…/6ebc73aa….jsonl` 两轮、含职责行）；管理员按帧自我规划（拒 gemini、改提成员）；真实越界调用 `delegate_to_agent(agent_type=gemini)` 被 broker 拒（tool_result `is_error` 原文含名单）；`get_conversation` 解析结果零帧泄漏（剥离生效）。测试：新增 15 个针对性用例；delegation + connection + parsers **1526 绿**；clippy 本模块零新增告警。提交 `7d3e06d`。
- **1.4 会话头成员条** ✅ 2026-10-12（用户 UI 验收通过）：`ConversationDetailHeader` 标题行下新增 `TeamMemberStrip`——每个成员一个 chip（状态点 + 名字 + 状态：排队 / 运行 / 等权限 / 完成 / 失败 / 已取消 / 待命；悬停显示任务预览）。状态解析 `lib/team-member-status.ts`：子会话行（DB，刷新安全）× 实时 binding（running 由快照重播，终态回落子行）；「排队」= binding 已到而子行未落的生成窗口（broker 无队列，排队语义即此）；「等权限」由 chip 订阅子连接 `pendingPermission` 升级。数据流：团队快照（workspace store）+ 花名册 + `listChildConversations`（delegation 事件防抖重取）+ `delegation-context.listBindings()`（新增）。非团队会话与草稿不渲染；i18n 10 语言。测试 22 用例（纯解析 15 + 组件 7）；tsc / eslint 零告警；conversations+chat+contexts+stores 1988 绿。提交 `75314c2` + 拆分前置 `74604ab`。
- **1.5 端到端实跑** ✅ 2026-10-12（暂时收口）：管理员调用链真实可用（越界拒绝、成员可派、成员条三态可视且刷新不丢——证据见 1.3 / 1.4）。**模型路由细化未达预期**（每成员模型未生效），用户拍板先收口、随 3.1 处理。→ **协作 v1 验收门槛就位**。

### Phase 2 · 角色化（花名册一等公民）

- **2.1 存储收敛** ✅ 2026-10-12：角色入 Codeg 自定义 agent 注册表——`custom_agent` 增 8 列（kind / body / instructions / rules / brain_model / brain_provider / skills_json / mcp_json；迁移 `m20261012_000002`；旧行默认 `kind='acp'` 行为不变）。新 `db/service/agent_role_service.rs`：角色 CRUD（`kind='role'`，与 ACP agent 共 id 空间、冲突拒绝）+ 一次性遗留迁移（`agent-roles.json` 先备份再导入、`.data/agents/*.jsonc` 去注释导入、重名以 Codeg 名册优先跳过、`app_metadata` 标记、非破坏可回滚）。发射面经 `list_launchable_defs` 与 role 行隔离（hydrate / 自定义 agent 列表 / 配置同步）。入口切换：命令层改 DB 后端（命令名与 wire 形状不变，前端零改动）、`team.rs` 直连服务、启动点挂迁移。真机：启动迁移 `codeg=4 python=6 skipped=2 failed=0`（管理员 / 前端工程师重名跳过），备份 `.pre-phase2.bak` 落盘，API 10 角色，自定义 agent 列表不含角色。测试：15 + 80 用例绿；前置拆分第五批（`custom_registry` 目录化）。提交 `a9e5d99` + `1134014`。→ 做到：单一写入方、单一真相源；设置页全功能。
- **2.2 人格生效链路** ✅ 2026-10-12：第三种内部帧 `codeg_internal_role`——角色文档（名字 / instructions / rules）以单行 JSON 落帧，保持三行体例、复用既有查找与剥离管线；在主会话与成员会话的最终边界注入，**不受委派开关限制**。角色解析：`TeamLookup::conversation_role`（会话 `team.leader_role_id` → 名册角色；无团队 / 失败降级为不注入）。成员链路：门禁解析出的成员表把 `role_id` 写进 `DelegationLink`，`create_with_delegation` 将成员角色写入子行 `team`（`member_role_ids` 空），子会话由此解析出自己的人格。真机：管理员会话（21 / 29）落「管理员」人格帧；委派「后端工程师」(claude_code) 后子行 30 `team={"leader_role_id":"backend-architect"}`、子会话落完整「后端工程师」人格帧（instructions 为迁移合并的 role+goal+backstory）。测试：角色帧 5 用例 + broker 链路断言 + 子行 role 用例；3113 用例绿；前置拆分第六批（`agent_mentions` 目录化）。提交 `51fdaa3` + 拆分提交。→ 做到：有 / 无角色的对照差异可见（无团队 = 不注入）。
- **2.3 技能 / MCP 归位** ✅ 2026-10-12：**MCP 按角色收窄**——`SessionState::role_mcp`（spawn 期注入）贯通 `spawn_agent` → `spawn_agent_connection` → `session/new`；委派链路把成员角色的 MCP 名单经 `ConnectionSpawner::spawn` 带进子会话，`load_mcp_servers_for_agent` 只挂角色选中的服务器（空选择=继承 agent-type 全集；缺失项告警），收窄规则抽成独立可测模块 `acp/mcp_scope.rs`。**技能进角色帧**——角色帧升至 v2，携带 `skills`（去重/截图截断）并条件化提示「按场景使用列出的技能（已为该 CLI 安装）」。真机（团队会话 32/34，成员=MCP 验证角色[mcp=codegraph, skills=frontend-design]）：broker 日志 `mounts the member role's MCP selection: ["codegraph"]`；子会话 `session/new` 日志 `kept 1 server(s) codegraph`（claude 的另一全局服务器被角色收窄掉）；子会话落 v2 角色帧含 `"skills":["frontend-design"]` 与技能提示句。测试：mcp_scope 4 + 角色帧技能 + broker 链路断言；3265 用例绿。v1 边界：① 主会话（UI connect 路径）的 MCP 收窄未做——会话建立早于角色可知（spawn 链可后续接）；② 技能为 advisory（内置 CLI 的技能由自身目录发现，Codeg 不做每会话文件级隔离）；③ 角色帧 1→2 版本变更按预发布策略放弃旧帧剥离。提交 `b24afb0`。

### Phase 3 · 成员模型与用量

- **3.1 子代理模型注入** ✅ 2026-10-12：成员角色的大脑（model/provider）在委派 spawn 时落成**每会话一个全新的隔离 `CLAUDE_CONFIG_DIR`**：settings.json = 路由 env + 网关切片的精确虚拟 ID（`anthropic/claude-ccr-h<hex>[1m]`）+ 继承全局 apiKeyHelper / enabledPlugins，`projects/plugins/todos/…` 软链回共享目录（历史解析照常）。实测教训：**隔离 home 不能复用**——第二次会话静默失败（Connected → Error ~5.4s、adapter 无 `session/create`），故每次子会话新建 + 启动清扫 24h 旧目录；`agent_defaults` 兜底（无角色模型时仍走 0.3 配方通道）。另加 `team::pick_member`：同 CLI 多成员按 task 文本中的角色名（管理员 owner 约定）消歧，最早提及优先、无提及退回首个。真机：同会话两成员 甲(pro) / 乙(flash) 均启动成功（子行 `external_id` 落库），request-log 出现 `mimo-v2.6-pro` 与 `mimo-v2.6-flash` 各 200。测试：agent_host 11 + 相关 3102 用例绿。v1 边界：非 claude CLI（codex 等自有 provider，非 CCR）暂不注入。→ 做到：同一任务两成员两模型，request-log 为证。
- **3.1.1 角色大脑解析与降级** ✅ 2026-10-17（修 3.1 的现场缺陷，用户真机报错 `Couldn't confirm model "75f800be/deepseek-flash"`）：`agent_host.rs` 按职责拆为 `agent_host/{mod,gateway,role_home}.rs`（启动配方 / 网关模型目录 / 角色隔离 home），网关侧两处重复的 `/v1/models` 读取合并为一处。**缺陷**：隔离 home 在网关解析不到精确 ID 时**回落到裸 `provider/model`** 写进 `ANTHROPIC_MODEL`，而 claude 的 ACP adapter 对确认不了的模型直接中断会话（0.3 配方通道有"不在列表 → 只保留路由注入"的兜底，3.1 这条路径没有），于是「成员角色 + 旧大脑」的会话一起即死。**修复**：① 解析不到 → **不建角色 home**，会话回落 CLI 自身（CCR 托管）配置，只告警不中断；② 大脑形状放宽——provider 可填 id 或显示名（按 CCR `Providers[]` 映射后编码）、省略 provider 时按各 provider 显示名逐个尝试、仍无果则在**唯一** provider 下按模型名的 hex 后缀匹配（同名的多个 provider 不猜）。**真机核对**（在线网关 19 个 claude 可见 ID × 9 角色）：修复前 2/9 可解析，修复后 6/9（`75f800be/mimo-v2.6-flash` 这类旧 provider + 有效模型名救回；无 provider 的 `deepseek-v4.1-flash` 也能定位）；余 3 个安全降级——`后端工程师` 的 `deepseek-flash` 在网关**并不存在**（需用户改成在线模型，如 `deepseek-v4.1-flash`），`管理员` / `文档维护专家` 非 claude、本就不走该通道。测试：agent_host 15 用例（新增 4）、clippy 无新增告警。→ 做到：会话不再因模型确认失败而中断；花名册里的旧 provider 不再必然失效。
- **3.1.2 删除 claude 模型注入通道** ✅ 2026-10-17（用户拍板）：`AgentHostRecipe` 去掉 `config_options`，配方只剩路由 env；按角色定模型**只留 3.1 的隔离 home**，没有角色大脑的会话跑 CLI 自身（CCR 托管）配置。原因：0.3 那条经 `session/set_config_option` 切模型的通道对成员会话**从不成功**（claude 只接受当前配置已命名的模型），2026-10-17 日志里每条成员会话都刷一条 `failed to apply preferred config 'model'… Couldn't confirm`，只贡献错误噪音。`gateway.rs` 连带删掉只有它用的 `claude_model_option_id`，`profile_api_key` 收敛为 `local_gateway_api_key`（唯一剩下的调用方固定用 local gateway key）。测试：agent_host 15 用例绿；`--all-targets` 编译干净。→ 做到：一个机制、零错误刷屏。
- **3.1.3 花名册旧大脑归一化** ✅ 2026-10-17（用户拍板；**用户数据，经唯一写入方 API `agent_roles_save` 改，不手改文件**，改前 `sqlite3 .backup` 备份到 `codeg.db.bak-before-roster-normalize`）：6 个角色的 provider `75f800be` / 空串 → `OpenCode Go`（模型名本就在网关）；`后端工程师` 的 `deepseek-flash`（网关**不存在**该模型）→ `deepseek-v4.1-flash`；`管理员`（codex）同款死模型 → `mimo-v2.6-flash`（与同级 codex 角色 `Agent 助手` 及 codex profile 一致）。**未动**：`文档维护专家`（`agnes/agnes-2.5-flash` 在网关无对应目标，且非 claude、本就不走该通道）——留待用户定，不替他猜。真机核对（在线网关 19 个 claude 可见 ID）：6/6 claude 角色可解析（改前 2/9）。
- **3.3 按身体模型注入（claude + codex 基础层）** 🟡 2026-10-19（用户要求「Agents 页选的模型必须是 CLI 真实调用的」）：把 3.1 的 claude 专属做法抽成**一个通用机制 + 按身体分派**（新 `agent_host/overlay.rs`）。机制一句话：**给会话一个自己的 home**——里面放我们写的配置文件，其余条目符号链接回用户真实 home（不碰全局、历史仍落共享树、整份 overlay 可丢弃）。`OverlayOutcome` 三态：`Applied` / `Unsupported(原因)` / `Unavailable(原因)`，**后两者都意味着会话跑 CLI 自己的模型，并且必须告警**（不再静默）。落地：**claude** 复用 3.1 隔离 home（`CLAUDE_CONFIG_DIR`）；**codex 新增**（`CODEX_HOME` = 我们的 `config.toml`（`model` + `[model_providers.crewchat]`：网关 `base_url`、`wire_api="responses"`、`requires_openai_auth=false`、`env_key`）+ 整 home 符号链接镜像；密钥**只进进程环境**（`env_key`），不落盘；模型先按 CCR 配置里的 provider 模型清单校验（`codex_gateway_model`），不在清单 → `Unavailable` 降级）。**主会话也要注入**（此前 `acp_connect` 传 `role: None`，管理员/组长的花名册模型从来没生效过）：`acp_connect` 增 `roleId`（前端从会话 `team.leader_role_id` 或草稿团队取），两个入口（tauri 命令 + web handler）各一行调用 `apply_connect_role_overlay`。**真机实测机制**：隔离 `CODEX_HOME` + 网关 provider 跑 `codex exec` → CCR request-log `client=Codex provider=OpenCode Go model=mimo-v2.6-flash 200`（改前 codex 直连 `apihub.agnes-ai.com`，**完全绕过 CCR**）。测试：`agent_host` 25 例（新增编解码/镜像/分派/校验）+ `acp-connections-context` 173 例绿。**未做**：Agents 页按身体面给候选（现在仍可存"该身体不可达"的组合）、其它身体（open_code / gemini / hermes / pi）的 overlay、以及"实例化后 UI 标出实际模型来源"。→ 做到：claude 与 codex 的会话跑的就是花名册选的模型，request-log 为证。
- **3.4 引擎可选模型的权威答案 + 花名册页说真话** 🟡 2026-10-19：3.3 之后，"这台引擎到底能绑哪些模型"必须只有一个权威答案，否则界面会继续承诺做不到的事（这是用户最初那个问题的另一半）。**后端**：新 `agent_host/faces.rs` 把两个面拉直——claude 面＝网关对 claude 客户端**广告**的 id（把 `h<hex>` 解码回 `Provider/model`，即花名册写的形式；CCR 已无该 provider 但仍服务时 provider_id 留空），codex 面＝responses 面能用的 provider 模型名（`models` + `autoFetchKnownModels` 两个清单）；没有 overlay 的身体返回 `supported=false` + 原因，**不再给一个看起来可选的空列表**。"哪些身体有 overlay"提成单一真相 `overlay_support()`，启动器与界面共用（加身体时两处必须同改）。新命令 `agent_role_model_face`（tauri + web 双模式薄壳）。**前端**：编辑器新增 `modelFace`——不支持的身体把提供商/模型两个下拉**锁住**并写原因；存了但不在面上的模型给一行琥珀提示"会话会改用 CLI 自己的配置"，**不动已存的值**；纯函数 `faceIncludesModel`（provider 填 id 或显示名都算匹配、provider 为空按模型匹配、用 `supported` 区分"不知道"与"没有"）。**真机实测**（3080）：`codex` → `supported:true` + OpenCode Go 清单；`claude_code` → 同上（从网关在线 id 解出）；`gemini` → `supported:false` + `"Gemini has no per-role model overlay yet"`。测试：agent_host 29 例（faces 新增 4）+ helpers 21 例（新增 8）+ i18n 键位 3 例绿、tsc / prettier / eslint 零告警。**未做**：花名册**列表行**上的"该模型不可达"标记（现在要打开角色才看得到）、其它身体的 overlay、运行期"实际模型来源"标注。→ 做到：界面列出的模型就是该引擎真能跑的。
- **3.2 按成员用量** ✅ 2026-10-12：委派卡在子回合**终态**时按次拉取子会话解析统计（`get_folder_conversation` 的 `session_stats`），显示「输入 / 输出 tokens · 时长」；与「会话详情」同一数据源与同一文案（`Folder.statusBar.tokens` 的 input/output、`formatTokenCount`），口径一致；按子会话缓存、失败静默、进行中不请求（卡片仍是状态 + 导航 + 一行用量）。真机数据路径：`get_folder_conversation(75)` → `total_usage {input 19698, output 662}`、`total_tokens 24456`、`duration 15.6s`（甲成员的子会话）。测试：hook 5 + 卡片用例；message 目录 628 用例绿、tsc / eslint 零告警；前端已重建（`out/`）。v1 边界：成本未显示（codeg 侧只记 tokens / 时长；成本在 CCR 请求日志，另开）。→ 做到：口径与用量文档一致。

### Phase 3.5 · open_code 的 overlay（机制已实测，实现待做）

- **3.5 open_code 按角色定模型** ⏸ 2026-10-19（机制已验证；实现因编辑事故整批撤回，未提交）。**为什么待做**：`文档维护专家` 是 open_code 身体，3.4 已让界面如实说"暂不支持"，但模型仍不生效。
- **已实测的配方**（照抄即可，别再考古）：
  - opencode 的配置是**分层**的——`$OPENCODE_CONFIG` 在用户全局配置**之后**合并（顺序见 `src-tauri/src/parsers/opencode_context_window.rs` 的 `config_files`），所以**不需要镜像 home**（比 codex 省一半），只需一个覆盖文件。
  - 覆盖文件内容：`model = "crewchat/<模型名>"`；`provider.crewchat = { npm: "@ai-sdk/openai-compatible", name: "CCR (crewchat)", options: { baseURL: "<网关>/v1", apiKey: "{env:CODEG_CCR_GATEWAY_KEY}" }, models: { "<模型名>": { name } } }`。
  - 环境：`OPENCODE_CONFIG=<覆盖文件>` + `CODEG_CCR_GATEWAY_KEY=<CCR 那把 profile key>`（`{env:…}` 由 opencode 在加载时解析 → **密钥不落盘**，比 codex 更干净）。
  - 模型校验复用 `provider_gateway_model`（原 `codex_gateway_model`，非 claude 面通用）；把 `OpenCode` 加进 `overlay_support`、启动分派、以及 `faces.rs` 的 `AgentType::Codex | AgentType::OpenCode` 分支。
  - **真机证据（2026-10-19）**：`opencode run "reply with exactly OK"` 在隔离配置下 → CCR 日志 `client=opencode provider=OpenCode Go model=mimo-v2.6-flash 200`（两次，其中一次配置里写的是 `{env:…}`）。
- **注意**：`文档维护专家` 当前大脑是 `agnes/agnes-2.5-flash`，而 CCR 里**没有** `agnes` provider → 即便实现完成，该角色也会如实降级（`Unavailable` + 会话跑 CLI 自己的配置）直到你改掉它。

### Phase 3.5 · open_code overlay（机制已实测，实现待做）

- **3.5 open_code 按角色定模型** ⏸ 2026-10-19：**机制已验证、实现整批撤回**（我在编辑中把函数插重复、编译失败，按纪律撤回未提交改动，不让纠缠代码进历史）。`文档维护专家` 是 open_code 身体，3.4 之后界面已如实说"暂不支持"，但模型仍不生效。
- **已验证的配方（照抄，别再考古）**：
  - opencode 的配置是**分层**的：`$OPENCODE_CONFIG` 在用户全局配置**之后**合并（顺序见 `parsers/opencode_context_window.rs` 的 `config_files`）→ **不需要镜像 home**（比 codex 省一半），只需一个覆盖文件。
  - 覆盖文件：`model = "crewchat/<模型名>"` + `provider.crewchat = { npm: "@ai-sdk/openai-compatible", name: "CCR (crewchat)", options: { baseURL: "<网关>/v1", apiKey: "{env:CODEG_CCR_GATEWAY_KEY}" }, models: { "<模型名>": { name } } }`。
  - 环境：`OPENCODE_CONFIG=<覆盖文件>` + `CODEG_CCR_GATEWAY_KEY=<CCR profile key>`；`{env:…}` 由 opencode 加载时解析 → **密钥不落盘**（比 codex 的 `env_key` 更干净）。
  - 复用 `gateway::codex_gateway_model`（非 claude 面通用，已按此考虑改名为 `provider_gateway_model`）+ 把 `OpenCode` 加进 `overlay_support` 与启动分派 + `faces.rs` 的 Codex 分支。
  - **真机证据**：`opencode run "reply with exactly OK"` 在隔离配置下 → CCR 日志 `client=opencode provider=OpenCode Go model=mimo-v2.6-flash 200`（两次，其中一次配置里写的是 `{env:…}`）。
- **注意**：`文档维护专家` 当前大脑是 `agnes/agnes-2.5-flash`，而 CCR 里没有 `agnes` provider → 实现完成后该角色也会如实降级（`Unavailable` + 跑 CLI 自己配置），除非先改掉它的模型。

### Phase 4 · 体验增强

- **4.1 卡内内联展开成员流** 🟡 2026-10-17（复用 `LiveTranscriptView`；代码完成、测试绿，真机验收待用户）：委派卡新增内联流开关——`trailingAction` 槽位（标签块与「查看会话」之间）展开固定高度（`h-80`）的子会话实时流，默认收起（不进内联流的用户看到的仍是单行卡）；**内联与抽屉互斥**——`SessionViewerHostValue` 增可选 `close?()`，任一方式打开即让另一方退出（两者挂同一个实时桥，同一子会话两个桥会互相清空）。开关仅在 `childConversationId` 已知时出现。测试：新增 `delegated-sub-thread.inline.test.tsx` 8 例（默认单行 / 展开指向子会话与子连接 / 折叠 / 无子会话无开关 / 双向互斥 / 只挂一个桥 / 卡自身行不变），既有卡测试补 mock 与断言；prettier / eslint / tsc 零告警；全量 `vitest` 8942 例绿（含同批修掉的两处拆分裂缝，见下）。**同时收尾两处历史红**：① `hermes-providers.test.ts` / `tab-view-order.test.tsx` 的源码路径断言改指拆分后落点（`commands/acp/mod.rs`、`conversation-detail-panel/index.tsx`）；② `conversation-detail-header{,-tags}.test.tsx` 的 `app-workspace-store` fake 补 `subscribe`（1.4 的 `TeamMemberStrip` 把 `tab-store` 的模块级 `subscribe` 带进头部导入图，只带 `getState` 的 fake 令整个 suite 在收集期崩）。→ 做到：可选展开，不影响现有卡与抽屉。
- **4.1.1 委派卡显示成员名** ✅ 2026-10-17（用户真机反馈「卡上是 Claude Code，不是成员名字」）：卡的**主标签**改为花名册成员名，CLI 名降为次要标记——「谁做的」是读者第一需要，引擎已由头像与次要标记承载。**成员身份走委派 meta**（刻意不动 `AcpEvent::DelegationStarted` / `session_state` / `pet_state_mapper` / `engine.rs`）：`build_delegation_meta` 增 `member_role_id`，写点四处（running / terminal / cancel / resume），刷新路径由 `build_historical_delegation_meta(child)` 从**子行自己的团队快照**补（所以重建后的卡也带名字）；前端 `parseDelegationMeta` 读出 → `useMemberRoleName`（名册模块级缓存 `listAgentRolesCached`——每张卡不重复取，改名/删角色即时生效，查不到回落 CLI 名）→ `DelegationCardRow.memberName`。**顺带修一个真 bug**：`DelegationLink.member_role_id` 原来用「按 agent_type 首个匹配」查找，与 `pick_member` 的消歧结果不一致（同一 CLI 有多个成员时链接会指错人），现统一取 `pick_member` 解析出的成员。测试：delegation 316 用例绿；新增 parse 2 + 行渲染 2 用例；tsc 零告警。**按职责外移**：历史 meta 构造器移入 `acp/delegation/meta_writer.rs`（`historical_delegation_meta`，与 `build_delegation_meta` 并列——一个形状一个归属，两者必须同形状），`commands/conversations/mod.rs` 2.8k → 2.76k 只留走 turn 的注入胶水，新函数在原地补 2 个直接用例。→ 做到：卡上是成员名，刷新不丢。
- **4.4 Agent 优先（会话起点）** ✅ 2026-10-18（提交 `3c199f8`，Goal 第 2 条）：草稿起点改为 **Agent 优先**——`TeamSelector` 升为第一入口（按钮直接显示所选 Agent 名，有成员时附「+N」；空 = 单 Agent 会话），CLI（引擎）选择器在 Agent 带 `agent_type` 时**跟随 Agent 并置为只读**（锁定提示 `engineFromAgentHint`）；Agent 无 `agent_type` 或名册不可达时引擎仍是用户自己的选择。`listAgentRolesCached` 让选择器与会话外壳共用同一个名册 promise，提前加载不产生额外请求。测试：`team-selector.test.tsx` 6 例 + i18n 键位对齐 3 例绿、`tsc` 绿（`scripts/verify.mjs changed`）。→ 做到：选人的入口是 Agent，引擎是它的读数。
- **4.5 团队与委派开关的一致性提示** ✅ 2026-10-19（用户真机报"选了管理员+前端 agent，但组长读不到另一个 agent"）：**根因不是模型，是开关**——团队帧（1.3）只在**委派开启**时注入（关掉委派连 `delegate_to_agent` 工具都不注入，帧就成了撒谎），而花名册页照样让你选成员、成员条照样显示"待命"，于是组长只能回答"我看不到本会话之外的其它 Agent"。修法（界面诚实那条线）：新增 `useDelegationEnabled`（进程内记忆化 + 窗口聚焦时刷新，失败的读不缓存），成员条在 `delegationEnabled === false` 时显示一行琥珀提示「「多智能体协同」未开启——组长看不到也无法委派成员」+「去开启 →」（跳 `/settings/collaboration`）。测试：成员条 9 例（新增 2：关闭时报警、开启/未知时不说）+ i18n 键位 3 例绿；tsc / prettier / eslint 零告警。**待做（同一问题的另一半）**：选团队时就该提示/一键开启（现在只在会话头提示），以及 app 自己的库默认没有 `delegation.*` 行——新库首启要不要默认开委派，需要产品决定。
- **4.2 @提及与团队联动** ✅ 2026-10-18（提交 `510d45a`）：`@` 面板新增**团队分组**——仅在会话带团队时出现，成员按花名册名字列出（与委派卡 / 团队帧一致），点选插入的仍是 `codeg://agent/<body>`，序列化 / 路由 / 后端门禁全部沿用现有链路（不新增第六种引用类型：`SuggestionGroupKind` 是**展示用**的宽化）。该分组**置顶**（`TAB_ORDER` 首位）且为面板首选非空页签；无团队会话页签仍是原来的四个（`LEGACY_TAB_ORDER`，非团队会话行为逐字不变）。智能体页签每行加提示：成员「团队成员」、其余「不在本次团队」（=「派给它会被拒」的事前提示）；提示在查询过滤**之后**追加，故提示词本身不会被搜索命中。成员 id 来源：持久化会话取会话行 `team`（workspace store，与成员条同源），草稿取 `TeamSelector` 选中的 `draftTeam`；经共享名册缓存 `listAgentRolesCached` 解析（不额外发请求），名册不可达 → 不显示团队分组。测试：新增 15 例（纯构造 8 + hook 解析 4 + 弹层页签 1 + 既有断言回归 2）；`chat-input` / `conversation-shell` / `rich-composer` / `message-input` / `use-reference-search` / `suggestion-popup` / i18n 键位 **212 例绿**、`tsc` 零告警；`components/conversations` **383 例绿**；prettier / eslint 零告警。→ 做到：@ 候选与团队一致。

- **4.3 异步回流**：长任务完成有明确提示与「让管理员汇总」入口。→ 做到：后台任务不丢、有回来路。

### Phase 4.6 · 权限归属到每个 Agent（5/5 已落地，待真机验收）

**用户决定**：权限不再由对话决定——不在 composer 暴露会话模式、权限请求也不再打断对话；**每个 Agent 在 Agents 页一次配好**。

- **4.6.1 花名册字段** ✅ 2026-10-19（`e53a300`）：`custom_agent` 增 `permission_mode`（角色会话的**起始**模式 id＝CLI 审批/沙箱 preset；空＝保持 CLI/CCR 默认＝今天的行为）与 `auto_approve`（该角色的权限请求自动批准；**默认关**）。只读这两列的是 `kind='role'` 行；`kind='acp'` 行逐字不变。迁移 `m20261019_000001`。测试：agent_role 服务 8 例绿。
- **4.6.2 会话启动时套用 + 4.6.3 权限请求自动应答** ✅ 2026-10-19（`0e31a36`）：`apply_connect_role_overlay` 现在顺带返回 `RoleSessionPolicy`；主会话（tauri 命令 + web handler）用角色的 `permission_mode` **覆盖** composer 的偏好（Agents 页才是配它的地方）。委派成员**按 `role_id` 直接查一次花名册行**取同一份事实——刻意**不往 `RoleSpawnContext` / `TeamMember` 加字段**：权限事实只留在 Agent 一处。自动批准拦在 `admit_permission`（「请求本会变成卡片」的唯一入口）：`SessionState.role_auto_approve` 为真就选 `allow` / `allowForSession`（否则第一个 option）直接应答、**不发布卡片**——所以不会留下没人能答的 parked responder，其它会话的队列/撤回机制完全不受影响。标记在 spawn 之后、**第一条 prompt 之前**落到连接（`set_role_auto_approve`），这正是它唯一有意义的窗口。`PermissionReply::respond_selected` 上提一层：队列与自动批准两条路共用同一份编码（否则 codex 的 elicitation 批准会发错形状）。
- **4.6.4 composer 不再暴露权限选择** ✅ 2026-10-19（`07b0059`）：角色绑定的会话（tab 视图按 `composerLeaderRoleId` 给出 `permissionsManagedByAgent`，经 `ConversationShell` → `ChatInput` → `MessageInput` 透传）**不渲染模式选择器**（`showModeSelector` 的末位条件）；非角色会话逐字不变。顺带修 `f7eedbb` 的回归：`connect()` 新增的第 5 参 `roleId` 漏同步到 `tab-first-show-auto-connect.test.tsx`（已 stash 到 HEAD 复现确认与本次改动无关）。**未做**：权限类 config option 的隐藏（现在只收模式选择器）。
- **4.6.5 编辑器 UI** ✅ 2026-10-19（`c1f1546`）：`role-editor.tsx` 新增「权限」区——**审批预设**下拉（候选＝**该身体自己广告的** modes，`useAgentOptions` 活体探针，与委派默认值页同源；首项「跟随 CLI 自己的默认」＝保存回 `""`，因 Radix 把空串留给「未选择」故用哨兵值）＋「不再询问——自动批准」开关（绑定 `auto_approve`，**默认关**，文案写明代价与「下次会话启动时生效」）。测试：helpers 26 例（新增 `permissionModeOptions`）、message-input 92 例（新增「Agent 管权限时不给模式选择器」）、team-selector 6 + 成员条 9 + i18n 键位 3 全绿；`tsc` / `prettier` / `eslint` 零告警。
- **待真机验收**：Agents 页给某角色设 `auto_approve` → 开一轮会话，看 `~/.claude-code-router/app-data/request-logs.sqlite` 与 `codeg-server` 的 `[agent-host]` 行，确认权限请求不再中断、模型仍是所选。**未验证**：真机端到端（后端只有 `cargo check` + 单测，运行期未实测）。

### Phase 5 · 平台收尾

- **5.1 CCR 托管** ✅ 2026-10-17（真机复测通过；待用户过目状态卡）：**探活 / 接管 / 拉起 / 守护 / 状态卡**——用户不再手动起 CCR，崩溃自动恢复。**刻意不重造进程管理**：CCR 自带 `ccr start`（daemon 化、写 `~/.claude-code-router/service.json`、幂等复用已验证实例）。Codeg 只做四件事：① **探活**——对管理地址做 **TCP connect**（故意不走 RPC：可达性才是「接管 or 拉起」的判据，且不依赖 token 正确，token 不对也不会误启第二个实例抢端口）；② **接管**——可达即采纳，不问是谁起的，也**不停**用户的实例；③ **拉起**——仅当不可达，且 **回环地址 + `CCR_WEB_AUTH_TOKEN` 已固定 + 能解析出 `ccr` CLI**（`CCR_BIN` → `PATH`）三者同时满足，否则只报 `blocked_reason`（与 `last_error` 分开）；④ **守护**——20→40→80→160→300s 退避封顶，**只起不停**（停是用户决定），总开关 `CODEG_CCR_SUPERVISE=0`。结构：新模块 `src-tauri/src/ccr/`（`client.rs` 收拢管理面传输——URL/token 契约与 RPC 转发从 `commands/ccr.rs` 移出，命令层只剩适配；`supervisor.rs` 托管，判据与退避是纯函数，13 用例）；`AppState.ccr_supervisor` 两模式同一实例，桌面 `lib.rs` setup 与 `bin/codeg_server.rs` 各起一次守护（`guard_loop` 由调用方选运行时：`setup` 无 tokio reactor，用 `tauri::async_runtime::spawn`）；路由 `POST /api/ccr_supervisor_state` / `/api/ccr_supervisor_start` + 同名 Tauri 命令；模型页顶部新增「CCR 服务」卡（状态 / 由 Codeg 启动·Codeg 之外启动 / pid / CLI / 无法自启原因 / 上次错误 / 启动·重试·重新探活），与既有 `GatewayCard` 相邻：**卡管宿主，网关卡管代理**；10 语言文案 + 5 个组件用例。**真机复测**：前台实例在跑时 `state=running, managed=false` 且不启第二个实例（3458 仅一个监听者）；杀掉 CCR（7733+7878）后 5s 探到 `stopped`、15s 变 `running, managed=true, pid=67852`，此后 45s 稳定；`service.json` 的 `ccr_web_token` 仍是固定的那个，经薄代理 `getConfig` 返回 **200** 并读出供应商 `OpenCode Go`。**未验证**：桌面端只做编译验证（运行期未实测）；经网关的模型调用需 CLI 自身凭证，未跑。规则同步：`AGENTS/gateway.md`（托管契约 + 三个环境变量）、`codeg/NOTICE.md`。
- **5.1.1 验证脚本按改动拆** ✅ 2026-10-17（用户要求「每次只测改动的」）：`codeg/scripts/verify.mjs`（`pnpm verify`）——`changed` 从 `git diff HEAD`（含未跟踪文件）派生范围并**先打印将执行的命令**（即验证证据）：改哪个 Rust 文件就按该文件自己的 `#[cfg(test)] mod <name>` 取**精确过滤器**（如 `ccr::supervisor::tests`；无内联测试的业务文件算接线，只编译），`src/bin/**` 补 `cargo check --bins`，`tests/` 补 `--tests`；前端跑**同名测试文件**（无则只 `tsc`）；locale JSON 跑键位对齐；Python 跑 `import` 冒烟。`--dry-run` 只打印计划。**诚实边界**：`cargo test --lib` 仍编译整个 crate（一 crate 一测试二进制），省的是「跑什么」与「一次构建而不是 check+test 两次」；全量（`cargo test` / `pnpm test`）留给里程碑收口。规则落 `AGENTS/verification.md` 与 `codeg/AGENTS.md`。
- **5.2 引擎状态页 / 用量页**收口。
- **5.3 CLI 覆盖补齐**：gemini / hermes 连接器；opencode / pi profile 启用。
- **5.4 打包**：dmg + 首次运行向导（供应商预设 → key → profiles）。

- **5.1.2 CCR 端点自动发现（双击可用）** 🟡 2026-10-19（用户真机反馈：桌面版显示「未配置访问 token」）：根因是 URL/token **只认环境变量**，而双击启动的桌面 app 没有 env → 模型页与按角色模型注入一起降级。修法：`ccr/client.rs` 改为 **env → CCR 自己的 `<ccr home>/service.json` → 默认回环**（`ccr start` 写的那份文件里，`url` 带 `?ccr_web_token=…`；这也是 5.1 托管已经在用的同一份文件）。**刻意不把 `serviceToken` 当 web token**（那是服务身份，用错就 401）——为此单列一条用例。`health()` 增 `tokenSource`（`env` / `service-file` / 无），模型页把来源写出来（「已配置（已从 CCR 的 service.json 自动发现）」）。真机：**不带任何 `CCR_*` 变量**重启后端 → `/api/ccr_health` 报 `tokenConfigured: true` + `tokenSource: "service-file"`，`agent_role_model_face`（需要读 CCR 配置）正常返回。测试：`ccr::client` 6 例（新增 3，含"不误用 serviceToken"与坏 JSON 容错）。**残留**：`CCR_BIN`（拉起 CCR 用）仍只认 env 或 PATH——双击场景下若 CCR 没在跑，Codeg 只能报"无法自启"；补发现（如 `<ccr home>/bin/ccr`）待做。

### Phase 6 · 收口

- **6.1 文档对齐**：README、`AGENTS/agent-model.md`、`bodies.md`、`gateway.md`、计划索引；Python 编排定位改写（参考 / 冻结）；本计划完成后移入 `archive/`。
- **6.2 Python 侧处置**：冻结标注（不删），未来用途另开计划。
- **6.3 持续**：四档法（含结构拆分）+ NOTICE 登记随每步走。

## Non-Goals（不做清单）

- 不桥接 Python 编排、不做前端直连 Python 的协作栈（已改判归档）。
- 不重写 Codeg 聊天内核；不改上游核心**逻辑**（四档法：结构拆分见 `AGENTS/upstream.md`）。
- v1 不做扁平多气泡时间线（管理员叙事为准；4.1 为增强）。
- 不在不可回退的状态下换代：每步可独立验收、可回退。

## 执行约束（给执行 Agent）

- 开工先按 `AGENTS/` 索引读对应规则（`ui.md` / `api-contract.md` / `agent-model.md` / `truth-sources.md` / `bodies.md` / `verification.md` / `upstream.md`）。
- **拆分纪律（重要）**：触达大文件时**先把新逻辑做成独立模块 / 新文件，再接线**，不往大文件里持续堆内容；**并主动把既有上游大文件拆开**（结构拆分，见 `AGENTS/upstream.md` 四档法与 `codeg/NOTICE.md` 的拆分表——2026-10-12 用户拍板，源项目源码同样拆）。已明确的巨型文件：`settings/acp-agent-settings.tsx`、`acp/connection.rs`、`acp/delegation/broker.rs`、`conversations/conversation-detail-panel.tsx`（Rust 侧第一轮测试外移已登记进 NOTICE）。主文件只留小集成点（`[crewchat]` 标记范围尽量小）。可参考的已落地做法：就绪卡拆成 `agent-readiness-card.tsx` + `use-agent-ccr-binding.ts`，主文件只留挂载。
- 每步独立小提交：一步一个主题，失败可单独回退；不把多步混进一个提交（见 `AGENTS/git.md`：分批提交、不推送）。
- 验证顺序：**按改动派生最小验证**——`cd codeg && node scripts/verify.mjs changed`（改哪个模块只跑哪个模块：Rust 按 `#[cfg(test)] mod` 精确过滤、前端按同名测试文件、locale 跑键位对齐、`src/bin/**` 补 `--bins` 编译），`--dry-run` 先看计划；**不每步跑全量**，全量（`pnpm test` / `cargo test --features test-utils`）只在里程碑收口时跑一次（用户 2026-10-17 拍板）。prettier / eslint / tsc 同理只在触达的前端范围先跑。
- server 模式：3080 起法（`CCR_WEB_URL/TOKEN`、`CODEG_STATIC_DIR/DATA_DIR/HOST/PORT`，5.1 起另加 `CCR_BIN` 指向 `ccr` CLI）；前端改动重建 `codeg/out`。
- **开发回路**：日常用桌面 `pnpm tauri dev`（前端 HMR、Rust 自动增量重启；跑 `codeg-dev.db`），server 模式只用于验收（真库 / 3080 浏览器）；**别来回切 feature 集**（会全量重编）。打包与验证的完整约定见 `codeg/NOTICE.md`「开发回路与打包」。
- 上游改动走四档法（含结构拆分）+ NOTICE 登记；计划与文档随步同步。
- 每步收口门槛：全量测试绿 + 场景真机证据（截图 / request-log）+ 文档同步 + 工作区干净。

### 关键地标（省去考古）

| 面 | 落点 |
| --- | --- |
| 委派内核 | `codeg/src-tauri/src/acp/delegation/{broker,spawner,types}.rs`（1.3 约束落点：broker 前置检查） |
| 提示帧注入 | `codeg/src-tauri/src/acp/agent_mentions.rs`（1.3 团队帧落点） |
| 会话表 | `codeg/src-tauri/src/models/conversation.rs` + `db/entities/conversation.rs`（1.1 落点） |
| 自定义 agent | `codeg/src-tauri/src/acp/custom_registry.rs`（2.1 落点） |
| 委派卡 / 抽屉 | `codeg/src/components/message/{delegated-sub-thread,sub-agent-session-dialog}.tsx` |
| 花名册页 / 就绪卡 | `codeg/src/components/settings/agent-roles/`、`agent-readiness-card.tsx` |
| codeg-mcp 解析 | `codeg/src-tauri/src/acp/connection.rs`（`CODEG_MCP_BIN`） |
| CCR 层 | `codeg/src/lib/ccr.ts` + `codeg/src-tauri/src/ccr/`（`client` 传输 / `supervisor` 托管）+ `commands/ccr.rs`（命令适配） |

## 风险与注意

- 管理员必须显式配大脑（沿用既有产品决定，不做隐式回退）。
- 越界委派必须清晰报错（体验关键，1.3 验收）。
- 防双花名册回潮：2.1 完成后只保留一个可写入口。
- 异步回流 v1 从简（拍板 #5），避免自动续跑引入难控循环。

## Done Criteria

- Phase 1：一次真实任务由 2+ 成员完成并汇总；取消 / 恢复可用。
- Phase 2：花名册单真相源；角色人格与技能对成员生效。
- Phase 3：同一任务多成员多模型，request-log 佐证。
- Phase 5：CCR 托管、打包可用。
- 全程：全量测试绿；文档（README / AGENTS / 本文件）与实现一致。
