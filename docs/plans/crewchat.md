# CrewChat 执行计划

Updated: 2026-10-10 (Asia/Shanghai)
Status: In Progress（Slice 1 已跑通）

往后只执行这一份。被取代的计划在 [archive/](archive/)，其中的 Current Facts 和未完成切片不再当任务做。

## Goal

一个会话里的多个 Agent 协作完成用户的任务。每个 Agent 保留自己的模型、身体、技能和 MCP。模型请求经本机网关，真实 key 不进子进程，也不改用户的全局 CLI 配置。

## Current Facts

- 网关是 `gateway/cc-switch/` 里的 headless `ccsw-proxy`，数据目录 `.data/gateway`。LiteLLM 已删除。用量写 `.data/crewchat.db` 的 `usage_events`，不读 cc-switch 的库。技能和 MCP 读 `~/.agents/`（`AGENTS/truth-sources.md`）。
- 模型绑定已落地。每个 ACP 进程自带一份配置、这次的发布 id 和 `PROXY_MANAGED`。id 格式在 `gateway/cc-switch/src-tauri/src/mode/stack.rs` 的 `encode`：Claude `ccs-claude-<key>--<model>`，Codex `ccs-<key>/<model>`，Gemini `ccs-gemini-<key>--<model>`（`/` 写成 `~`），其余 `ccs-<app>-<key>/<model>`。`Session` 不调用 `Gateway.ensure`。
- 2026-10-10 用大脑 `agnes/agnes-2.5-flash`、提示「只用一个词回答：pong」实测，Claude、Codex、Gemini、Grok、OpenCode、OpenClaw、Hermes、Pi 都返回了模型回复。Pi 的开场技能清单已用 `quietStartup` 挡掉。Gemini 的工具 schema 在转发前补上 `parameters`。
- Mcode（MiniMax Code）已跑通。二进制在 `~/.minimax-code/bin/mcode`（0.6.5），不在默认 PATH 里，身体会自己找这个官方安装目录。`get_adapter` 走 Codex 适配器。2026-10-10 同一提示下，`POST /mcode/v1/chat/completions` 返回 `pong`，`cli-mcode` 对话也是 `pong`。
- 协作只做了阶段 1：`orchestration/group.py` 的 `build_prompt` 让后一个 Agent 看到前面的产出，`api/chat.py` 在群聊回合调用它。两个真实 CLI「乙引用甲的具体内容」这一条没有在最近这次实测里重跑。阶段 2（委派）和阶段 3（规划者）没有代码。
- 身体是两个：`llm_body` 直连，`acp_body` 覆盖全部 CLI。协作不用 crewai 的执行引擎，理由在 `AGENTS/architecture.md`。

## Non-Goals

- 不更换网关，不恢复 Gemini 的 Google OAuth client id / secret。
- 不把用量、技能、MCP 改去读 cc-switch 的库。本应用没有 Prompt 页，不接它的 Prompt 表。
- 不重做已经落地的进程配置和发布 id。不改用户的 `~/.claude`、`~/.codex`、`~/.gemini`、`~/.grok`、`~/.hermes/config.yaml`、`~/.openclaw`。
- `llm` 继续直连。引擎扫描与 `--version` 是否一致，本次未复核，不挡下面的切片。

## Approach

模型路径先收口，再证明阶段 1 在真机上成立，然后做委派，最后做规划者。委派是循环里的一步，不是 Agent 自己去起进程。循环仍在 `orchestration/`，执行仍走现有 `Executor`。

## Work Slices

### Slice 1：Mcode 走和 Pi 一样的转发

