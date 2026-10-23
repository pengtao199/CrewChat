# MCP 层：统一管理、默认链接、按 Agent 授权

Updated: 2026-10-23
Status: Research（只做调研与方案，不写实现）
关系：`cli-layer`（ACP 宿主 / 花名册）、`mcp-layer`（拟建）、`AgentOS`（MCP / Agents 页）、`model-layer`（Keychain 机制可复用）。
参考：`AGENTS/agent-model.md`、`AGENTS/truth-sources.md`、`AgentOS/engine-api.md`、`cli-layer/src/http.rs`、`cli-layer/src/engines.rs`、`cli-layer/src/roles.rs`。

---

## 0. 先确认理解

我理解的目标是：

1. **MCP 服务统一管理**：一份全局 MCP 库，不再让 Claude / Codex / OpenCode 各存一份分叉配置。
2. **所有 CLI 默认可用、可链接**：安装一个 MCP 后，默认同步到所有**兼容**的 CLI；不兼容的 CLI 要显式标出来，不能静默失败。
3. **使用权由 Agent 管理**：MCP 服务本身是资源；Agent 的 `mcps` 是 allowlist。会话启动时只把该 Agent 选中的 MCP 暴露给它。
4. **和 skill 同构，但不能照搬 skill 的软链实现**：skill 是文件，MCP 是运行时进程或远端连接；MCP 需要处理 transport、生命周期、工具发现、密钥、健康检查。
5. **先调研，不实现**：这一轮只产出兼容性矩阵、方案取舍和分阶段计划。

一句话：**“可用性”按 CLI 默认铺开，“使用权”按 Agent 收窄。** 这两件事必须分开，否则“所有 CLI 默认启用”会变成“所有 Agent 默认拿到全部工具”。

---

## 1. 当前现状

### 1.1 代码与数据

| 位置 | 现状 |
| --- | --- |
| `cli-layer/src/http.rs` | `mcp_scan_local` / `mcp_upsert_local_server` / `mcp_remove_server` 仍返回 501 |
| `cli-layer/src/store.rs` | `agent_roles.mcp_json` 已存在；Agent 角色可存 MCP id 列表 |
| `cli-layer/src/engines.rs` | 15 个引擎；`supports_mcp` 目前只有 OpenClaw 为 `false` |
| `cli-layer/src/roles.rs` | 10 个引擎有 per-role home / 模型 overlay；Gemini、CodeBuddy、Cursor、Qoder、Antigravity 为 `OverlayKind::None` |
| `cli-layer/src/acp/host.rs` | `session/new` 只传 `cwd`，**没有传 `mcp_servers`** |
| `AgentOS` | MCP 页已能展示 `spec` + `apps`（哪些 CLI 写入），但数据来自尚未实现的 501 命令 |
| `codeg/`（冻结只读） | 有 15 个 CLI 的 MCP 扫描/写入适配器，可作为配置格式参考，不能作为运行时 |
| `AGENTS/truth-sources.md` | 当前写的是“各 CLI 自己的 MCP 配置 + 共享 `~/.agents/mcp.json`” |

### 1.2 已经确认的协议能力

当前 `cli-layer` 使用的 ACP SDK 2.2（schema 1.9.1）里，`session/new` 的 `NewSessionRequest` 已经有：

```rust
pub struct NewSessionRequest {
    pub cwd: PathBuf,
    pub additional_directories: Vec<PathBuf>,
    pub mcp_servers: Vec<McpServer>,
    ...
}
```

`McpServer` 支持：

- `stdio`：`name` / `command`（绝对路径）/ `args` / `env`；所有 Agent 必须支持。
- `http`：`name` / `url` / `headers`；需要 Agent capability 声明支持。
- `sse`：`name` / `url` / `headers`；需要 Agent capability 声明支持。

这意味着：**如果各 CLI 的 ACP adapter 真的消费 `session/new.mcp_servers`，MCP 可以像 skill 一样按会话精确注入，不需要依赖全局配置文件。** 这是本轮调研最重要的发现，也是 P0 必须实测的点。

### 1.3 当前缺口

