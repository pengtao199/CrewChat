# 模型层：移除 CCR

Updated: 2026-10-22
Status: In Progress
关系：`agentos-shell.md` 的模型页接线不变；`swift-runtime-migration.md` 的 Phase 1 改由本计划承担。终态若迁到 Swift 自研 runtime，模型层仍是独立 HTTP 服务，可原样保留，或按同一套 conformance fixture 移植。

> 历史说明：原文件名为「模型从 ccr 切换到 agentos Swift 原生」。结论已改为 **Rust 模型层 sidecar**，原因见第 1、3 节；文件名同步改为现在这个。

---

## 0. 结论

- 模型层落 **Rust 独立 sidecar**（`model-layer/`），不是 AgentOS 壳；端口沿用 `3456`（网关）与 `3458`（控制面）。
- 翻译核心 **vendor** [`raine/claude-code-proxy`](https://github.com/raine/claude-code-proxy)（MIT，axum 0.8，已实现 OpenCode Go / Responses / `x-opencode-session`），只取约 3.1k 行。
- 模型层 **不感知 Agent、不绑定 CLI**：它只按请求里的 `model` 路由。Agent → `(engine, provider, model)` 的绑定归核心 `codeg.db`，会话注入归 `agent_host/overlay`。
- 3–4 周切换；第一刀 3–5 天，先证明「导入一致 + codeg 只读兼容」，不发真实 prompt。
- 模型层完成后才建 Agent；建 Agent 是「目录 + 绑定 + 每会话注入」三件事，不需要再改网关。

---

## 1. 关键前提：模型、引擎、Agent 是三件事

这是本计划最容易做错的地方。三者的关系是：

```
模型目录（模型层）              绑定（核心 codeg.db）                 会话（每个 Agent 独立）
Provider / Credential    ←    Agent = 引擎 + provider + model   →   每会话独立 home
Model / [1m] / 协议              custom_agent(kind='role')             CLAUDE_CONFIG_DIR
        ↑                                                             CODEX_HOME
        └──────────── 网关只按请求里的 model 路由 ←────────────────── OPENCODE_CONFIG
```

### 四条硬规则

1. **模型层不存「当前模型」或「CLI → 模型」**。任何全局当前态都会让并发 Agent 串味。
2. **Agent 绑定只写 `custom_agent`**，不写网关配置；网关不知道 Agent 存在。
3. **每个会话一个独立 home**（Claude `CLAUDE_CONFIG_DIR`、Codex `CODEX_HOME`、OpenCode `OPENCODE_CONFIG`），模型在进程启动时注入。
4. **`applyProfile` 只服务「用户在终端裸跑 CLI」**；Agent 会话不依赖它。全局 CLI 配置是 fallback，不是主路径。

### 现有能力（已实现，不用重做）

| 引擎 | 模型注入方式 | 现状 | 备注 |
| --- | --- | --- | --- |
| Claude Code | `CLAUDE_CONFIG_DIR/settings.json` → `ANTHROPIC_MODEL` | ✅ `agent_host/role_home.rs` | 写的是虚拟 ID |
| Codex | `CODEX_HOME/config.toml` → `model` + `[model_providers.crewchat]` | ✅ `agent_host/overlay.rs` | 写裸 model |
| OpenCode | `OPENCODE_CONFIG` 分层 `opencode.json` | ✅ `agent_host/overlay.rs` | 写 `crewchat/<model>` |
| Grok / Kimi / Pi / Kilo | 全局配置 / wrapper | ❌ `overlay_support` 返回 Unsupported | Agent 页会锁住；要支持另开 |

结论：**同一个 CLI 的多个 Agent 可以同时跑不同模型**，因为每个会话有自己的 home，互不覆盖。这不是网关要多做的功能，而是核心已经做好的注入机制；网关只要保证「按 model 正确路由」。

### 对模型层的三个要求

1. `model` 必须无歧义：接受虚拟 ID、`Provider/model[1m]`、裸 model（只有唯一 provider 时才接受，否则 400）。
2. 凭据选择是**请求级**的：priority/weight、健康度、429 隔离；不能有「当前凭据」。
3. `/v1/models` 是**目录**：一次返回全部 Claude 可见 ID；每个 Agent 自己挑一个。

### 对 Agent 页的要求

- 建 Agent 时选引擎 → 只列该引擎能注入的模型（`agent_role_model_face` 已有）。
- 模型被删除/禁用时，Agent 行要显示「模型不可用」，不能静默回退到 CLI 默认。
- 同一模型可被多个 Agent、多个引擎同时引用；不做独占。
- Caller key 是**按 CLI** 发的（`profile:codex` 等），不是按 Agent；Agent 级计量以后要做，另加 `x-agent-id` 或 per-agent key，不要把它塞进路由。

### 建一个 Agent 的实际链路（现有代码，不需要新造）

1. Agents 页选引擎 → `agent_role_model_face(agent_type)` 返回该引擎能注入的 provider/model 列表。
2. 保存 → `agent_roles_save` 写 `codeg.db` 的 `custom_agent`（`body`、`provider`、`model`）。
3. 开新会话 → `agent_host::prepare_role_overlay(body, role_key, provider, model)`。
4. overlay 建独立 home，写入网关地址、模型、caller key 的引用（不落明文 key）。
5. CLI 启动 → 请求带模型 → `:3456` 按 model 路由。

模型层完成后，建 Agent **不需要改网关**；只需要保证 `agent_role_model_face` 的列表来自新控制面，并且模型删除时有明确错误。

| 引擎 | Agent 里存的值 | overlay 写入 | 网关收到的形态 |
| --- | --- | --- | --- |
| Claude Code | `brain_provider` + `brain_model` | `ANTHROPIC_MODEL=<虚拟 ID>` | 虚拟 ID |
| Codex | `brain_provider` + `brain_model` | `model = <裸 model>` + `[model_providers.crewchat]` | 裸 model |
| OpenCode | `brain_provider` + `brain_model` | `model = crewchat/<model>` | `crewchat/<model>` |

这张表就是「不同 CLI 跑不同模型」的全部机制：差异只在注入方式，网关只认最后的 `model` 字符串。`crewchat/` 是 OpenCode 配置里的 provider 名，不是 CCR 供应商名，网关要剥掉它再解析。

---

## 2. 实测现状（2026-10-22）

从正在运行的 CCR 实时读取（`getConfig` + `/v1/models` + `config.sqlite`）：

| 数据 | 现状 |
| --- | --- |
| Provider | 3 个：OpenCode Go、Agnes、OpenRouter |
| OpenCode Go | `openai_responses`，13 个路由模型、31 个 autoFetch、2 条凭据、3 个额度 meter（5h / weekly / monthly） |
| Agnes | `openai_chat_completions`，6 个模型、2 条凭据 |
| OpenRouter | `openai_chat_completions`，19 个模型、1 条凭据 |
| Claude 可见模型 | **38** 个虚拟 ID，其中 **8** 个带 `[1m]` |
| Caller keys | **7** 条：`local-gateway` + 6 条 `profile:*`（Claude / Codex / Grok / Kimi / Pi / Kilo） |
| 配置存储 | `~/.claude-code-router/config.sqlite` → `app_config.default`、`api_keys`、`runtime_state` |
| 禁用模型 | 核心数据目录下的 `ccr-disabled-models.json` |
| 全局 Claude | `~/.claude/settings.json`：base → 3456，`apiKeyHelper` → CCR bin，模型是 `OpenCode Go/mimo-v2.6-flash[1m]` |

### 对原文档的两处修正

1. **OpenCode Go 是 `openai_responses`，不是 Chat**。Claude Code 的翻译目标以 **Responses** 为主、Chat 为 fallback。原文档写「首选 Chat，比 Responses 简单」会改变现有行为（reasoning / usage 字段会丢）。
2. **`[1m]` 不是独立模型**。它是 Claude 可见 ID 的后缀，解码和转发上游时剥掉；存储上记一个 `one_million` 布尔，不要建两条模型。

---

## 3. 目标架构

```
AgentOS.app (Swift)
  ├─ 启动 codeg-server（不变）
  ├─ 启动 model-layer sidecar（新，Rust）
  └─ 只渲染

        ┌──────────────────────────────┐
        │ codeg-server                 │
        │  agent_host / overlay        │
        │  body_model_face             │
        └──────────┬───────────────────┘
                   │ CCR_WEB_URL=http://127.0.0.1:3458
                   ▼
        ┌──────────────────────────────┐
        │ model-layer :3458 控制面      │  getConfig / saveConfig
        │                              │  getProviderAccountSnapshots
        │                              │  probeProvider / applyProfile
        ├──────────────────────────────┤
        │ model-layer :3456 网关        │  /v1/models
        │                              │  /v1/messages（翻译）
        │                              │  /v1/chat/completions（透传）
        │                              │  /v1/responses（透传）
        └──────────┬───────────────────┘
                   ▼
        OpenCode Go / Agnes / OpenRouter
```

Claude Code / Codex / OpenCode 仍经各自 CLI 的 per-role overlay 指向 `:3456`；模型由每个 Agent 的 home 在启动时决定。

### 为什么是独立 sidecar，不是 AgentOS 壳

| 维度 | 独立 `model-layer/` | 放进 AgentOS 壳 |
| --- | --- | --- |
| 分层契约 | 模型与密钥归模型层，壳只渲染 | 违反 `agentos-boundary.md` 三条（路由、协议、持久化模型） |
| codeg 改动 | **零**（只改 `CCR_WEB_URL`） | 要等壳实现完才能用 |
| 未来 Swift runtime | 同一个 HTTP 服务可直接复用 | 迁到 Swift 时还要再搬一次 |
| 故障域 | CCR 死了不影响会话引擎 | 网关拖垮 UI |
| 协议翻译 | 复用 Rust 现成实现 | Swift 没有成熟基座，要重写并重测 |

一句话：**模型层是基础设施，不是壳的功能。** 做成 HTTP 服务，codeg 和未来的 Swift runtime 都能用；做进壳里，等于把基础设施绑在一个客户端上。

### 为什么也不塞进 `codeg/src-tauri`

- 会动 vendored 上游文件，每次升级都要重放 `[crewchat]` 补丁。
- 切换/回退要改代码重编译，不能靠一个环境变量。
- 它和 codeg-server 同生共死，而模型层在 Swift 迁移后还要活着。

---

## 4. 兼容契约（必须逐字对齐）

### 端口与鉴权

| 项 | 值 |
| --- | --- |
| 网关 | `127.0.0.1:3456` |
| 控制面 | `127.0.0.1:3458` |
| 控制面鉴权 | `x-ccr-web-auth: <token>` |
| 请求/响应包装 | 请求 `{"method": "...", "args": [...]}`；响应 `{"ok": true, "value": ...}` 或 `{"ok": false, "error": {"message": "..."}}` |
| 网关鉴权 | `Authorization: Bearer <caller key>`（继续接受导入的 `ccr-profile-*`） |
| 发现 | 写 `~/.claude-code-router/service.json`（`url` 带 `?ccr_web_token=…`），或由 AgentOS 设 `CCR_WEB_URL` |
| 监听 | 仅回环，禁止 LAN / 公网 |

### 控制面方法（codeg 实际调用的全集）

| 方法 | 用途 | 备注 |
| --- | --- | --- |
| `getConfig` | 模型页、`body_model_face`、overlay | 响应必须带 `configRevision`；`Providers[].models` 是 Codex/OpenCode 的面，不能和 `/v1/models` 的 Claude 面混用 |
| `saveConfig` | 模型页写回整份配置 | 调用是 `saveConfig(config, {applyProfile: true})`；必须做 revision 冲突 409；保留未知字段；`applyProfile` 只写 claude-code 的全局配置；codex / opencode 暂不覆盖（Agent 会话走 per-role overlay） |
| `probeProvider` | 测试连接 / 拉模型 | 只发最小请求 |
| `getProviderAccountSnapshots` | 额度 | 见 Phase 4 |
| `getProviderPresets` | 新建供应商预设 | 可静态 |
| `getGatewayStatus` | 模型页状态卡 | 合成健康状态 |
| `startGateway` / `stopGateway` / `restartGateway` | 状态卡按钮 | 可 no-op |
| `applyProfile` | 写全局 CLI 配置 | **只服务终端裸跑；Agent 会话不依赖** |

### 模型 ID 的四种形态

| 形态 | 例子 | 谁发 |
| --- | --- | --- |
| Claude 虚拟 ID | `anthropic/claude-ccr-h<hex>[1m]` | Claude Code，`hex = lowercase_hex("Provider 显示名/model")` |
| 人读 label | `OpenCode Go/mimo-v2.6-flash[1m]` | 全局配置、Agent 页 |
| `Provider/model` | `OpenCode Go/mimo-v2.6-flash` | 模型页 / roster 展示 |
| `crewchat/<model>` | `crewchat/mimo-v2.6-flash` | OpenCode overlay（`crewchat` 是配置里的 provider 名） |
| 裸 model | `mimo-v2.6-flash` | Codex overlay；仅在唯一 provider 服务它时接受 |

规则：
- `[1m]` 在解码、路由、转发上游时剥掉；响应里保留 Claude 需要的语义。
- `crewchat/` 前缀在路由前剥掉，不能当成 CCR 供应商名。
- 裸 model 命中多个 provider → **400**，不猜。
- Provider 改名继续禁止（会破坏 hex 编码和已有 Agent）；模型删除保留 tombstone，Agent 页显式提示。

### `/v1/models`

| UA | 返回 |
| --- | --- |
| Claude UA（`claude-cli/...`） | 38 个 `anthropic/claude-ccr-h<hex>`，其中 8 个带 `[1m]` |
| 其他 UA | 38 个 `Provider/model`，不带 `[1m]` |

两种列表都必须与现网逐字一致；这是 `body_model_face` 和 Claude role 解析的输入。已有 Agent 的 `provider` / `model` 不改，导入后按原值解析；解析不到就显式报「模型不可用」，不要改写花名册。

---

## 5. 数据模型

SQLite（`~/Library/Application Support/AgentOS/model/model.db`，0600）。原始 JSON 整段保留，规范化列只用于查询。

| 表 | 关键列 | 说明 |
| --- | --- | --- |
| `providers` | `id`、`name`、`type`、`api_base_url`、`raw_json`、`enabled`、`position` | `raw_json` 保留 `account` / `capabilities` / `icon` / `preferredProvider` |
| `provider_models` | `provider_id`、`model`、`one_million`、`routable`、`auto_fetch`、`enabled`、`display_name`、`capabilities_json` | `[1m]` 只存布尔 |
| `credentials` | `id`、`provider_id`、`name`、`api_key`、`priority`、`weight`、`enabled`、`limits_json`、`position` | 0600；日志永不打印 |
| `caller_keys` | `id`、`name`、`key`、`limits_json`、`enabled` | 必须可还原：`getConfig.APIKEYS` 要把它交给 codeg，overlay 再写进会话环境；不能只存 hash |
| `disabled_models` | `provider_id`、`model` | 与现有 `ccr-disabled-models.json` 对齐 |
| `legacy_config` | `key`、`value_json` | `profile` / `Router` / `preferredProvider` 原样回放 |
| `quota_snapshots` | `credential_id`、`meters_json`、`fetched_at` | 过期显示缓存，不伪造 |
| `imports` | `source`、`source_hash`、`imported_at` | 幂等 |

**密钥落点**：v1 沿用 CCR 的做法——provider key 与 caller key 存在 0600 的 SQLite 里；DB 与备份不进 git、不进日志。签名稳定后再评估迁 Keychain（开发期 ad-hoc 签名会让 Keychain ACL 反复弹窗，见 `AGENTS/agentos-boundary.md` 的同一条理由）。

导入源：**运行中的 CCR**（`getConfig` 会注入 `APIKEYS`，静态 sqlite 不会）+ 实时 `/v1/models` 两种 UA + `api_keys` 表 + 禁用清单。不要只读 `config.sqlite`。

---

## 6. 路由与并发语义

每个请求构造一个请求级 `Route`，不允许任何跨请求的可变「当前状态」：

```
Route {
  requested_model: String,
  provider_id: String,
  protocol: Responses | Chat | Anthropic,
  credentials: Vec<CredentialRef>,   // enabled + weight
  one_million: bool,
  caller_key_id: String,
}
```

| 关注点 | 规则 |
| --- | --- |
| 模型解析 | 虚拟 ID → hex 解码 → `(provider 显示名, model)`；label → 精确匹配；裸 model → 唯一才接受 |
| 凭据选择 | 每次请求按 weight/priority 选一个；429/401 只影响该凭据健康度，不写全局 |
| 协议 | provider 级 `type` 决定；允许 per-model 覆盖 |
| 并发 | 请求之间无共享缓冲；流式响应各自持有上游连接 |
| 取消 | 客户端断开 → drop 上游流；不影响其他请求 |
| 失败隔离 | 一个模型/凭据的错误不得改变另一个 Agent 的路由 |
| 日志 | request id + model + provider + credential id；不打 key、不打 prompt |

---

## 7. 开源复用

| 候选 | 语言/许可 | 结论 |
| --- | --- | --- |
| [raine/claude-code-proxy](https://github.com/raine/claude-code-proxy) | Rust / MIT / 662★ / 活跃 | **选它**：axum 0.8，OpenCode Go、Responses、`x-opencode-session`、SSE、工具调用都已实现 |
| [9j/claude-code-mux](https://github.com/9j/claude-code-mux) | Rust / MIT / 516★ / 2025-11 停更 | 参考路由 / failover；配置是 TOML + 自带 UI，不作基座 |
| [jasonxu114514/opencode2api](https://github.com/jasonxu114514/opencode2api) | Go / GPL-3.0 | 许可证不适合 vendor |
| LiteLLM / Bifrost | Python / Go | 换语言没收益，还多一个运行时 |
| Swift 候选（`token-share` 26★、`model-gate` 0★） | Swift | 没有可复刻的成熟基座 |

### 只 vendor 这些（约 3.1k 行）

| 文件 | 行数 | 用途 |
| --- | --- | --- |
| `anthropic/{schema,sse,error}.rs` | ~195 | Messages 类型与 SSE |
| `providers/opencode/model.rs` | 278 | 模型 / 端点解析 |
| `providers/opencode/responses.rs` | 494 | Anthropic → Responses 流式翻译 |
| `providers/opencode/chat.rs` | 1701 | Anthropic → Chat 翻译（工具 / thinking / usage） |
| `providers/opencode/client.rs` | 368 | 上游请求、`x-opencode-session`、UA |
| `providers/translate_shared.rs` | 232 | system / tool / image / effort 归一化 |

保留 `LICENSE` 与版权头，新增 `model-layer/NOTICE.md` 记录来源、commit 与改动。

### 备选（如果提取耦合太重）

把 `claude-code-proxy` 二进制当子进程，只让它处理 OpenCode Go；我们的控制面在前面。多 2 天，仍比从零写快。

---

## 8. 分阶段计划

### Phase 0：冻结与 fixture（0.5–1 天）

- 备份 `config.sqlite`（含 WAL）、`api_keys`、`ccr-disabled-models.json`、`~/.claude/settings.json`、`~/.codex/config.toml`、被 CCR 写过的其他全局配置。
- 导出 `legacy-ccr-config.json`（0600，含 key，仅回滚）。
- 录 fixture：`getConfig`（脱敏）、`/v1/models` 两种 UA、7 条 caller key 的 id/name、**一段真实 Claude 流式 + 工具调用**、一段 Codex/Responses 流。
- **Gate**：fixture 可在测试里回放；不打印任何 key。

### Phase 1：Store + 导入 + 只读控制面（3–5 天）

- 建 `model-layer/`，SQLite schema + 导入器。
- `:3458` 实现 `getConfig` / `getProviderPresets` / `getGatewayStatus`。
- `:3456` 实现 `/v1/models` 两种 UA。
- `CCR_WEB_URL` 指向新服务，codeg 不改代码。
- **Gate**：3 个 provider、13+6+19 个模型、38 个 ID、8 个 `[1m]`、7 条 caller key 与现网逐字一致；模型页、`body_model_face` 结果不变；不发真实 prompt。

### Phase 2：网关透传 + 并发（3–4 天）

- `/v1/chat/completions`、`/v1/responses` 透传；模型解析、凭据池、`x-opencode-session`、UA。
- 请求级 `Route`；凭据健康度；取消与超时。
- **Gate**：Codex + OpenCode 各跑一轮；两个不同模型并发互不串；一个凭据 429 不影响另一个。

### Phase 3：Claude 翻译层（5–7 天）

- `/v1/messages`：Anthropic → Responses（主）/ Chat（fallback）；SSE 反向翻译。
- 文本、工具调用、工具结果、finish_reason、usage；`[1m]` 剥离；`count_tokens` 先估算。
- **Gate**：Claude Code 完整一轮：流式文本、工具调用、工具结果、取消、多轮工具循环不崩。
- 用 Phase 0 的 fixture 做逐帧对比；不一致先修 fixture，不改预期。

### Phase 4：写入、探测、额度（3–4 天）

- `saveConfig`（revision 409、保留未知字段）、`probeProvider`、模型/凭据开关（现有 `provider_toggles` 直接复用）。
- `getProviderAccountSnapshots`：受限 JSONPath + 四则表达式，覆盖现有 `http-json` connector；拉不到显示缓存 + 时间，不伪造。
- `applyProfile`：只做 claude-code 的最小写入；codex / opencode 的全局配置暂不覆盖（Agent 会话走 per-role overlay）。
- **Gate**：模型页 CRUD 正常；额度与切之前一致；`forceRefresh` 可刷新。

### Phase 5：切换与回退（1–2 天）

- 停 CCR launchd；AgentOS 启动 `model-layer`；`CODEG_CCR_SUPERVISE=0`。
- 保留 CCR 数据与回退开关一个版本。
- **Gate**：`lsof` 无 CCR；Claude/Codex/OpenCode 各一轮；重启正常；回退命令可用。

### Phase 6：建 Agents 与按引擎验证（2–4 天）

模型层完成后才做，不需要再改网关。

- 在 Agents 页建 3 个 Agent：Claude / Codex / OpenCode，各配不同 provider + model。
- `use-agent-ccr-binding` 现在会调 `applyProfile`；切换后它只更新全局 fallback，**不能成为会话生效的前提**。
- 同时跑：两个 Claude Agent 用不同模型；一个 Claude + 一个 Codex 用同一 provider。
- 删/禁一个模型，确认 Agent 页显示「模型不可用」，会话不静默换模型。
- 补齐或明确锁住 Grok / Kimi / Pi / Kilo：没有 per-role overlay 的引擎，Agent 页继续显示 Unsupported，不要假装支持。
- **Gate**：每个 Agent 跑在自己选的模型上；并发不串味；`applyProfile` 不再是会话前置条件。

**总工期：3–4 周。** Phase 3 是唯一的关键路径；如果提取失败，走第 7 节的备选，回到 2 周内。

---

## 9. 验收清单

### 数据

- [ ] 3 个 provider、13+6+19 个模型、38 个 Claude ID、8 个 `[1m]`、7 条 caller key，与现网逐字一致。
- [ ] `priority` / `weight` / `limits` / `enabled` / 顺序保留。
- [ ] `account`、`capabilities`、`modelDisplayNames`、`autoFetchKnownModels` 未丢。
- [ ] 全程无 key 出现在日志/测试输出。

### 会话

- [ ] Claude Code：流式文本、工具调用、工具结果、取消、多轮工具循环。
- [ ] Codex：`/v1/responses` 透传。
- [ ] OpenCode：`/v1/chat/completions` 透传。
- [ ] 全局 `apiKeyHelper` 继续可用，或已换成新 helper。

### 并发（这一项不能省）

- [ ] 两个 Claude Agent 同时跑不同虚拟 ID，互不串。
- [ ] 一个 Claude + 一个 Codex 同时用同一 provider，互不影响。
- [ ] 一个凭据 429，另一个凭据/另一个 Agent 不受影响。
- [ ] 取消一个流，另一个流继续。

### Agent

- [ ] 三个引擎各建一个 Agent，模型不同，各自生效。
- [ ] 模型被禁用后，Agent 页提示不可用，不静默回退。
- [ ] 同一模型被多个 Agent 引用，不做独占。

### 回退

- [ ] 停 `model-layer`、启 CCR、恢复 `CCR_WEB_URL`，模型页与 CLI 全部恢复。
- [ ] 备份的 `config.sqlite`、全局配置可原样恢复。

---

## 10. 风险与对策

| 风险 | 影响 | 对策 |
| --- | --- | --- |
| vendored 代码与它的 config/auth 耦合 | 提取失败 | 2 天 spike；失败走「二进制子进程」备选 |
| Responses / Chat 行为差异 | Claude 工具调用或 usage 异常 | Phase 0 录真实 SSE，逐帧对比 |
| 虚拟 ID 与 `[1m]` 对不上 | Claude 拒绝启动或 Agent 静默回退 | 导入实时 `/v1/models`；`one_million` 单列；Agent 页显式提示 |
| `apiKeyHelper` 指向 CCR bin | 删 CCR 后 Claude 起不来 | 先导入 7 条 key 继续接受；切换后再换 helper |
| `applyProfile` 体量大 | 模型页改完全局 CLI 不生效 | 只做 claude；Agent 会话本来不走它 |
| 双写 | 配置互相覆盖 | 导入后停 CCR，单写入者 |
| 并发串味 | 两个 Agent 用错模型 | 禁止全局当前态；请求级 Route；并发验收 |
| 端口冲突 | 切换时 3456/3458 被占 | 先停 CCR launchd；失败即回退 |

---

## 11. 不做什么

- 不在 AgentOS 壳里做模型存储、路由、协议翻译。
- 不做「CLI → 模型」的全局绑定；绑定只属于 Agent。
- 不做 CCR 的全量 provider 矩阵、插件、bot gateway、virtual model profiles。
- 不在网关里判定 Agent、权限或团队。
- 不伪造额度、不静默换模型、不猜有歧义的裸 model。
- 不为了「以后可能需要」提前做 per-agent caller key；要做另开计量计划。

---

## 12. 第一刀

不动 UI，不改 `~/.claude/settings.json`，先做三件只读的事：

1. **备份与冻结**：见 Phase 0。
2. **录 fixture**：`getConfig`、两种 UA 的 `/v1/models`、一段真实 Claude 流式 + 工具调用。
3. **2 天 spike**：把 3.1k 行拉进 `model-layer/`，假上游回放，证明「Anthropic 进 → Responses 出 → Anthropic SSE 回」。

这三步无论最后走 Rust 还是将来移植 Swift 都不浪费：fixture 就是 conformance 测试。

---

## 13. 进展（2026-10-22）

代码在 `model-layer/`（Rust，二进制 `crewchat-model`）。

### 已完成

- **Phase 0 / 1**：Store + 从运行中的 CCR 导入 + 只读控制面 + `/v1/models`。
  - 实测：3 个 provider、13+6+19 个模型、38 个 Claude ID、8 个 `[1m]`、7 条 caller key；
    与现网 `/v1/models` 的集合和顺序完全一致。
- **Phase 2**：`/v1/chat/completions`、`/v1/responses` 透传；请求级凭据池（priority +
  weight）；没有全局「当前凭据」或「当前模型」。
- **Phase 3（Chat 面）**：`/v1/messages` → Chat 翻译，流式文本、工具调用、工具结果、
  `stop_reason`、usage；已用假上游验证完整 SSE 事件序列。
- **控制面**：`getConfig` / `saveConfig`（revision 409）/ `probeProvider` /
  `getProviderPresets` / `getGatewayStatus` / 启停 no-op；`saveConfig` 往返与
  stale revision 已实测。
- **Phase 4**：额度 `http-json` mapping 已实现（实测 OpenCode Go 2 条凭据 ×
  3 个 meter）；`applyProfile` 写 `~/.claude/settings.json` + 自己的
  `apiKeyHelper`（默认不在 saveConfig 时自动应用，`MODEL_LAYER_APPLY_PROFILE=1` 打开）。
- **AgentOS 接线**：壳在启动核心前拉起 `crewchat-model`，端口被 CCR 占用时自动改用空闲端口，
  并把 `CCR_WEB_URL` / `CCR_WEB_AUTH_TOKEN` / `CODEG_CCR_SUPERVISE=0` 传给核心。
- **切换工具**：`model-layer/scripts/cutover.sh` / `rollback.sh` / `apply-profile.sh`；
  规则已同步到 `AGENTS/gateway.md`、`truth-sources.md`、`architecture.md`。
- 25 个单元测试通过（模型解析、store 往返、翻译、SSE 状态机、额度求值）。

### 未完成

- Responses 面的 Anthropic 翻译（Claude 目前走 Chat；OpenCode Go 的 Chat 端点可用）。
- 正式打包的 copy phase（开发用 scheme / `install-dev.sh`）。
- CCR 进程下线本身（脚本已就绪，等用户执行 `cutover.sh`）。
- Keychain 迁移（现在沿用 0600 SQLite）。

### 下一步

Phase 5（执行 `cutover.sh`，CCR 下线）→ Phase 6（建 Agents 并按引擎验收）。