- Intent：网关认 Mcode 的发布 id，占位凭据能换成真实 key。对话是否跑通取决于本机有没有 `mcode`。
- Files：`gateway/cc-switch/src-tauri/src/proxy/providers/mod.rs`。进程配置已在 `acp_body.py` 的 `_mcode_env`，投影已走 `_openai_compatible`。
- Steps：`get_adapter` 与 `from_app_type_and_config` 里把 `AppType::Mcode` 从 `None` 改成 `CodexAdapter`，与 Pi 同一条。重建 `ccsw-proxy` 后，用发布 id 和 `PROXY_MANAGED` 打 `POST /mcode/v1/chat/completions`，期望 200。
- Verify：上面的 HTTP 返回上游模型的正文。本机有 `~/.minimax-code/bin/mcode` 时再跑一轮 `cli-mcode` 对话，回复必须是模型文本。
- 结果：2026-10-10 已通过。HTTP 与对话都是 `pong`。二进制不在默认 PATH 上，`_mcode_launcher` 会找官方安装目录。
- Checkpoint：单独提交。尚未提交。

### Slice 2：用两个真实 CLI 证明阶段 1

- Intent：后一个 Agent 的回复能用到前一个的具体产出，而不只是各答一遍。
- Files：`src/crewchat/api/chat.py`、`src/crewchat/orchestration/group.py`。只在真机失败时改。
- Steps：同一个会话放两个已实测能说话的 Agent（例如 OpenCode 与 Codex），任务让甲给出一个具体词或数字，让乙复述。看乙的气泡。
- Verify：乙的文本包含甲的那个词或数字。事件仍是每个 Agent 一个 `msg_id`，`turn_complete` 只在最后一个为真。失败时先看 `build_prompt` 的 `prior` 有没有进乙的 prompt，再改循环。
- Checkpoint：通过后再做委派。不通过就不开始 Slice 3。

### Slice 3：显式委派

- Intent：一个 Agent 可以请求把子任务交给在场的另一个 Agent，循环决定是否执行，界面能看到这一步。
- Files：`src/crewchat/orchestration/group.py`（或同目录新文件）、`src/crewchat/api/chat.py`。注入方式先读 `AGENTS/bodies.md`：各 CLI 的 MCP 入口不一样，不在执行器里写会话状态。
- Steps：定义 `handoff(to, task)`。循环认出这次调用后，把任务放进待办，下一轮用被点名的 Agent 跑，prompt 带上委派说明和已有产出。Agent 只能请求，不能自己起进程。
- Verify：甲把一个子任务交给乙。流里能看到委派这一步，乙的回复针对该子任务，而不是把用户原话再答一遍。取消进行中的回合时，待办不再继续跑。

### Slice 4：规划者

- Intent：一个 leader 把任务拆成带负责人的子任务，循环按依赖顺序执行，最后汇总。
- Files：`src/crewchat/core/models.py`（只加数据形状）、`src/crewchat/orchestration/`、`src/crewchat/api/chat.py`。
- Steps：leader 先跑一轮，产出结构化子任务（谁做、依赖谁、完成标准）。循环按依赖调度，仍走 Slice 3 的委派。汇总用最后一轮，不另起执行引擎。只借 crewai 的字段命名，不用它的循环。
- Verify：一个需要两步的任务（先查再改）由 leader 拆开，第二步能看到第一步的产出，界面上能看出负责人和顺序。

## Risks

- Slice 2 若失败，原因可能在提示词截断（`PER_OUTPUT_LIMIT`）或群聊根本没把 `prior` 传进去。先看进 prompt 的文本，再改协议。
- 委派若做成「Agent 自己 spawn 另一个 CLI」，会绕开网关和取消。必须留在循环里。
- Mcode 的二进制缺失时，不要用回显 server 的成功代替对话成功。
- 阶段 3 的子任务若再塞回用户原话，会退回阶段 1 之前的「各答一遍」。

## Done Criteria

- Mcode 的 `POST /mcode/v1/chat/completions` 在发布 id + `PROXY_MANAGED` 下返回 200。有二进制时，对话回复是模型文本。
- 两个真实 CLI 的群聊里，后者引用了前者的具体产出。
- 委派在事件流里可见，被委派的 Agent 执行的是子任务，回合可取消。
- 规划者拆出的子任务按依赖执行，汇总能对上各步产出。
- `uv run pytest -q`、`uv run python scripts/check_layers.py`、`uv run python scripts/check_imports.py` 通过。
