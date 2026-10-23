# MCP 层：Rust 网关 + 按 Agent 实例

Updated: 2026-10-23
Status: In Progress（已从 Go 方案切换到 Rust `mcp-proxy`，后端改造中）
关系：`cli-layer`（ACP 宿主 / 花名册）、`mcp-layer`（控制面 + 每 Agent 网关实例）、`AgentOS`（MCP / Agents 页）。

---

## 0. 结论

1. **最终能力是完整本地网关**：所有 MCP server 注册到 `mcp-layer`，CLI 只连网关。
2. **不再使用 Go/MCPJungle**：Go 依赖下载在当前网络下不可用，源码 vendor 已移除。
3. **复用 Rust `mcp-proxy`**（MIT OR Apache-2.0）作为网关内核，不自己重写 MCP 协议、transport、聚合。
4. **每个 Agent 一个 proxy 实例**：只配置该 Agent 选中的 backends，天然隔离 tools / prompts / resources，不依赖动态 token API。
5. **不做旧数据兼容**：旧 CLI MCP 配置不迁移、不读取；新库从 `~/.agents/mcp/servers/<id>.json` 开始。
6. **保证边界**：支持矩阵内 + CLI adapter 消费 `session/new.mcpServers` + proxy 正常启动 = 保证 Agent 选中的 MCP 可用；否则显式 `unsupported`。

一句话：**mcp-layer 管注册表和 Agent 实例；每个 Agent 的网关只看见自己的 MCP。**

---

## 1. 为什么改成每 Agent 一个实例

`mcp-proxy` 支持聚合、stdio/HTTP/WS、per-token tool scopes，但认证配置是静态的，Agent 动态变更需要重建配置。与其在中央网关上做复杂动态 token，不如：

```
mcp-layer
  ├── Agent A → proxy A（只挂 A 的 MCP）→ 127.0.0.1:portA
  ├── Agent B → proxy B（只挂 B 的 MCP）→ 127.0.0.1:portB
  └── Agent C → proxy C（只挂 C 的 MCP）→ 127.0.0.1:portC
```

好处：

- 每个实例只看见自己的 backends，**不存在跨 Agent 工具泄漏**；
- prompts / resources 也随 backend 一起隔离，不受 per-token tool scope 限制；
- Agent 修改只重启自己的实例，不影响其他 Agent；
- 不需要中央网关的动态 client token / allowlist API；
- 不需要 SQLite/Postgres；配置是 TOML，运行时无数据库。

代价：

- 同一 stdio backend 被多个 Agent 使用时可能重复启动；后续可按“相同 backend 集合”合并实例，或 fork `mcp-proxy` 做共享 runtime；
- 每个 Agent 一个 loopback 端口；端口由 `mcp-layer` 分配；
- 进程数随 Agent 数增长；v1 可接受。

---

## 2. 架构

```
AgentOS / cli-layer
  │  Agent 选择 MCP
  ▼
mcp-layer（Rust 控制面，127.0.0.1:3461）
  ├── ~/.agents/mcp/servers/<id>.json
  ├── ~/.agents/mcp/registry.sqlite
  ├── ~/.agents/mcp/runtime/proxies/<agent>/proxy.toml
  └── 管理 proxy 实例
        │
        ▼
mcp-proxy（Rust，每个 Agent 一个）
  ├── 只挂 Agent 选中的 backends
  ├── HTTP endpoint 127.0.0.1:<port>/
  ├── stdio / HTTP / WebSocket upstream
  └── bearer token
        │
        ▼
Claude / Codex / OpenCode / ...（ACP adapter）
```

### 组件职责

| 组件 | 职责 |
| --- | --- |
| `mcp-layer` | 注册表、server 定义、Agent → proxy 配置、端口/进程生命周期、健康、给 cli-layer 返回 `mcpServers` |
| `mcp-proxy` | MCP 协议、transport、聚合、tools/prompts/resources、auth、resilience |
| `cli-layer` | 花名册、`agent_mcps_set`、ACP `session/new.mcpServers` 注入 |
| `AgentOS` | MCP 库 / Agent 选择器 / 错误展示 |

---

## 3. 复用 `mcp-proxy` 的方式