- 没有全局 MCP 注册表、版本/哈希、来源、健康状态、工具缓存。
- 没有“哪些 CLI 兼容 / 哪些不兼容”的权威矩阵。
- 没有 Agent → MCP 的专用命令（目前只能整份 `agent_roles_save`）。
- 没有 per-session MCP 注入，也没有 per-role MCP 配置过滤。
- 没有工具级 allow/deny、审计、密钥引用、进程生命周期管理。
- 全局配置与用户手改配置的边界未定义，直接写会覆盖用户内容。

---

## 2. 开源实现调研

### 2.1 项目对比

| 项目 | 形态 / 技术栈 | 关键机制 | 可借鉴 | 不直接采用 |
| --- | --- | --- | --- | --- |
| [MCPJungle](https://github.com/mcpjungle/MCPJungle) | 自托管 MCP gateway，Go，MPL-2.0 | 一个 `/mcp` 端点；server 注册表；`server__tool` 命名空间；Tool Groups 按组暴露；stateless / stateful 连接；per-client token | **Tool Group 映射 Agent**；冷启动用 stateful + idle timeout；统一端点 | 它是中心网关，不是桌面侧配置管理器；MPL 代码不宜直接拼进闭源核心 |
| [MetaMCP](https://github.com/metatool-ai/metamcp) | MCP aggregator / orchestrator，TS，MIT | Namespace 聚合多个 server；Endpoint 绑定 namespace；middleware；tool override；idle session 预热 | **Namespace = Agent 可见集合**；middleware 做过滤/日志；预热降低冷启动 | 需要 Docker/Postgres；偏服务端多租户 |
| [MCPHub（samanhappy）](https://github.com/samanhappy/mcphub) | 自托管控制面，TS，Apache-2.0 | server/group 路由；per-user credentials；隔离 stdio runtime；日志、健康检查、OAuth | **per-Agent 凭据 + 隔离 runtime**；健康/日志；group 可见性 | 体量大；Web 控制面不是桌面应用需要 |
| [MCP Hub（ravitemer）](https://github.com/ravitemer/mcp-hub) | 本地 MCP 协调器，TS，MIT | start/stop/enable/disable；健康监控、自动重连；配置热更新；统一 `/mcp` | **进程生命周期与健康恢复**；配置热更新；SSE 事件 | 单端点聚合，不做 Agent 权限 |
| [ContextForge](https://github.com/IBM/mcp-context-forge) | 企业级 registry + proxy，Python，Apache-2.0 | 联邦 MCP/REST/gRPC；Virtual Server 组合工具；auth/RBAC/限流/OTel；Admin UI | **Virtual Server** 作为“按 Agent/团队暴露的工具集”；策略、审计、可观测性 | 过重；Python 服务端；不适合直接嵌桌面 sidecar |
| [ToolHive](https://github.com/stacklok/toolhive) | MCP 平台 / 容器运行时，Go，Apache-2.0 | 每个 server 跑容器；registry + gateway + K8s operator；OIDC、审计、工具过滤 | **隔离与最小权限**；容器化 stdio server；registry 治理 | 需要 Docker/K8s；v1 不引入容器依赖 |
| [Docker MCP Gateway](https://github.com/docker/mcp-gateway) | Docker MCP 插件 / gateway，Go，MIT | Profile 组合 server；tool allowlist；client connect；secrets/OAuth；容器运行 | **Profile = Agent/角色**；工具 allowlist；客户端连接管理；密钥管理 | 依赖 Docker Desktop；不适合作为唯一运行时 |
| [mcpm](https://github.com/pathintegral-institute/mcpm.sh) | MCP 管理器 CLI，Python，MIT | 全局安装一次；Profile 标签；client config 集成；server 更新；直接运行 stdio/HTTP | **最接近 skill 的“全局库 + profile + client link”模型** | 没有运行时网关/权限；Python CLI 不适合嵌入 Rust 核心 |
| [MCP Router](https://github.com/mcp-router/mcp-router) | 桌面 MCP 管理器，TS，Sustainable Use License | Projects / Workspaces；server 与 tool 开关；请求日志；一键连接客户端 | **桌面 UX**：项目/工作区、工具开关、日志 | 2026-09-18 已 EOL；许可证不是 OSI；不采用 |
| [官方 MCP Registry](https://github.com/modelcontextprotocol/registry) | 官方 server registry，Go | 发布/发现 server；`server.json` 元数据；命名空间校验 | 后续市场/来源校验；`server.json` 字段 | 只做发现，不做本地调度 |

### 2.2 从开源实现提炼出的模式

**模式 A：配置管理器（mcpm、skill 式）**

- 全局安装一次，生成各 CLI 的配置片段。
- 优点：简单、贴近现有 CLI；不需要新进程。
- 缺点：无法真正按 Agent 收窄；无法工具级过滤；配置漂移和覆盖风险高。

**模式 B：统一网关（MCPJungle、MetaMCP、MCPHub、ContextForge）**

- 所有 server 由网关持有，客户端只连一个端点；网关做命名空间、鉴权、工具过滤、健康、日志。
- 优点：权限和生命周期集中；多客户端复用；可审计。
- 缺点：多一层进程/端口；stdio 需要桥接；客户端仍要配置网关地址；冷启动和故障隔离要自己做。

**模式 C：容器/沙箱运行时（ToolHive、Docker Gateway）**

- 每个 server 隔离运行，最小权限，镜像/密钥/网络策略。
- 优点：安全边界最好。
- 缺点：重；桌面 v1 不应强制 Docker。

**模式 D：ACP 会话注入（本项目最相关的空白）**

- 客户端在 `session/new` 里直接声明本次会话可用的 MCP server。
- 优点：天然 per-Agent；不需要改全局配置；不需要网关；stdio/http/sse 都能表达。
- 缺点：依赖 adapter 实现；如果 CLI 同时读全局配置，可能“注入的 + 全局的”合并，需要实测。

### 2.3 对本项目的结论

**不要一开始就做完整网关。** 先验证 ACP 注入；如果 Claude / Codex / OpenCode 这三个主力 CLI 能按 `session/new.mcp_servers` 工作，v1 就能用很低成本实现“按 Agent 授权”。网关只在两种情况下才值得做：

1. 关键 CLI 不支持 per-session MCP，且无法用 per-role home 隔离；
2. 需要工具级 allow/deny、统一审计、跨 Agent 复用长连接/凭据。

---

## 3. Skill 与 MCP 的关键差异

| 维度 | Skill | MCP |
| --- | --- | --- |
| 内容形态 | Markdown + 资源文件 | 进程（stdio）或远端服务（HTTP/SSE） |
| 发现方式 | 文件系统目录 | 协议握手 + `tools/list` |
| 会话隔离 | 软链到 session home | `session/new.mcpServers` 或 per-role 配置文件 |
| 生命周期 | 安装/更新/删除文件 | 启动/停止/重启/健康检查/空闲回收 |
| 密钥 | 一般没有 | env / headers / OAuth / Keychain |
| 权限粒度 | 技能级（文件） | server 级，最好到 tool 级 |
| 兼容性 | 目录约定 | transport + adapter 能力 + 配置格式 |
| 失败模式 | 文件缺失/损坏 | 启动失败、握手失败、工具调用失败、超时 |

因此 MCP 层不能只是“skill-layer 加一个 MCP 表”。它需要独立的 **registry + link ledger + 兼容矩阵 + 可选的 runtime/gateway**。

---

## 4. 四种接入方案与取舍

### 方案 A：只写各 CLI 配置（skill 式）

```
mcp-layer registry
  └── 写 ~/.claude.json / ~/.codex/config.toml / opencode.json ...
```

- 全局默认：所有兼容 CLI 都写入。
- Agent 级：写 per-role home 的配置（仅对有 overlay 的引擎）。
- 优点：实现最快；和现有 CLI 行为一致。
- 缺点：没有 overlay 的 CLI 无法按 Agent 隔离；配置合并/覆盖风险；没有工具级权限；没有生命周期。

### 方案 B：ACP `session/new.mcpServers` 注入

```
cli-layer acp_connect
  └── NewSessionRequest { cwd, mcp_servers: [选中 MCP] }
```

- 优点：per-Agent 精确；不需要全局配置；stdio/http/sse 都能表达；最符合“Agent 使用权”。
- 缺点：必须实测 adapter；如果 adapter 还读全局配置，可能合并出多余 server；需要处理 command 绝对路径、env、密钥。
- 适用：所有 ACP 引擎，尤其是没有 per-role home 的引擎。

### 方案 C：中心 MCP gateway

```
CLI / Agent → crewchat-mcp gateway (per-Agent endpoint/token)
                  ├── server A
                  ├── server B
                  └── tool filter / audit / health
```

- 优点：强制权限；工具级过滤；进程复用；审计；健康检查。
- 缺点：最复杂；stdio 桥接；端口/认证；冷启动；故障隔离；客户端仍需配置网关。
- 适用：关键 CLI 不支持注入/隔离；或需要 tool-level 权限与审计。

### 方案 D：混合（推荐）

1. **全局可用性**：`mcp-layer` 注册表 + 兼容矩阵 + 各 CLI 全局链接（默认所有兼容 CLI）。
2. **Agent 授权**：优先用 ACP `session/new.mcpServers` 注入；对已验证支持 per-role home 的引擎，用 per-role 配置作为回退。
3. **终端裸跑**：读全局 CLI 配置，看到所有兼容 MCP；这是“终端 fallback”，不是 Agent 路径。
4. **不兼容的 CLI**：UI 明确标 `unsupported`；只保留终端全局链接，不承诺 Agent 级隔离。
5. **网关后置**：只有 P0 证明注入不可行，或需要 tool-level 权限/审计时，再做最小 gateway。

---

## 5. 推荐的数据模型与真相源

### 5.1 目录

```
~/.agents/mcp/
  servers/<id>.json          # 规范化 server 定义（可读、可手改；不含明文密钥）
  registry.sqlite            # 元数据、链接 ledger、工具缓存、健康、来源
  runtime/                   # 可选：pid / socket / 日志 / 临时配置
  trash/                     # 删除后的定义/版本
```

- `servers/<id>.json` 是**唯一定义**；各 CLI 配置只是生成/链接结果。
- `registry.sqlite` 不复制定义正文，只存索引、哈希、状态。
- 可选生成 `~/.agents/mcp.json` 作为兼容导出，但不能成为第二个可写入口。
- Agent 绑定仍放 `cli-layer.db` 的 `agent_roles.mcp_json`；`mcp-layer` 不感知 Agent。

### 5.2 Server 定义

```json
{
  "id": "playwright",
  "name": "Playwright",
  "transport": "stdio",
  "command": "npx",
  "args": ["-y", "@playwright/mcp@latest"],
  "env": {
    "API_KEY": { "keychain": "com.crewchat.mcp-layer/playwright/api-key" }
  },
  "cwd": null,
  "url": null,
  "headers": {},
  "enabled": true,
  "compatibility": {
    "unsupported": ["openclaw"]
  },
  "tools": {
    "allow": [],
    "deny": []
  },
  "source": {
    "kind": "manual",
    "url": null
  }
}
```

原则：

- **密钥不落 JSON**：env / headers 里的 secret 用 Keychain 引用；导入的明文配置迁移到 Keychain，迁移前标 `containsSecret`。
- **命令不经过 shell**：`command` 与 `args` 直接 exec；禁止 `sh -c` 拼接。
- **stdio command 用绝对路径**（ACP schema 要求绝对路径）；`npx` / `uvx` 解析后存绝对路径或明确记录依赖 PATH。
- **不覆盖用户条目**：每个 CLI 配置里的托管条目要有 ledger；只改 `managed=1` 的 id；同名 foreign 条目标冲突，不静默覆盖。

### 5.3 链接 ledger

`registry.sqlite` 至少需要：

```sql
servers(id, name, transport, hash, enabled, source_kind, source_url, updated_at)
links(server_id, agent_type, path, state, managed, linked_at)
tools(server_id, tool_name, schema_hash, enabled, last_seen)
health(server_id, state, checked_at, latency_ms, error)
```

- `links.state`: `linked` / `unsupported` / `foreign` / `broken` / `disabled`。
- `links.managed`: 只对我们写入的条目负责；用户手改的条目永不被删除。
- `tools` 用于 UI 展示与工具级 allow/deny；v1 可只缓存 `tools/list`。

---

## 6. 兼容性矩阵（待 P0 实测）

`supports_mcp` 是当前 `cli-layer` 的引擎标志，不等于“支持按 Agent 隔离”。最后一列是建议的 v1 结论。

| 引擎 | `supports_mcp` | per-role home | ACP `mcp_servers` | 配置来源（参考 codeg） | v1 建议 |
| --- | --- | --- | --- | --- | --- |
| `claude_code` | ✅ | ✅ Claude overlay | 待实测（P0 重点） | `~/.claude.json` `mcpServers` + `~/.claude/settings.json` | 注入通过则 Agent 级支持；否则 per-role 配置 |
| `codex` | ✅ | ✅ Codex overlay | 待实测（P0 重点） | `~/.codex/config.toml` `[mcp_servers.*]`；仅 stdio + streamable HTTP | 注入通过则 Agent 级支持；注意 SSE 不支持 |
| `open_code` | ✅ | ✅ OpenCode overlay | 待实测（P0 重点） | `opencode.json` | 注入通过则 Agent 级支持 |
| `gemini` | ✅ | ❌ `OverlayKind::None` | 待实测 | `~/.gemini/settings.json` | 仅注入通过才支持 Agent 级；否则全局终端 only |
| `openclaw` | ❌ | ✅ OpenClaw bridge | 待实测 | `~/.openclaw/openclaw.json` | 暂不支持，先从 Agent 选择器排除 |
| `cline` | ✅ | ✅ Cline overlay | 待实测 | `~/.cline/data/settings/cline_mcp_settings.json` | P1 验证；配置回退可用 |
| `hermes` | ✅ | ✅ Hermes overlay | 待实测 | 见 codeg 适配器 | P1 验证 |
| `codebuddy` | ✅ | ❌ `OverlayKind::None` | 待实测 | `~/.codebuddy.json` / `~/.codebuddy/settings.json` | 仅注入通过才支持 Agent 级 |
| `kimi_code` | ✅ | ✅ Kimi overlay | 待实测 | 见 codeg 适配器 | P1 验证 |
| `pi` | ✅ | ✅ Pi overlay | 待实测 | 见 codeg 适配器 | P1 验证 |
| `grok` | ✅ | ✅ Grok overlay | 待实测 | `~/.grok/config.toml` | P1 验证 |
| `cursor` | ✅ | ❌ `OverlayKind::None` | 待实测 | `~/.cursor/mcp.json` | 仅注入通过才支持 Agent 级 |
| `deepseek` | ✅ | ✅ DeepSeek overlay | 待实测 | 见 codeg 适配器 | P1 验证 |
| `qoder` | ✅ | ❌ `OverlayKind::None` | 待实测 | `~/.qoder/settings.json` | 仅注入通过才支持 Agent 级 |
| `antigravity` | ✅ | ❌ `OverlayKind::None` | 待实测 | 共享 `mcp_config.json` | 仅注入通过才支持 Agent 级 |

**矩阵的用法：**

- “全局默认链接”只看 transport / 配置格式兼容，不看 per-role home。
- “Agent 级授权”必须同时满足：能按会话注入，或能写 per-role home 且 CLI 不合并全局配置。
- 不满足的 CLI 在 Agents 页显示“该引擎不支持按 Agent 隔离 MCP”，不要假装支持。

---

## 7. 调度与生命周期

### 7.1 先区分三种“调度”

1. **链接调度**：一个 MCP 应该写入哪些 CLI、哪些会话。
2. **进程调度**：stdio server 由谁启动、何时回收、失败如何重启。
3. **工具调度**：一个会话能看到哪些 tool，调用如何路由/审计。

v1 建议只做 1，2 交给 CLI 原生行为，3 只做 server 级；不要一开始就做中心 runtime。

### 7.2 生命周期模式

| 模式 | 谁启动 stdio server | 优点 | 缺点 |
| --- | --- | --- | --- |
| `external`（v1 默认） | CLI / ACP adapter | 简单、贴近原生；不引入新进程 | 每会话可能重复启动；无法集中健康/审计 |
| `supervised`（P2） | `mcp-layer` | 复用、健康、日志、统一 env | 需要 stdio↔HTTP 桥接；客户端要支持远端 MCP |
| `remote` | 远端服务 | 无本地进程 | 需要 OAuth/网络/超时/重试 |

### 7.3 如果做 supervisor/gateway，参考这些规则

- **状态机**：`stopped → starting → ready → degraded → failed`；启动超时、握手失败、`tools/list` 失败分别有错误码。
- **冷启动**：MCPJungle 的 stateless 默认 + stateful 可选；MetaMCP 的 idle session 预热。v1 不需要，P2 再按需。
- **空闲回收**：按 server 配置 `idle_timeout`；有活跃调用时不回收。
- **并发**：只读/无凭据 server 可共享；带 per-Agent env/凭据的 stdio server 要按凭据作用域隔离（MCPHub 的 per-user runtime 思路）。
- **失败隔离**：单个 server 失败不阻塞其他 server；会话继续，UI 显示 degraded；是否 fail-closed 由产品决定。
- **工具命名**：`server__tool` 命名空间，避免冲突；工具列表变化发事件，UI 刷新。
- **审计**：记录 server、tool、耗时、结果状态；参数默认不落库（可能含密钥）。

### 7.4 进程归属建议

- v1：**CLI 拥有 stdio 进程**，`mcp-layer` 只做 registry/链接/健康探测/工具缓存。这样不会改变现有 CLI 行为，风险最低。
- P2：如果要做 gateway，再引入 `mcp-layer` 的 supervisor；此时才需要处理端口、认证、stdio 桥接、空闲回收。

---

## 8. API 与 UI 影响

### 8.1 建议命令

保留现有命令名与 wire 形状，新增能力用可选字段，避免 AgentOS 大改：

| 命令 | 作用 | 备注 |
| --- | --- | --- |
| `mcp_scan_local` | 扫描/导入本机各 CLI 的 MCP | 返回 `servers` + `warnings`；新增 `transport` / `compatibleApps` / `unsupportedApps` / `health` 可选字段 |
| `mcp_list` | 全局 MCP 库 | 新增；返回规范化 server + link 状态 |
| `mcp_read` | 单个 server 详情 | 新增；spec + tools + health |
| `mcp_upsert_local_server` | 新建/更新 | 保留 `{serverId,spec,apps}`；`apps` 可省略 = 所有兼容 CLI |
| `mcp_remove_server` | 删除 | 保留；只删托管条目，保留 foreign |
| `mcp_link_status` | 全局链接状态 | 新增；可按 `agentType` 过滤 |
| `mcp_sync_links` | 同步全局链接 | 新增；默认所有兼容 CLI |
| `agent_mcps_set` | 保存 Agent MCP 绑定 | 新增；`{roleId, serverIds}` |
| `agent_mcp_status` | Agent 可用性 | 新增；`{servers:[{id,name,available,compatible,health}], missing, unsupported}` |

`agent_roles_save` 已经能存 `mcp`，但建议新增 `agent_mcps_set`，避免前端为了改 MCP 而整份覆盖角色。

### 8.2 MCP 页

- 列表：全局 server、transport、来源、健康、工具数、链接的 CLI。
- 详情：spec、env/header 的 secret 引用（不显示明文）、工具列表、链接状态、冲突。
- “链接的 CLI”：默认展示所有兼容 CLI；不兼容置灰并说明原因；高级模式允许按 CLI 关闭。
- 不要再用“手动勾选 15 个 CLI”作为主交互；默认全兼容，例外才手动改。
- 删除/更新：只处理托管条目；foreign 条目提示“这是用户自己写的，不覆盖”。

### 8.3 Agents 页

- MCP 多选：显示名称、transport、健康、是否与当前引擎兼容。
- 当前引擎不兼容的 server 标黄/红，不允许静默保存成“可用”。
- 保存走 `agent_mcps_set`；显示“已选 N 个 / 不可用 M 个”。
- 空选择语义必须在 UI 写清楚（见 §10）。

---

## 9. 分阶段计划

### P0：调研 spike（不写产品代码）

1. 用 Claude / Codex / OpenCode 三个 ACP adapter 实测 `session/new.mcpServers`：
   - stdio 一个（如 `npx -y @modelcontextprotocol/server-everything`）；
   - HTTP 一个；
   - 观察是否只拿到注入的 server，是否仍合并全局配置。
2. 实测 per-role home 是否真的隔离全局 MCP：
   - Claude `CLAUDE_CONFIG_DIR`；
   - Codex `CODEX_HOME`；
   - OpenCode `OPENCODE_CONFIG`。
3. 实测无 overlay 的 Gemini / Cursor 等是否支持注入；不支持就标 `unsupported`。
4. 产出最终兼容矩阵和“v1 支持引擎列表”。

### P1：核心闭环

- `mcp-layer` daemon：registry、`servers/<id>.json`、scan/import、link ledger、健康/工具缓存。
- `cli-layer` 代理：`mcp_*` + `agent_mcps_set` / `agent_mcp_status`。
- 全局链接：所有兼容 CLI 默认同步；foreign 保留。
- Agent 注入：对 P0 通过的引擎用 `session/new.mcpServers`；对 per-role home 引擎用配置回退。
- UI：MCP 页 + Agents 页 MCP 选择器；不兼容显式提示。

### P2：运行时增强

- 健康检查、工具列表缓存/刷新、错误态。
- 工具级 allow/deny（先做 server 级，后做 tool 级）。
- 可选 supervisor（stdio 生命周期/空闲回收）。
- 审计日志（脱敏）。

### P3：网关/市场/安全

- 最小 gateway：per-Agent endpoint/token、工具过滤、审计；只在 P1 证明必要时做。
- 官方 MCP Registry / 市场搜索与安装。
- Keychain 密钥引用、OAuth、导入明文迁移。
- 容器/沙箱隔离（ToolHive 思路）作为可选高级模式。

---

## 10. 待决策问题

1. **Agent 空选择语义**：`mcp=[]` 是“没有任何 MCP”，还是“继承全部”？
   - 现状 codeg 旧逻辑是空 = 继承全部。
   - 建议新增 `mcp_mode: "all" | "selected"`：迁移旧角色为 `all`，新建角色默认 `selected` 且空 = 无；UI 明确显示“全部可用 / 仅选中的 / 无”。
2. **ACP 注入是否被三个主力 adapter 真正支持**：P0 必须先回答；不能假设。
3. **全局配置与注入的合并行为**：如果 CLI 同时读全局配置，Agent 会拿到多余 server；需要 per-role home 或禁用全局。
4. **全局链接的 ownership**：同名 foreign 条目怎么处理？建议不覆盖、标冲突、让用户选择。
5. **密钥**：MCP env/headers 里的 token 是否全部迁 Keychain？导入明文配置是否强制迁移？
6. **是否 v1 就做 gateway**：建议不做；只有 P0 证明注入不可行或需要工具级权限时再做。
7. **stdio 进程归属**：v1 CLI 拥有；P2 是否引入 supervisor？
8. **工具级权限**：v1 只到 server 级，还是直接做 tool allow/deny？
9. **远程 MCP 的 OAuth**：v1 只支持静态 header/token，OAuth 后置？
10. **市场来源**：官方 MCP Registry、Smithery、Docker Catalog 先接哪个？

---

## 11. 参考

- ACP 协议（session/new / MCP servers）：https://agentclientprotocol.com/protocol/session-setup#mcp-servers
- MCP 规范：https://modelcontextprotocol.io/specification
- 官方 MCP Registry：https://github.com/modelcontextprotocol/registry
- MCPJungle：https://github.com/mcpjungle/MCPJungle
- MetaMCP：https://github.com/metatool-ai/metamcp
- MCPHub（samanhappy）：https://github.com/samanhappy/mcphub
- MCP Hub（ravitemer）：https://github.com/ravitemer/mcp-hub
- ContextForge：https://github.com/IBM/mcp-context-forge
- ToolHive：https://github.com/stacklok/toolhive
- Docker MCP Gateway：https://github.com/docker/mcp-gateway
- mcpm：https://github.com/pathintegral-institute/mcpm.sh
- MCP Router（已 EOL，仅 UX 参考）：https://github.com/mcp-router/mcp-router
