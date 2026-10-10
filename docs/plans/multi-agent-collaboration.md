# 多 Agent 协作核心 执行计划

Updated: 2026-10-10 (Asia/Shanghai)
Status: In Progress（阶段 1 已接入 `api/chat.py` → `orchestration/group.py` 的 `build_prompt`；阶段 2、3 未做）

## Goal

让**一个会话里的多个 Agent 真正协作**：后来者能看到、接续先行者的产出，共同完成用户下达的任务；
每个 Agent 保留自己的模型、身体、Skill 与 MCP，全程可见可取消。

这是产品的核心命题。当前实现**没有协作**，只有串行（见 Current Facts 第 1 条）。

## Current Facts

均为本仓库或本机实测/读码所得，非推测。

1. **现状是串行同题，不是协作**。`src/crewchat/api/chat.py:88` 的 `_prompt_for` 给每个 Agent 塞
   **同一段话**，只在前面拼 persona：

   ```python
   head = persona
   if group:
       others = "、".join(s.display for s in specs if s.id != spec.id)
       head += f"\n\n在场 agent（你之外）：{others}\n给出你的判断与产出…"
   prompts.append(f"{head}\n\n---\n\n{content}")
   ```

   Agent 之间**互相看不到产出**，只是依次各答一遍。`_run_turn` 逐个跑、每个 agent 一个 `msg_id`、
   独立 usage —— 事件与身份是对的，**缺的是上下文流动**。

2. **crewai 驱动不了 CLI**。实测 `crewai.LLM(model='claude')` 不报错，但**静默落到 OpenAI** 并返回
   401（`Incorrect API key provided: x`）。`Agent.llm` 的类型是 `str | BaseLLM | None`——只有传自定义
   `BaseLLM` 实例才可能接 CLI，那不是"配一个模型 id"能解决的。且 crewai 自带执行循环，会与 CLI 自身
   的工具循环叠加。

3. **crewai 的 conversational flow 一次只路由到一个响应者**。`route_turn()` 把一次用户消息分派给
   单个 route handler；与"多个 Agent 各出一份产出"的诉求不匹配。

4. **CAO 只有终端级状态**。`models/terminal.py:14` 的 `TerminalStatus` 是
   `unknown/idle/processing/completed/waiting_user_answer/error`，靠 tmux pane 字符缓冲 + 提示符
   （`❯`）模式匹配得出。拿不到 delta / 工具入参出参 / token / 成本。它自带 `cao-server` + DB +
   Web UI（第二真相源），且强依赖 tmux 3.3+（Windows 需 WSL）。

5. **我们的执行器已给到模型级分辨率**。`executors/` 经 SDK 直接产出 `ExecEvent(delta/tool/error/done)`，
   带 `args` / `result` / usage / cost / duration；`claude_body` 与 `codex_body` 已修复会话恢复与
   事件去重。**这是分辨率来源，不能丢。**

6. **共享工作区已具备**。同一会话的 Agent 以同一 `spec.resolve_cwd(workspace)` 为 cwd
   （`orchestration/session.py`），文件级协作天然可行。

7. **参照实现的收敛**：上游 AionUi（我们 fork 的来源）Team Mode 与 AWS CAO **独立收敛到同一形状** ——
   supervisor + MCP 派活（`handoff` / `assign` / `send_message`）+ worker 隔离 + 共享 workspace +
   inbox 回收。两家大厂各写一遍落到同一形状，说明**形状是对的**。

8. **规则与现状冲突（需修正）**：`AGENTS/architecture.md` 现写「群聊用 `crewai.flow.conversational`
   （router 选发言者）」——已被第 2/3 条证伪。

## Design

核心 = 一层**薄协调层**，架在 `executors/` 之上。四个部件，都不引入第三方 runtime。

```
            ┌── 用户消息 ──┐
            ▼              │
   ┌─────────────────────────────────────────┐
   │  协作循环（后端，确定性、可观测、可取消）    │
   │                                           │
   │   ① 共享上下文  谁做了什么、产出了什么       │
   │   ② 路由        下一步谁干                  │
   │   ③ 委派        agent 主动把活交给别人       │
   │                                           │
   │   for step in loop:                       │
   │       选中 agent                          │
   │       组 prompt = 共享上下文 + persona + 任务│
   │       executor 跑（流式出帧 → 气泡）         │
   │       产出入共享上下文                      │
   │   until done                              │
   └─────────────────────────────────────────┘
```

