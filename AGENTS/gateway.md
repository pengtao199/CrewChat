# 模型网关

## 适用范围

改动模型接入、上游端点、网关配置，或遇到"某 CLI 不支持某端点 / 某端点的工具格式被拒"类问题时。

## 目标

让任意 OpenAI 兼容端点驱动任意身体；协议差异由网关吸收，不进入业务代码。

## 网关 = cc-switch 代理核心

网关由 **cc-switch 的代理核心**承担（独立进程 `ccsw-proxy`），已取代 LiteLLM。

`ccsw-proxy` 是我们给 `farion1231/cc-switch`（MIT）加的 headless 入口：复用它的
`ProxyService`、`mode::controller` 与工具层，不传 Tauri handle。**核心源码已固化进本仓库
`gateway/cc-switch/`**（upstream commit 与我们的改动清单见其 `NOTICE.md`），从仓库内构建，
不依赖外部克隆。**选型依据见 `AGENTS/dependencies.md` 要求**：对比 LiteLLM 后选它，
因为它原生完成三件事，而我们此前要分别拼：

1. **三种线格式互转**，且 **codex 路径会解析它自己的私有工具形状**（`namespace` / `web_search`），
   按上游能力摊平或转译 —— 正是"某 CLI 发出第三方网关不认的工具"这类问题的通用解法。
2. **供应商（provider）管理**。选型时它自带「每 CLI 一个当前通道」；CrewChat 在此之上用发布 id 做逐请求路由，见下文「进程配置」。
3. **协议差异靠 provider 的 `apiFormat` 表达**（`openai_responses` / `openai_chat` / `anthropic`），
   不靠试错。

| 客户端 | 需要的线格式 | 上游常见能力 |
| --- | --- | --- |
| codex | `/v1/responses` | 多数第三方端点只有 `/v1/chat/completions` |
| claude | `/v1/messages` | 同上 |
| opencode / llm | `/v1/chat/completions` | 一致 |

代理一个进程同时暴露三种格式，因此所有身体都能接任意上游。

## 原则

- **网关是基础设施，不是依赖**：独立进程，经 HTTP + 只读 SQLite 通信。不把 cc-switch 加进项目
  Python 依赖。
- **网关是"端口 + 适配器"**：业务只依赖 `core/gateway.py` 的 `ModelGateway` 端口
  （`status` / `ensure_running` / `stop` / `activate(app, provider)` / `deactivate` / `usage`），
  实现可替换。当前实现是 `core/gateway_ccswitch.py`，由 `Settings.gateway_kind` 选。
  新增网关实现只在 `core/` 加一个适配器 + 在工厂登记，业务与 API 不动。
- **协议差异集中在网关**：业务代码只认 OpenAI 兼容语义。
- **端点能力差异显式表达**：上游不支持 Responses 扩展（如 codex 的 `namespace` 工具）就在
  provider 的 `apiFormat` 里表达，让网关走转译路径，不靠客户端试错、不在身体里手写转换。

## 具体规则

- 新增推理端点时，**只改 `.data/providers.jsonc`**（一份通用 provider）；某个 CLI 要用它时由
  `Store.project_provider(app, provider)` 投影进网关。不直接往网关库里塞 provider。
  - 投影按 app_type 生成该 CLI 的 `settings_config`，并把通用 provider 的 `api_format` 写进 meta：
    上游原生支持 Responses 扩展 → `openai_responses`；仅 chat/completions → `openai_chat`；原生 Anthropic → `anthropic`。
  - 判据不是"能不能连上"，而是"上游能否接受该 CLI 发出的完整工具声明"。
- **一份通用 provider，所有 CLI 复用**：不为每个 CLI 各存一份；投影把它编译成各 CLI 的格式，核心负责反代与协议转换。
- 让某 CLI 走代理经端口 `Gateway.activate(app, provider)`。返回值是该 CLI **本次应使用的发布 id**。
  每个经网关的 CLI 都返回 id，不返回 `None`。`llm` 不走这里。
- **进程配置是模型绑定的位置**。每个 ACP 进程写自己的一份配置，模型名是这次的发布 id，凭据是
  `PROXY_MANAGED`。用户的 `~/.claude`、`~/.codex`、`~/.gemini`、`~/.grok`、`~/.hermes/config.yaml`、
  `~/.openclaw` 不是 CrewChat 的写入目标。Hermes 的临时家放在 `~/.hermes/crewchat-sessions/`
  （家在这份目录外面时，Hermes 会把这次启动当成另一份安装并重装依赖），进程退出时删掉这一层。
  `activate` 只登记 Stack，不调用 `enter_with_route`。
  `Session` 不调用 `Gateway.ensure`。
- **同一个 CLI 的多个会话靠发布 id 区分供应商**。Claude `ccs-claude-<key>--<model>`，Codex
  `ccs-<key>/<model>`，Gemini `ccs-gemini-<key>--<model>`（`/` 写成 `~`，因为模型名在 URL 路径段），
  其余 `ccs-<app>-<key>/<model>`（`mode/stack.rs` 的 `encode`）。代理在该 app 处于「代理 + Stack 开」
  时按 id 逐请求路由。真实 key 只留在网关。
- 隔离位置：Claude `CLAUDE_CONFIG_DIR`，Codex `CODEX_HOME`，Gemini 的 `GEMINI_MODEL` /
  `GOOGLE_GEMINI_BASE_URL`，Grok `GROK_HOME`，OpenCode 的 `OPENCODE_TEST_HOME` +
  `OPENCODE_CONFIG_DIR/opencode.json`（`providers.*.settings`，`modelID` 为发布 id），OpenClaw
  `OPENCLAW_STATE_DIR`（`openclaw acp` 另起一次自己的 Gateway），Hermes
  `~/.hermes/crewchat-sessions/<id>`，Pi
  `PI_CODING_AGENT_DIR`（`models.json` 的 `api` 必须是 `openai-completions`），Mcode
  `MINIMAX_DATA_DIR`。
- **哪些身体走网关由 `Executor.needs_gateway` 决定**：上表里的 CLI 都是 `True`。`llm` 直连。
- 身体的模型指定仍通过大脑池（`Provider.model`）传递；`codex` 的 provider 名放
  `settings.codex_provider`，不硬编码在身体里。
- 密钥的落点见 `AGENTS/truth-sources.md`；不得把密钥写进代码、日志或前端。
- 排查"某个身体接不上"时：先确认该身体需要的线格式 → 再确认该 provider 的 `apiFormat` 是否
  与上游能力一致 → 最后才怀疑端点。

## 禁止项

- 不修改 cc-switch 已安装/已构建产物的源码作为长期方案；确需临时验证时，验证后必须还原并在此记录。
- 不在身体里手写 Responses ↔ Chat 的转换（那是网关的职责）。
- 不在客户端发现"某工具类型不被接受"后，用 `--disable` / `-c` 之类的客户端开关逐个绕过 ——
  那是 provider 的 `apiFormat` 配错，属网关配置问题。
- 不绕过网关直连端点去"验证一下能不能用"然后写进代码。
- 不把网关暴露到非本机地址而不设鉴权。

## 检查

- 这个上游支持哪种线格式？provider 的 `apiFormat` 对不对？
- 换一个新的 OpenAI 兼容端点，需要改几处？理想是只在 cc-switch 加一条 provider。
- 这次改动是否写了用户的 `~/.claude` 或 `~/.codex`？进程临时目录才是模型绑定的位置。
- 读的是 cc-switch 的库还是我们自己的副本？应该只有前者。
