# MCP 层：完整网关 + 按 Agent 授权

Updated: 2026-10-23
Status: In Progress（后端控制面已落地；网关 sidecar 构建待网络恢复）
关系：`cli-layer`（ACP 宿主 / 花名册）、`mcp-layer`（薄控制面）、`AgentOS`（MCP / Agents 页）、`model-layer`（Keychain 机制可复用）。
参考：`AGENTS/agent-model.md`、`AGENTS/truth-sources.md`、`AgentOS/engine-api.md`、`cli-layer/src/http.rs`、`cli-layer/src/engines.rs`、`cli-layer/src/acp/host.rs`。

---

## 0. 结论（先看）

1. **最终能力是完整网关，不是“给每个 CLI 各写一份 MCP 配置”。**
   所有 MCP server 注册到网关；CLI 只连网关。Agent 选择哪些 MCP，由网关按 Agent token / allowlist 收窄。
2. **不自己造网关。** 直接复用开源实现 **MCPJungle**（Go，单二进制，SQLite/Postgres，stdio/HTTP/SSE，Tool Groups，per-client token）。以 vendor/fork 的方式固定版本，作为独立 sidecar 运行。
3. **`mcp-layer` 只做薄控制面**：规范化注册表、把配置同步给 MCPJungle、为每个 Agent 创建 client token / allowlist、把网关地址注入 ACP 会话、给前端提供 API。**不做 tool 聚合、协议转发、进程管理。**
4. **每个 Agent 一个网关身份**：Agent 的 `mcps` 是显式 allowlist；空 = 没有 MCP，不继承全部。两个 Agent 即使使用同一个 CLI，也拿不同的 token / allowlist，天然隔离。
5. **会话注入优先走 ACP `session/new.mcpServers`**：
   - CLI 支持 HTTP MCP：直接注入网关的 HTTP endpoint + token。
   - CLI 只支持 stdio：注入 `mcp-remote`（开源）作为桥，连网关。
   - CLI adapter 不消费 `mcpServers`：标为不支持 Agent 级 MCP，不做全局配置兜底。
6. **不做旧数据兼容。** 不迁移 `~/.agents/mcp.json`、不扫描/改写旧 CLI 配置；新库从零开始。旧配置最多作为用户手工参考，不进入产品路径。
7. **保证边界**：对“支持矩阵内 + ACP 注入通过 + MCP 本身兼容”的组合，保证 Agent 选中的 MCP 可用；不满足的组合显式标 `unsupported`，不静默、不假装支持。

一句话：**MCP 的最终形态是“一个本地网关 + 每个 Agent 一张门禁卡”，不是“每个 CLI 一套 MCP 配置”。**

---

## 1. 为什么最终是网关

### 1.1 配置式方案的天花板

如果只做“全局库 + 给每个 CLI 写配置”：

- 每个 CLI 的配置格式、路径、合并规则都不同（15 个引擎，JSON/TOML/嵌套 key 各不相同）。
- 无法保证 Agent 级隔离：CLI 可能把全局配置和项目配置合并。
- 无法做工具级 allow/deny、审计、健康检查、统一日志。
- 每接一个 CLI 就要维护一套配置适配器，长期成本高。
- “所有 CLI 默认启用”会变成“每个 CLI 都要单独验证一遍 MCP 配置”，不可控。

### 1.2 网关带来的确定性

网关把差异收敛到一处：

- **MCP server 只注册一次**：stdio / HTTP / SSE 由网关统一持有。
- **CLI 只认一个 MCP endpoint**：不再关心每个 server 的 command/args/env/url。
- **Agent 权限由 token + allowlist 决定**：不是靠“CLI 有没有读到某个文件”。
- **工具聚合、命名空间、健康、日志、OAuth、冷启动**都由网关处理。
- **同一 CLI 的多个 Agent 并发**：每个会话拿不同 token，互不串味。

---

