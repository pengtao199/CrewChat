# CLI 模型绑定：按进程配置，不改用户全局

Updated: 2026-10-10 (Asia/Shanghai)
Status: 已落地。回显（本地假上游，不是线上 ccsw-proxy）确认 Claude、Codex、Gemini、Grok、OpenCode、Pi、Mcode、Hermes、OpenClaw 都会把发布 id 和 `PROXY_MANAGED` 打到本机反代。Hermes 的临时家在 `~/.hermes/crewchat-sessions/`。OpenClaw 每次另起自己的 Gateway。假上游不返回 SSE，所以 Hermes / OpenCode 的回合在发出请求后超时，请求本身已经带对了模型和占位凭据。

## Goal

每个 ACP 进程只拿到「反代地址 + 占位凭据 + 这次要用的模型名」。网关继续用 cc-switch，按请求里的模型名选供应商、换上真实 key。用户自己的 `~/.claude` 与 `~/.codex` 不被改写，同一个 CLI 上两个 Agent 可以同时用两家模型。

## Current Facts

- 身体调用已经统一走 ACP（`src/crewchat/executors/acp_body.py`）。`llm` 不是 CLI，直连大脑池。
- 网关进程是 headless `ccsw-proxy`。Claude / Codex 的逐请求路由靠 Stack 发布 id：Claude `ccs-claude-<key>--<model>`，Codex 官方格式是 `ccs-<key>/<model>`（`gateway/cc-switch/src-tauri/src/mode/stack.rs` 的 `encode` / `decode`）。`stack::resolve` 只在该 app 已处于代理且 Stack 打开时认这些 id。
- `654d2ec` 把「全部 CLI `needs_gateway` + `PROXY_MANAGED`」提交进了仓库，并把 Codex 的 id 改成了 `ccs-<app>-<key>/<model>`。`generic_app_of` 按第一个 `/` 切开，解不出 app 名，Codex 的旧 id 和新 id 都路由不到。这不是本计划，实施时要收回。
- `Session._gateway_model` 之后会调 `Gateway.ensure`，而 `ensure_projection` 会把全局 `~/.claude/settings.json` 和 `~/.codex/config.toml` 再写一遍。
- `AcpBody.run` 在 env 钩子之前把真实 key 放进 `{PROVIDER}_API_KEY`。占位符只写进配置，进程环境里仍有明文 key。
- 官方隔离目录：Claude 用 `CLAUDE_CONFIG_DIR`，Codex 用 `CODEX_HOME`。OpenCode 用 `OPENCODE_CONFIG_CONTENT`。Gemini / Grok 由 cc-switch 的路由模式改它们自己的配置，一条 CLI 一个当前供应商。
- 换网关解决不了这件事。Nyro、MuxLayer、opencodex 也是改客户端配置加按模型名转发，而且一键同步写的是全局配置。Codex 私有工具格式转换已经在 cc-switch 里。

调研记录留在 [gateway-cli-routing-research.md](gateway-cli-routing-research.md)。本文件是实施计划。

## Non-Goals

- 不更换网关，不把 cc-switch 换成 Nyro / MuxLayer / Bifrost。
- 不在这一计划里做协作的路由和委派（见 [multi-agent-collaboration.md](multi-agent-collaboration.md)）。
- 不靠一条「当前路由」让两个会话共用同一家供应商。每个进程带自己的发布 id。
- `llm` 继续直连。

## Approach

网关只负责两件事：持有 key，按模型名转发。模型名写进**这一次进程**的配置，不写进用户的全局配置。

| CLI | 进程配置 | 模型名 | 并发 |
| --- | --- | --- | --- |
| Claude | 临时目录，`CLAUDE_CONFIG_DIR` | `ccs-claude-<key>--<model>` | 可同时多家 |
| Codex | 临时目录，`CODEX_HOME` | `ccs-<key>/<model>` | 可同时多家 |
| Gemini | `GEMINI_MODEL` + `GOOGLE_GEMINI_BASE_URL` | `ccs-gemini-<key>--<model>` | 可同时多家 |
| Grok | `GROK_HOME` | `ccs-grokbuild-<key>/<model>` | 可同时多家 |
| OpenCode | `OPENCODE_TEST_HOME` + `OPENCODE_CONFIG_DIR` 的 V2 `providers.gw`，目录键 `main`，`modelID` 为发布 id | `ccs-opencode-<key>/<model>` | 可同时多家 |
| OpenClaw | `OPENCLAW_STATE_DIR`，`api: openai-completions`，另起本机 Gateway | `ccs-openclaw-<key>/<model>` | 可同时多家 |
| Hermes | `~/.hermes/crewchat-sessions/<id>`，不改用户的 `config.yaml` | `ccs-hermes-<key>/<model>` | 可同时多家 |
| Pi | `PI_CODING_AGENT_DIR` 的 `models.json`，`api: openai-completions` | `ccs-pi-<key>/<model>` | 可同时多家 |
| Mcode | `MINIMAX_DATA_DIR` | `ccs-mcode-<key>/<model>` | 可同时多家 |