**关键取舍：循环由我们写，Agent 在自己那一轮里自治。** 这正是 CAO 的反面 —— CAO 让 Agent 经 MCP
工具驱动循环，所以必须抓终端、所以不透明。我们把委派做成循环里**一个可观测的步骤**，于是随时能显示
「谁在说」、能取消、能持久化。

### ① 共享上下文

- **文件产物打底**：Agent 共享 cwd，靠读写文件交接（coding agent 最自然的信息通道，且已具备）。
- **工作记录做骨架**：结构化记录「任务 → 负责人 → 产出状态」，按需注入，防止上下文爆炸。
- 原始对话全文只作兜底，不作为主通道。

### ② 路由

分三档，从确定到智能，**决定权始终在循环**（Agent 只能建议）：

| 档 | 触发 | 说明 |
| --- | --- | --- |
| 规则 | `@agent` 指定 | 用户直接点名，最可控 |
| 顺序 | 在场顺序 | 缺省：按 `agent_ids` |
| 建议 | LLM 判下一步 | 循环把「谁该动」作为一次独立小调用，结果可被覆盖 |

### ③ 委派

注入一个 `handoff(to, task)` MCP 工具（语义照抄 CAO / AionUi）：Agent 调用它即**请求**循环把活交给
另一个 Agent。循环收到后决定是否执行 —— 委派是循环的一步，不是 Agent 自己开进程。

### 归属（对照 `AGENTS/architecture.md` 的 L0–L4）

- 循环、共享上下文、路由、委派 → **L3 `orchestration/`**，新增 `group.py`（架构文档已预留）。
- 执行仍走现有 **L2 `executors/`**，不改执行器契约。
- 对外帧协议不变（`message.stream` 每 Agent 一个 `msg_id`），**L4 `api/chat.py` 只切换调用目标**。
- `Task` schema（如需）→ **L0 `core/models.py`**，只借 crewai 的 Pydantic 模型。

## Stages

每阶段独立可验证，做完一个再评估下一个。

### Stage 1 — 共享上下文（流水线）

把 `_prompt_for` 的「同一段话」改成「流水线」：Agent N 的 prompt 带上前面 Agent 的产出。

- 改动面：`api/chat.py` 的 `_prompt_for` + `_run_turn` 累积已完成文本；或新增 `orchestration/group.py`
  承载。
- **不需要任何新框架**，是当前形态的最小改造。
- 验证：两个真实 CLI（如 `opencode` + `codex`），甲产出交给乙审，乙的回复能引用甲的具体内容。

### Stage 2 — 显式委派

注入 `handoff(to, task)` MCP 工具；循环识别并路由。此阶段之后才算「真正的多 Agent」。

- 改造 executor 起进程时注入该 MCP server（各身体方式不同，见 `AGENTS/bodies.md`）。
- 循环需要一个「待办 / 队列」以承接委派。
- 验证：甲主动把子任务交给乙，UI 上能看到「甲 → 委派乙」这一步，且乙确实执行。

### Stage 3 — 规划者

一个 leader Agent 把任务拆成带负责人的子任务；循环按计划执行。

- 此处引入 `Task{description, expected_output, context}` schema（**只借 crewai 数据模型，不用其引擎**）。
- 验证：复杂任务下 leader 的分解合理，子任务按依赖顺序执行，产出可回收汇总。

## Verification

每阶段的共同底线：

```bash
uv run pytest -q                         # 后端测试
uv run python scripts/check_layers.py    # 不得出现向上依赖
uv run python scripts/check_imports.py
```

- **协作语义**必须真机验证（起真实 CLI，看产出是否被接续），不能只靠单测。
- 事件顺序：每个 Agent 一个 `msg_id`、`turn_complete` 只在最后一个 Agent 为真、usage 按 Agent 独立。
- 本机 claude CLI 当前配置有误（`~/.claude/settings.json` 指向本机代理且模型 id 非法），
  真机验证优先用 `opencode` / `codex`。

## Progress

- [x] Stage 1 共享上下文（`group.build_prompt`，`chat.py` 群聊回合已调用）
- [ ] Stage 2 显式委派
- [ ] Stage 3 规划者

## 待修正

- `AGENTS/architecture.md` 的「crewai 的定位」一节 与本次实测冲突，需随本计划首批改动一起更正
  （改为：只借数据模型，执行循环自建）。