## 2. 最终架构

```
AgentOS / Agents 页
  │  选择 Agent 的 MCP（server ids）
  ▼
cli-layer（花名册 + ACP host）
  │  session/new.mcpServers
  ▼
mcp-layer（Rust，薄控制面）
  ├── ~/.agents/mcp/servers/<id>.json      # 规范化定义（唯一可写入口）
  ├── ~/.agents/mcp/registry.sqlite        # 元数据 / token 引用 / 健康 / 工具缓存
  ├── 启动、健康检查、reconcile
  └── 调 MCPJungle API
        │
        ▼
MCPJungle sidecar（Go，复用上游）
  ├── /mcp                                # 主网关
  ├── /v0/groups/<agent>/mcp              # 可选：按 Agent 的工具组
  ├── per-client token + allowlist
  ├── stdio / streamable HTTP / SSE upstream
  └── SQLite/Postgres 运行时库（可重建）
        │
        ▼
Claude / Codex / OpenCode / ...（ACP adapter）
```

### 2.1 组件职责

| 组件 | 职责 | 不做什么 |
| --- | --- | --- |
| `mcp-layer` | 规范化 server 定义；同步到 MCPJungle；为 Agent 建 token/allowlist；启动/健康检查 sidecar；给 cli-layer/AgentOS 提供 API；记录映射与审计 | 不实现 MCP 协议、不做 tool 路由、不自己管 stdio 进程 |
| MCPJungle | MCP gateway：注册 server、聚合 tools/prompts/resources、per-client token、Tool Groups、stdio/HTTP/SSE、会话模式、指标 | 不知道 CrewChat 的 Agent/CLI 语义；不直接面向 AgentOS |
| `cli-layer` | 花名册、`agent_mcps_set`、ACP `session/new` 注入、会话生命周期 | 不拼 MCP 配置、不解析 MCP 协议 |
| `AgentOS` | MCP 库 / Agent 选择器 / 健康与错误展示 | 不直接连 MCPJungle、不写配置 |

### 2.2 为什么把 MCPJungle 放在独立进程

- **许可证边界**：MCPJungle 是 MPL-2.0（文件级 copyleft）。独立进程 + vendor 目录可以清楚隔离；不把 MPL 代码翻译进闭源 Rust 文件。
- **故障隔离**：网关崩溃不拖垮 cli-layer；可单独重启。
- **升级简单**：固定上游 tag/commit；升级只替换 sidecar。
- **复用最大化**：tool 聚合、transport、token、group、健康、指标全部直接用上游，不重写。

### 2.3 “所有 CLI 默认启用”怎么落

- **不是**把每个 MCP 分别写进每个 CLI 的配置；那是旧方案。
- **是**所有支持矩阵内的 CLI 默认拿到同一个网关入口；新增 MCP 只在网关注册一次，CLI 不需要改配置。
- Agent 会话由 `cli-layer` 在 `session/new` 注入网关 endpoint + 该 Agent 的 token。
- 终端裸跑是可选 fallback：给每个 CLI 配一个 “terminal” gateway client（all servers），不参与 Agent 会话。
- 因此“所有 CLI 默认启用”与“Agent 只拿到选中的 MCP”不矛盾：CLI 只连网关，权限在 token。

---

## 3. 复用决策：为什么是 MCPJungle

### 3.1 候选对比