- crate：`mcp-proxy = { version = "0.6", default-features = false }`
- 不启用默认的 OTel / OAuth / OpenAPI / WebSocket / discovery / YAML / Redis / SQLite，保持轻量；
- 只用核心：stdio/HTTP backend、聚合、bearer auth、TOML 配置；
- 在 `mcp-layer` 内嵌 `Proxy`，每个 Agent 一个 `Proxy` + axum listener；不是再起一个外部 Go 进程；
- 后续二次开发：fork `mcp-proxy` 或在其上层补：
  - 共享 backend runtime（多个 Agent 复用同一 stdio server）；
  - 动态实例配置（不重启更新 backends）；
  - tool/prompt/resource 级 Agent 权限；
  - 审计/健康/用量。

已知限制（当前接受）：

- 每个 Agent 一个实例，stdio backend 可能重复；
- 认证配置静态；Agent 变更重启该实例；
- `mcp-proxy` 0.6 的默认 tool scope 主要覆盖 tools；我们用“每 Agent 只挂自己的 backends”绕开；
- 需要验证 Claude / Codex / OpenCode 对 `session/new.mcpServers` 的消费。

---

## 4. 数据与真相源

```
~/.agents/mcp/
  servers/<id>.json          # 唯一 server 定义；不含明文密钥
  registry.sqlite            # servers / agents / 端口 / token 引用 / 健康
  runtime/proxies/<agent>/   # 每 Agent 的 proxy.toml / 日志
```

- `servers/<id>.json` 是唯一定义；`mcp-layer` 是唯一写入方。
- Agent 绑定在 `cli-layer.db` 的 `agent_roles.mcp_json`；`mcp-layer` 只存 Agent → proxy 的运行时映射。
- 旧 CLI MCP 配置不迁移、不读取。
- 密钥后续走 Keychain 引用；当前先允许 spec 中的 env/header，UI 不展示明文。

---

## 5. Agent 会话

`PUT /v1/agents/{id}` body：

```json
{ "serverIds": ["backend-api", "postgres"] }
```

返回：

```json
{
  "agentId": "role-1",
  "clientName": "agent-role-1",
  "serverIds": ["backend-api", "postgres"],
  "endpoint": "http://127.0.0.1:41001/",
  "token": "<agent token>",
  "http": { "type": "http", "name": "mcp-layer", "url": "...", "headers": [...] },
  "stdio": { "type": "stdio", "name": "mcp-layer", "command": "/abs/npx", "args": [...] },
  "mcpServers": [ /* stdio 优先；无 npx 时回退 http */ ]
}
```

- 每个 Agent 的 proxy 只配置选中的 backends。
- 空 `serverIds` → 停止该 Agent 的 proxy，返回空 `mcpServers`。
- 缺失 server → `409 server_missing`。
- CLI 只连该 Agent 的 endpoint；不存在全局共享 MCP endpoint。

---

## 6. 支持矩阵（P0 实测后定稿）

| CLI | MCP | ACP `mcpServers` | 结论 |
| --- | --- | --- | --- |
| `claude_code` | ✅ | 待实测（P0 重点） | 通过则支持 |
| `codex` | ✅ | 待实测（P0 重点） | 通过则支持 |
| `open_code` | ✅ | 待实测（P0 重点） | 通过则支持 |
| `gemini` | ✅ | 待实测 | 通过则支持 |
| `openclaw` | ❌ | — | 暂不支持 |
| 其余 ACP 引擎 | ✅ | 待实测 | 通过则支持 |

不通过的 CLI 标 `unsupported`，不做全局配置兜底。旧数据不迁移。

---

## 7. 生命周期

- `mcp-layer` 为 Agent 分配 loopback 端口，写 `proxy.toml`，构建 `Proxy`，启动 axum listener。
- Agent 修改：停止旧实例，写新配置，启动新实例；活跃会话不热改，重连后生效。
- Server 修改：重启引用该 server 的 Agent 实例；其他实例不动。
- Server 删除：从所有 Agent 选择中移除；重启受影响实例。
- 健康：检查 proxy 端口/HTTP；失败标 `unavailable`。
- 网关只监听 `127.0.0.1`。

---

## 8. API