`activate` 仍把通用 provider 投影进网关并打开 Stack，这样 `resolve` 认发布 id。它不再调用会改写 `~/.claude` / `~/.codex` 的 `enter_with_route` / `ensure`。子进程结束后删除临时目录。子进程环境不放入真实 key。

## Work Slices

### Slice 1：收回 Codex id，停止改写全局配置

- Intent：`654d2ec` 里坏掉的发布 id 和解码回到官方格式；起进程不再调用 `ensure`。
- Files：`gateway/cc-switch/src-tauri/src/mode/stack.rs`、`src/crewchat/orchestration/session.py`、`src/crewchat/executors/acp_body.py`（去掉 `run` 里对真实 key 的 `setdefault`）。
- Steps：Codex 恢复 `ccs-<key>/<model>` 与对应 `decode`。`uses_generic_stack` 不再包含 Codex，`ensure_projection` 对 Codex 不再提前返回。`Session` 不再为 Claude / Codex 调用 `Gateway.ensure`。
- Verify：`stack.rs` 里 Codex round-trip 测试回到 `ccs-deepseek/deepseek/deepseek-v4-pro`。`uv run pytest -q` 通过。
- Checkpoint：单独提交。此后用户全局配置不再被这一路径改写。

### Slice 2：Claude 与 Codex 的进程目录

- Intent：每次 spawn 写一份只属于该进程的配置，模型名用 Slice 1 的发布 id，凭据用 `PROXY_MANAGED`。
- Files：`src/crewchat/executors/acp_body.py`、`tests/test_core_logic.py`。
- Steps：Claude 写临时 `settings.json` 并设 `CLAUDE_CONFIG_DIR`。Codex 写临时 `config.toml`（`wire_api = "responses"`，`base_url` 为反代 `/v1`）并设 `CODEX_HOME`。进程结束删除目录。`activate` 对这两家只登记 Stack 成员并返回发布 id，不走会写全局文件的 `enter_with_route`。
- Verify：单测断言临时配置含发布 id 与 `PROXY_MANAGED`，不含 `sk-`。两个不同大脑的 Codex 请求带不同的 `ccs-<key>/…`。

### Slice 3：OpenCode 内联配置

- Intent：沿用 `OPENCODE_CONFIG_CONTENT`，明文 key 不进配置、不进环境。
- Files：`src/crewchat/executors/acp_body.py`、`tests/test_core_logic.py`。
- Steps：provider 名用唯一的 `gw`，`baseURL` 指向反代，`apiKey` 为 `PROXY_MANAGED`，顶层 `model` 为目录短名，`modelID` 为真正发给网关的值。清掉继承来的 `OPENCODE_*`。
- Verify：生成的 JSON 不含真实 key。用回显 server 看请求体的 `model` 是否为 `modelID`。未证实之前，OpenCode 按一条当前路由计，不声称逐请求多家。

### Slice 4：Gemini / Grok 维持官方路由

- Intent：这两家继续 `/enter`，身体不套 `OPENAI_BASE_URL`。
- Files：`src/crewchat/core/gateway_ccswitch.py`、`src/crewchat/executors/acp_body.py`。
- Steps：`activate` 对 `gemini` / `grok` 只调 `/enter` 并返回 `None`。env 钩子不注入 OpenAI 变量。
- Verify：`/enter` 后反代能完成协议转换（返回结构正确）。文档写明一条 CLI 一个当前供应商。

### Slice 5：规则与计划状态

- Intent：规则与落地后的代码一致。
- Files：`AGENTS/gateway.md`、`AGENTS/bodies.md`、本文件状态行、`docs/plans/README.md`。
- Steps：写明进程配置是模型绑定的位置；全局 `~/.claude` / `~/.codex` 不是 CrewChat 的写入目标。身体表按上表更新「经网关」列。
- Verify：规则里的 id 格式与 `stack.rs` 的 `encode` 一致。

## Risks

- `resolve` 要求 Stack 模式已打开。只登记成员、不调用 `enter_with_route` 时，必须确认状态文件里该 app 仍是「代理 + Stack 开」，否则发布 id 会返回 StackOff。
- `OPENCODE_CONFIG_CONTENT` 与全局配置是合并还是替换，尚未用回显 server 证实。Slice 3 的验收就是这一条。
- Gemini / Grok 的官方账号不能经路由。产品上要求第三方供应商。
- 临时目录必须在进程退出时删除，包括取消和异常。

## Done Criteria

- Claude、Codex 的子进程配置在临时目录，用户的 `~/.claude` 与 `~/.codex` 在一次 spawn 前后字节不变。
- 子进程环境与配置中搜不到真实 provider key。
- 两个 Codex（或两个 Claude）同时在场时，请求里的模型 id 分别落到各自的供应商。
- Codex 发布 id 恢复为 `ccs-<key>/<model>`，解码测试通过。
- `uv run pytest -q` 通过。OpenCode 的 `modelID` 若未用回显 server 证实，计划状态保持「OpenCode 逐请求未验证」。