| 项目 | 技术栈 / 许可 | 能力 | 结论 |
| --- | --- | --- | --- |
| [MCPJungle](https://github.com/mcpjungle/MCPJungle) | Go，MPL-2.0 | 单二进制；SQLite/Postgres；stdio/HTTP/SSE；Tool Groups；per-client token + allowlist；stateless/stateful；指标 | **首选**；最贴合“本地网关 + Agent 门禁” |
| [MetaMCP](https://github.com/metatool-ai/metamcp) | TS，MIT | Namespace / Endpoint / middleware；idle session | 备选；MIT 更宽松，但 Node + Postgres，桌面打包重 |
| [MCPHub（samanhappy）](https://github.com/samanhappy/mcphub) | TS，Apache-2.0 | 控制面；group；per-user credentials；健康/日志 | 备选；功能全但体量大，Web 控制面偏服务端 |
| [MCP Hub（ravitemer）](https://github.com/ravitemer/mcp-hub) | TS，MIT | 生命周期、健康、热更新、单端点 | 适合做进程管理参考；缺少 per-client allowlist |
| [ContextForge](https://github.com/IBM/mcp-context-forge) | Python，Apache-2.0 | 联邦、Virtual Server、RBAC、限流、OTel | 过重；Python 服务端，不适合桌面 sidecar |
| [ToolHive](https://github.com/stacklok/toolhive) | Go，Apache-2.0 | 容器隔离、registry、gateway、K8s | 安全隔离参考；v1 不强制 Docker |
| [Docker MCP Gateway](https://github.com/docker/mcp-gateway) | Go，MIT | Profile、tool allowlist、容器、secrets | 依赖 Docker；不适合作为唯一运行时 |
| [mcpm](https://github.com/pathintegral-institute/mcpm.sh) | Python，MIT | 全局安装、profile、client config | 只有配置管理，没有网关；不满足最终形态 |
| [MCP Router](https://github.com/mcp-router/mcp-router) | TS，Sustainable Use | 桌面 UX、项目/工作区、tool 开关 | 2026-09-18 EOL；许可证非 OSI；只做 UX 参考 |
| [官方 MCP Registry](https://github.com/modelcontextprotocol/registry) | Go | server 发现/发布 | 后续市场来源；不是运行时 |

### 3.2 采用方式：vendor / fork，不重写

建议：

1. 在仓库放 `vendor/mcpjungle/`（或 `mcp-layer/upstream/mcpjungle/`），固定 tag/commit。
2. 保留上游 `LICENSE`、`NOTICE`、来源说明；对 MPL 文件的修改继续按 MPL 发布。
3. 构建成独立二进制 `crewchat-mcp-gateway`，由 `mcp-layer` 拉起。
4. 只通过 MCPJungle 的 HTTP API / CLI 操作，不复制它的内部逻辑到 Rust。
5. 需要补的能力（例如 CrewChat 特有的 Agent 映射）放在 `mcp-layer`，不塞进 MCPJungle。

**“直接复制拷贝”的正确含义**：复制/固定上游源码并复用其能力，而不是把网关重新实现一遍。若团队不接受 MPL-2.0，再评估 MetaMCP（MIT）作为备选；不要先写自研网关。

### 3.3 MCPJungle 的关键能力映射

| 需求 | MCPJungle 能力 |
| --- | --- |
| Agent 只能用选中的 server | enterprise mode 的 per-client token + `allowed_servers` |
| Agent 只能用选中的 tool | Tool Group：`included_tools` / `included_servers` / `excluded_tools` |
| 一个 Agent 一个 endpoint | Tool Group endpoint `/v0/groups/<agent>/mcp`（可选） |
| 同一 CLI 多 Agent 隔离 | 每个 Agent 不同 token；会话注入不同 endpoint/token |
| stdio server | 上游 stdio 支持；网关持有子进程 |
| 远程 server | streamable HTTP / SSE |
| 冷启动 | stateless 默认；stateful + idle timeout 可选 |
| 健康/指标 | 内置健康与 Prometheus/OTel 指标（企业模式） |
| 工具发现 | `tools/list` 聚合；CrewChat 只读缓存给 UI |

已知限制：

- Tool Group 目前只覆盖 tools，不覆盖 prompts/resources；需要 prompts/resources 的 Agent 用主 `/mcp` + per-client allowlist，不用 group。
- 企业模式目前只有 admin 能建 group；`mcp-layer` 本地持有 admin token，用户不直接操作。
- 下游客户端认证是静态 token，不支持 OAuth/SSO；本地桌面够用。
- 上游 OAuth 是 gateway-scoped，不是 per-Agent；需要 per-Agent 凭据的 server 要单独评估。

---

## 4. “任何 Agent 选了 CLI 就保证能调用”怎么定义

不能承诺“任意 CLI 都行”。承诺的是**支持矩阵内**：

```
Agent 选择：CLI X + MCP [A, B]
  → CLI X 在支持矩阵内
  → X 的 ACP adapter 消费 session/new.mcpServers
  → mcp-layer 为 Agent 创建 token（allowlist=[A,B]）
  → 会话注入 gateway endpoint + token（HTTP 或 stdio bridge）
  → 网关只暴露 A、B
  → 保证 A、B 的 tools 可被发现/调用
```

### 4.1 保证条件

1. CLI 支持 MCP，且在支持矩阵内。
2. CLI adapter 真的消费 `session/new.mcpServers`（P0 实测）。
3. 网关健康；Agent 的 token/allowlist 已 reconcile。
4. MCP server 本身能被网关连接（transport/凭据/启动成功）。
5. 如果 MCP 需要 prompts/resources，且使用了 Tool Group，则不在保证范围（group 限制）。

任何一条不满足：

- UI 标 `unsupported` / `unavailable`，并给出原因；
- 不静默回退到全局配置，不让 Agent 误以为自己有某个 MCP；
- 不做“旧数据兼容”或“猜一个配置”。

### 4.2 注入方式

**优先：HTTP**

ACP `McpServer::Http`：

```json
{
  "type": "http",
  "name": "crewchat-mcp",
  "url": "http://127.0.0.1:<port>/mcp",
  "headers": [
    { "name": "Authorization", "value": "Bearer <agent-token>" }
  ]
}
```

适合支持 streamable HTTP 的 adapter。

**兜底：stdio bridge**

ACP 所有 Agent 必须支持 stdio MCP。对只支持 stdio 的 adapter，注入：

```json
{
  "type": "stdio",
  "name": "crewchat-mcp",
  "command": "/absolute/path/to/npx",
  "args": [
    "-y",
    "mcp-remote",
    "http://127.0.0.1:<port>/mcp",
    "--allow-http",
    "--header",
    "Authorization: Bearer <agent-token>"
  ],
  "env": []
}
```

- 复用开源 `mcp-remote`，不自己写桥。
- 需要 Node/npx；应用本来就在用 npx 引擎，可接受。
- 如果不想依赖 Node，再评估 Rust/Python 的现成 bridge；仍不自己写。

**不支持：adapter 不消费 `mcpServers`**

- 标 `unsupported`；不做全局配置文件兜底。
- 这类 CLI 仍可在终端手动连网关，但不属于 Agent 级保证。

---

## 5. Agent 授权模型

### 5.1 绑定

- Agent 的 `mcps` 存 **server id 列表**，在 `cli-layer.db` 的 `agent_roles.mcp_json`。
- **空列表 = 没有 MCP**，不继承全部。不做旧数据兼容，所以不需要 `all` 模式。
- UI 可以提供“全选”按钮，但保存的是显式 id 列表。
- `agent_mcps_set` 专用命令，避免整份角色覆盖。

### 5.2 每个 Agent 一个网关身份

- `mcp-layer` 为每个 Agent 创建一个 MCPJungle client：
  - `allowed_servers = Agent.mcps`
  - token 存 Keychain（service `com.crewchat.mcp-layer`），registry 只存引用。
- 可选：为 Agent 创建 Tool Group（工具级 allow/deny），endpoint 替代主 `/mcp`。
- Agent 修改 MCP 选择：
  - 更新 client allowlist / group；
  - 新会话拿新 token/endpoint；
  - 活跃会话不热改（避免中途工具集变化），重连后生效。

### 5.3 两个 Agent 用同一个 CLI

```
Agent A (Claude) → token A → allowlist [backend]
Agent B (Claude) → token B → allowlist [ios]
```

- 两个 Claude 会话各自 `CLAUDE_CONFIG_DIR`，各自 `session/new.mcpServers`；
- 全局 `~/.claude` 不参与；
- 不存在“先启动的 Agent 改全局配置，后启动的串味”。

### 5.4 工具级权限

- v1 先做 **server 级** allowlist（Agent 选 server）。
- 需要工具级时，用 MCPJungle Tool Group：
  - `included_servers` + `excluded_tools`，或 `included_tools`；
  - group endpoint 只暴露 tools；
  - prompts/resources 仍走主 endpoint + server allowlist。
- 不在 v1 自研工具过滤层。

---

## 6. 数据与真相源

### 6.1 目录

```
~/.agents/mcp/
  servers/<id>.json          # 规范化定义；唯一可写入口；不含明文密钥
  registry.sqlite            # 元数据、Agent token 引用、健康、工具缓存
  runtime/mcpjungle.db       # MCPJungle 运行时库；可删除重建
  logs/                      # sidecar 日志
```

- **`servers/<id>.json` 是唯一定义**。
- `registry.sqlite` 只存索引/映射/状态，不复制定义正文。
- `runtime/mcpjungle.db` 是运行时缓存：启动时由 `mcp-layer` 从规范化定义 reconcile；丢了可以重建。
- Agent 绑定仍只在 `cli-layer.db`；`mcp-layer` 不存 Agent 定义。
- **不读取、不迁移旧 `~/.agents/mcp.json` / 旧 CLI 配置。**

### 6.2 Server 定义

```json
{
  "id": "playwright",
  "name": "Playwright",
  "transport": "stdio",
  "command": "/absolute/path/to/npx",
  "args": ["-y", "@playwright/mcp@latest"],
  "env": {
    "API_KEY": { "keychain": "com.crewchat.mcp-layer/playwright/api-key" }
  },
  "url": null,
  "headers": {},
  "enabled": true,
  "tools": { "allow": [], "deny": [] },
  "source": { "kind": "manual", "url": null }
}
```

- 密钥不落 JSON；env/headers 用 Keychain 引用。
- 命令不经过 shell；stdio command 用绝对路径（ACP 要求）。
- 不覆盖用户手写配置：新库从零开始，不存在旧条目；MCPJungle 运行时库由我们独占。

### 6.3 同步流程

```
mcp-layer 启动
  → 读取 servers/*.json
  → 启动 MCPJungle sidecar（若未运行）
  → 等 /health
  → upsert servers 到 MCPJungle
  → 为每个 Agent reconcile client token / allowlist / group
  → 标记 health/tools
```

- `mcp-layer` 是唯一写入方；MCPJungle dashboard/CLI 不作为用户入口。
- Agent 修改 → 只 reconcile 该 Agent 的 client/group。
- 删除 server → 从 MCPJungle 删除；同时清理引用它的 Agent allowlist（UI 标 missing）。

---

## 7. 支持矩阵（P0 实测后定稿）

不再维护“15 个 CLI × 每种配置格式”的矩阵；只问三个问题：

1. CLI 是否支持 MCP？
2. ACP adapter 是否消费 `session/new.mcpServers`？
3. 能否用 HTTP，或至少用 stdio bridge？

| CLI | MCP | ACP 注入 | 结论 |
| --- | --- | --- | --- |
| `claude_code` | ✅ | 待实测（P0 重点） | 通过则支持 |
| `codex` | ✅ | 待实测（P0 重点） | 通过则支持；注意上游 SSE 限制 |
| `open_code` | ✅ | 待实测（P0 重点） | 通过则支持 |
| `gemini` | ✅ | 待实测 | 通过则支持；否则 unsupported |
| `openclaw` | ❌（当前 `supports_mcp=false`） | — | 暂不支持 |
| `cline` | ✅ | 待实测 | 通过则支持 |
| `hermes` | ✅ | 待实测 | 通过则支持 |
| `codebuddy` | ✅ | 待实测 | 通过则支持 |
| `kimi_code` | ✅ | 待实测 | 通过则支持 |
| `pi` | ✅ | 待实测 | 通过则支持 |
| `grok` | ✅ | 待实测 | 通过则支持 |
| `cursor` | ✅ | 待实测 | 通过则支持 |
| `deepseek` | ✅ | 待实测 | 通过则支持 |
| `qoder` | ✅ | 待实测 | 通过则支持 |
| `antigravity` | ✅ | 待实测 | 通过则支持 |

原则：

- **P0 只验证 Claude / Codex / OpenCode**；这三个通过就先上线。
- 其余 CLI 通过一个接一个加，不阻塞主线。
- 不通过的 CLI 在 Agents 页标“该引擎不支持 Agent 级 MCP”，不做配置兜底。
- “某些 MCP 不支持某些 CLI”由网关/transport/工具组能力决定，单独记录，不强求。

---

## 8. 调度与生命周期

### 8.1 谁管什么

| 事项 | 负责方 |
| --- | --- |
| MCPJungle sidecar 启动/停止/健康 | `mcp-layer` |
| 端口分配、SQLite 路径、admin 初始化 | `mcp-layer` |
| MCP server 注册/删除 | `mcp-layer` → MCPJungle API |
| stdio 子进程、连接、stateless/stateful、idle timeout | MCPJungle |
| per-Agent token / allowlist / Tool Group | `mcp-layer` → MCPJungle API |
| 会话注入 gateway endpoint/token | `cli-layer`（ACP `session/new`） |
| 工具发现缓存、健康展示 | `mcp-layer` 读 MCPJungle |
| 审计/指标 | MCPJungle（企业模式）+ `mcp-layer` 映射 |

### 8.2 启动顺序

```
AgentOS 启动
  → cli-layer 启动
  → mcp-layer 启动
      → 启动 MCPJungle（127.0.0.1:<random/3461>）
      → 等 /health
      → reconcile servers + agents
  → 会话建立时注入
```

- 网关只监听回环，禁止 LAN/公网。
- 端口冲突：由 `mcp-layer` 选空闲端口；MCPJungle 配置写入 runtime。
- 网关崩溃：`mcp-layer` 重启并 reconcile；正在进行的 MCP 调用失败，UI 显示错误。

### 8.3 失败语义

- 网关不可用：Agent 会话可以继续聊天，但 MCP 标 `unavailable`，UI 明确提示；不静默假装可用。
- 单个 upstream MCP 不可用：MCPJungle 标记该 server；其他 server 不受影响；Agent 调用该 server 的 tool 返回错误。
- Agent 选择的 MCP 已删除：会话启动/状态显示 `missing`；不自动替换成别的 server。
- 是否让会话启动直接失败：建议 **不失败**，但 UI 必须显式提示“该 Agent 的 MCP 不可用”；严格 fail-closed 可作为设置项后置。

---

## 9. API 与 UI

### 9.1 命令

旧 `mcp_scan_local` / `mcp_upsert_local_server` / `mcp_remove_server` 不再兼容旧配置语义；前端改用新命令：

| 命令 | 作用 |
| --- | --- |
| `mcp_list` | 全局 MCP 库 + 健康 + 工具数 + 被哪些 Agent 使用 |
| `mcp_read` | 单个 server 详情（spec、tools、health、gateway 状态） |
| `mcp_upsert` | 新建/更新 server（写入 `servers/<id>.json` 并同步网关） |
| `mcp_remove` | 删除 server（同步网关 + 清理引用） |
| `mcp_health` | 网关 / server 健康 |
| `mcp_tools` | 工具列表/刷新 |
| `mcp_sync` | 手动 reconcile（排障用） |
| `agent_mcps_set` | 保存 Agent MCP allowlist |
| `agent_mcp_status` | Agent 选中 MCP 的 available / missing / unsupported / health |

- `mcp_upsert` 的 body 直接是规范化 spec；不再有 `apps` 多选。
- 旧的 `apps`（写入哪些 CLI）概念删除：所有 Agent 都走网关，CLI 差异由支持矩阵处理。

### 9.2 MCP 页

- 全局 server 列表：名称、transport、来源、健康、工具数、使用中的 Agent。
- 详情：spec、工具列表、健康、网关 client/group 状态。
- 新建：手工 JSON / 模板 / 市场（后置）。
- 删除：提示哪些 Agent 在用；删除后这些 Agent 标 missing。
- **不再有“选择写入哪些 CLI”**；只有“这个 MCP 是否启用/可用”。

### 9.3 Agents 页

- MCP 多选：显示 server 名称、transport、健康、是否支持当前 CLI。
- 空 = 没有 MCP；提供“全选”按钮但保存显式列表。
- 当前 CLI 不支持 Agent 级 MCP：置灰并说明。
- 保存走 `agent_mcps_set`；显示“已选 N / 不可用 M”。

---

## 10. 分阶段计划

### P0：方案验证（不写产品代码）

1. 运行 MCPJungle（enterprise mode + SQLite）：
   - 注册一个 stdio server（如 `server-everything`）；
   - 创建 client token + allowlist；
   - 验证 token 只能看到 allowlist 内的 server；
   - 验证 Tool Group endpoint。
2. 用 Claude / Codex / OpenCode 实测 ACP `session/new.mcpServers`：
   - HTTP endpoint + token；
   - stdio `mcp-remote` bridge；
   - 确认没有合并全局配置。
3. 确认 MCPJungle 企业模式在 SQLite 下可用；若必须 Postgres，再决定是否内置/外置。
4. 产出最终支持矩阵；不通过的直接标 unsupported。

### P1：核心闭环

- vendor/fork MCPJungle，构建 sidecar。
- `mcp-layer`：registry、reconcile、sidecar 生命周期、Agent token/allowlist。
- `cli-layer`：`agent_mcps_set` / `agent_mcp_status`，`session/new.mcpServers` 注入。
- AgentOS：MCP 页 + Agents 页 MCP 选择器。
- 只支持 P0 通过的 CLI（先 Claude / Codex / OpenCode）。

### P2：增强

- 更多 CLI 接入。
- Tool Group / 工具级 allow-deny。
- 健康、工具缓存、审计、错误态。
- 终端 fallback：一个 gateway client token + 各 CLI 的 gateway 配置（可选）。
- Token 轮换 / 失效处理。

### P3：市场与安全

- 官方 MCP Registry / 市场搜索安装。
- 上游 OAuth、Keychain 密钥引用。
- 容器/沙箱隔离（ToolHive 思路）作为高级选项。
- 团队/多用户网关（非桌面 v1）。

---

## 11. 待决策问题

1. **MPL-2.0 是否接受**：MCPJungle 是 MPL-2.0。若不可接受，改用 MetaMCP（MIT）还是自研最小网关？建议先接受 MPL，独立进程 + vendor。
2. **MCPJungle 企业模式 + SQLite** 是否满足本地单用户；若不行，是否接受内置 Postgres（不推荐）或换实现。
3. **`mcp-remote` 依赖 Node** 是否可接受；是否需要 Rust/Python 的现成 bridge。
4. **哪些 CLI 的 ACP adapter 真正消费 `mcpServers`**：P0 必须实测。
5. **空 `mcps` = 无 MCP** 是否确认；建议确认，不做继承全部。
6. **网关不可用时是否 fail-closed**：建议先 fail-open + 显式提示，严格模式后置。
7. **Tool Group 不支持 prompts/resources**：哪些 MCP 依赖 prompts/resources？这些 Agent 走主 endpoint + server allowlist。
8. **per-Agent token vs per-session token**：建议 per-Agent；会话级轮换后置。
9. **旧数据**：确认完全不做迁移；旧 `~/.agents/mcp.json` 和 CLI 配置不读取。
10. **市场来源**：官方 MCP Registry 优先，还是 Smithery/Docker Catalog。

---

## 12. 参考

- ACP 协议（session/new / MCP servers）：https://agentclientprotocol.com/protocol/session-setup#mcp-servers
- MCP 规范：https://modelcontextprotocol.io/specification
- MCPJungle：https://github.com/mcpjungle/MCPJungle
- MCPJungle 文档：https://docs.mcpjungle.com
- MCPJungle Tool Groups：https://docs.mcpjungle.com/guides/tool-groups
- MCPJungle Access Control：https://docs.mcpjungle.com/governance/access-control
- MetaMCP：https://github.com/metatool-ai/metamcp
- MCPHub（samanhappy）：https://github.com/samanhappy/mcphub
- MCP Hub（ravitemer）：https://github.com/ravitemer/mcp-hub
- ContextForge：https://github.com/IBM/mcp-context-forge
- ToolHive：https://github.com/stacklok/toolhive
- Docker MCP Gateway：https://github.com/docker/mcp-gateway
- mcpm：https://github.com/pathintegral-institute/mcpm.sh
- mcp-remote：https://github.com/geelen/mcp-remote
- 官方 MCP Registry：https://github.com/modelcontextprotocol/registry
- MCP Router（已 EOL，仅 UX 参考）：https://github.com/mcp-router/mcp-router

---

## 13. 后端核心进展（2026-10-23）

### 已落地

- 新 crate `mcp-layer/`（二进制 `crewchat-mcp`，控制面 `127.0.0.1:3461`）。
- 规范化 server 定义：`~/.agents/mcp/servers/<id>.json`。
- SQLite registry：`~/.agents/mcp/registry.sqlite`（servers / agents）。
- HTTP API：health、server CRUD/启停、Agent allowlist 同步、gateway start/stop/sync。
- Agent 会话配置：返回 ACP 可直接消费的 `mcpServers`；优先 stdio `mcp-remote` 桥，无 npx 时回退 HTTP。
- 每个 Agent 一个网关 client token + allowlist；空 allowlist 删除 client。
- 网关 sidecar 生命周期、初始化、admin token、reconcile。
- 网关源码固定在 `mcp-layer/vendor/gateway/`，构建脚本 `mcp-layer/scripts/build-gateway.sh`；许可证见 `NOTICE.md`。
- 6 个测试通过（registry / spec / Agent session / fake gateway），clippy 通过；daemon health 冒烟通过（gateway 缺失时显式 `unavailable`）。

### 未完成 / 阻塞

- 网关 sidecar 尚未在本机构建成功：`go build` 下载 `modernc.org/sqlite@v1.23.1` 时网络连接被重置。
  错误原文：
  `read "https://proxy.golang.org/modernc.org/sqlite/@v/v1.23.1.zip": read tcp ...: read: connection reset by peer`
  网络恢复后执行 `mcp-layer/scripts/build-gateway.sh`，再做真实 gateway + Agent token 冒烟。
- `cli-layer` 尚未接 `agent_mcps_set` / `session/new.mcpServers`；前端未动。
- 真实 MCP server（stdio/HTTP）尚未通过 sidecar 跑通。

### 下一步

1. 网络恢复后构建 sidecar，跑通“注册 server → 创建 Agent client → token 只能看到 allowlist”。
2. 接 `cli-layer`：Agent 保存 MCP 选择、`acp_connect` 注入 `mcpServers`。
3. 用 Claude / Codex / OpenCode 实测 stdio bridge 与 HTTP 注入。