| 命令 | 作用 |
| --- | --- |
| `GET /v1/health` | 控制面 + proxy 实例状态 |
| `GET /v1/servers` | MCP 库 |
| `GET /v1/servers/{id}` | 单个 MCP |
| `PUT /v1/servers/{id}` | 新建/更新定义 |
| `DELETE /v1/servers/{id}` | 删除并清理 Agent 引用 |
| `POST /v1/servers/{id}/enable` / `disable` | 启停 |
| `GET /v1/agents` | Agent proxy 状态 |
| `GET /v1/agents/{id}` | 单个 Agent |
| `PUT /v1/agents/{id}` | 设置 allowlist，启动/重启 proxy，返回会话配置 |
| `DELETE /v1/agents/{id}` | 停止 proxy，删除映射 |
| `GET /v1/gateway` | 所有 proxy 实例状态 |
| `POST /v1/gateway/sync` | 重启所有实例 |

旧 Go 网关的 `/v1/gateway/start|stop` 删除。

---

## 9. 分阶段

### P0：验证

- `mcp-proxy` 最小配置（一个 stdio backend）能启动、列工具；
- 两个 Agent 实例 A/B 只看到各自 backend；
- Claude / Codex / OpenCode 的 ACP `session/new.mcpServers` 能消费 HTTP 或 stdio bridge；
- 确认 prompts/resources 随实例隔离。

### P1：核心

- `mcp-layer` 注册表 / server CRUD / Agent proxy 管理；
- `cli-layer` 的 `agent_mcps_set` / `session/new.mcpServers` 接线；
- AgentOS MCP / Agents 页。

### P2：优化

- 按相同 backend 集合合并实例；
- fork `mcp-proxy` 做共享 runtime / 动态配置；
- 健康、日志、审计、Keychain；
- 工具级 allow/deny。

### P3：市场与安全

- 官方 MCP Registry / 市场；
- OAuth、容器/沙箱、团队多用户。

---

## 10. 待确认

1. `mcp-proxy` 0.6 的 `Proxy` 在 axum 内嵌时的 shutdown/backend 清理行为。
2. 每个 Agent 一个实例的进程数是否可接受；何时合并。
3. ACP adapter 对 `session/new.mcpServers` 的支持矩阵。
4. prompts/resources 是否在 proxy 实例内完整隔离（预期是）。
5. 后续 fork `mcp-proxy` 的维护策略和上游同步。

---

## 11. 参考

- `mcp-proxy`：https://github.com/joshrotenberg/mcp-proxy
- `mcp-proxy` docs：https://docs.rs/mcp-proxy
- ACP session/new：https://agentclientprotocol.com/protocol/session-setup#mcp-servers
- MCP 规范：https://modelcontextprotocol.io/specification

---

## 12. 后端核心进展（2026-10-23）

### 已落地

- 删除 Go gateway vendor 与构建脚本，改用 `mcp-proxy = 0.6`（`default-features = false`）。
- `mcp-layer` 控制面：
  - `~/.agents/mcp/servers/<id>.json` 规范化定义；
  - SQLite registry（servers / agents / 端口）；
  - server CRUD、启停、Agent allowlist；
  - 每个 Agent 动态分配 loopback 端口，生成 `proxy.toml`，启动/停止 `mcp-proxy` 实例；
  - 返回 ACP 可直接消费的 `mcpServers`（stdio bridge 优先，HTTP 回退）。
- 每个 Agent 的 proxy 只配置自己的 backends；Agent 变更只重启该实例。
- 7 个测试通过：registry / spec / proxy config / proxy 启动（fake stdio backend）/ 空选择拒绝；clippy 通过。
- 真实 `mcp-proxy` 已在本机编译并启动 fake stdio backend；通过 HTTP 完成 `initialize`，返回 `mcp-layer` serverInfo。
- Go vendor / 构建脚本已删除；当前网络不再影响 MCP 网关构建。

### 未完成

- `cli-layer` 的 `agent_mcps_set` / `session/new.mcpServers` 接线；
- Claude / Codex / OpenCode 的 ACP 注入实测；
- 真实 MCP server（stdio/HTTP）冒烟（已用 fake stdio server 跑通基础 HTTP 握手）；
- Agent 修改时活跃会话的切换策略；
- 共享 backend runtime / 工具级权限 / 审计。

### 下一步

1. 用真实 stdio MCP server 跑两个 Agent 实例，确认工具列表互不泄漏。
2. 接 `cli-layer`，在 `acp_connect` 注入 `mcpServers`。
3. 再决定是否 fork `mcp-proxy` 做共享 runtime。
