# MCP Layer API：cli-layer / AgentOS 交付说明

Updated: 2026-10-23
Status: `mcp-layer` 后端核心可用；`cli-layer` 代理、`agent_roles_save` MCP 同步与 `session/new` 注入已接；AgentOS MCP 页已接；真实 ACP 待验证。
读者：cli-layer 工程师、AgentOS 前端工程师。

---

## 1. 谁跟谁说话

```
AgentOS
  │ 只调 cli-layer 的核心命令
  ▼
cli-layer（花名册 / ACP host）
  │ 内部 HTTP
  ▼
mcp-layer (127.0.0.1:3461)
  ├── 全局 MCP 注册表
  └── 每个 Agent 一个 mcp-proxy 实例
        └── 只挂该 Agent 选中的 MCP
```

- 前端**不直接连** `mcp-layer`。
- `cli-layer` 通过 `~/.agents/mcp/runtime.json` 找控制面。
- 每个 Agent 的 proxy 是动态 loopback 端口，不是固定端口。

---

## 2. 运行时与鉴权

`~/.agents/mcp/runtime.json`（0600）：

```json
{
  "pid": 12345,
  "host": "127.0.0.1",
  "port": 3461,
  "url": "http://127.0.0.1:3461",
  "token": "<control-token>",
  "proxyDir": "/Users/me/.agents/mcp/runtime/proxies"
}
```

所有 `mcp-layer` 请求：

```http
Authorization: Bearer <control-token>
```

Agent proxy 的 token 是另一个 token，只在 `PUT /v1/agents/{id}` 返回；`GET` 列表不返回 token。

---

## 3. API 一览

| 方法 | 路径 | 作用 |
| --- | --- | --- |
| `GET` | `/v1/health` | 控制面 + 所有 proxy 状态 |
| `GET` | `/v1/servers` | 全局 MCP 库 |
| `POST` | `/v1/scan` | 扫描本机 CLI 配置并导入新 MCP |
| `GET` | `/v1/servers/{id}` | 单个 MCP |
| `PUT` | `/v1/servers/{id}` | 新建/更新 MCP 定义 |
| `DELETE` | `/v1/servers/{id}` | 删除 MCP，清理 Agent 引用 |
| `POST` | `/v1/servers/{id}/enable` | 启用 |
| `POST` | `/v1/servers/{id}/disable` | 停用 |
| `GET` | `/v1/agents` | Agent → proxy 映射（不含 token） |
| `GET` | `/v1/agents/{id}` | 单个 Agent 映射（不含 token） |
| `PUT` | `/v1/agents/{id}` | 设置 Agent allowlist，启动/重启 proxy |
| `DELETE` | `/v1/agents/{id}` | 停止 proxy，删除映射 |
| `GET` | `/v1/gateway` | 所有 proxy 实例状态 |
| `POST` | `/v1/gateway/sync` | 重启所有 Agent proxy |

---

## 4. Server 定义

`PUT /v1/servers/{id}` body：

```json
{
  "name": "Playwright",
  "transport": "stdio",
  "description": "Browser automation",
  "enabled": true,
  "command": "/opt/homebrew/bin/npx",
  "args": ["-y", "@playwright/mcp@latest"],
  "env": { "LOG_LEVEL": "info" }
}
```

HTTP：

```json
{
  "name": "Remote MCP",
  "transport": "http",
  "url": "https://example.com/mcp",
  "headers": { "X-API-Key": "..." },
  "bearerToken": "..."
}
```

- `transport`: `stdio` / `http` / `sse`。
- `id` 来自 URL，必须是 `[a-z0-9_-]`。
- `stdio` 必须有 `command`；`http`/`sse` 必须有 `url`。
- `enabled=false` 时保留定义，但不应进入新 Agent proxy。
- 返回值：

```json
{ "server": { "id": "...", "spec": {...}, "hash": "...", ... }, "warnings": [] }
```

- 保存后，引用该 server 的 Agent proxy 会重启；`warnings` 是重启失败项。

---

## 5. Agent 同步（核心调用）

`PUT /v1/agents/{id}` body：

```json
{ "serverIds": ["playwright", "postgres"] }
```

- `id` 是 `cli-layer.db` 的 `agent_roles.id`。
- MCP 页打开时 `mcp_list` 会先尽力扫描本机 CLI 配置，所以已安装的 MCP 应自动出现在库里。
- `serverIds` 是显式 allowlist；**空数组 = 没有 MCP**，不继承全部。
- 幂等：相同 `serverIds` 重复调用会复用已有 proxy。
- 缺失 server → `409 server_missing`，不会部分启动。
- 返回：

```json
{
  "agentId": "role-1",
  "clientName": "agent-role-1",
  "serverIds": ["playwright", "postgres"],
  "endpoint": "http://127.0.0.1:41001/",
  "token": "<agent-token>",
  "http": {
    "type": "http",
    "name": "mcp-layer",
    "url": "http://127.0.0.1:41001/",
    "headers": [
      { "name": "Authorization", "value": "Bearer <agent-token>" }
    ]
  },
  "stdio": {
    "type": "stdio",
    "name": "mcp-layer",
    "command": "/opt/homebrew/bin/npx",
    "args": [
      "-y", "mcp-remote", "http://127.0.0.1:41001/",
      "--allow-http",
      "--header", "Authorization: Bearer <agent-token>"
    ],
    "env": []
  },
  "mcpServers": [ /* stdio 优先；无 npx 时回退 http */ ]
}
```

- `mcpServers` 是 ACP `session/new` 可直接消费的数组。
- `endpoint` 是 `mcp-proxy` 的根路径 `/`。
- Agent token 只在 `PUT` 返回；`GET /v1/agents` 不含 token。
- 空 `serverIds` 会停止该 Agent proxy，返回 `mcpServers: []`。

---

## 6. 错误形状

```json
{
  "error": { "code": "server_missing", "message": "servers not installed: x" },
  "missing": ["x"]
}
```

| code | HTTP | 说明 |
| --- | --- | --- |
| `unauthorized` | 401 | 控制面 token 错 |
| `not_found` | 404 | server/agent 不存在 |
| `invalid_server` | 400 | spec 校验失败 |
| `invalid_agent` | 400 | agent id 非法 |
| `server_missing` | 409 | Agent 选了未安装 server；无部分启动 |
| `proxy_unavailable` | 503 | proxy 启动失败 |
| `internal` | 500 | 内部错误 |

---

## 7. cli-layer 接线

### 7.1 客户端（已落地）

`cli-layer/src/infra/mcp_layer.rs`：

- 从 `~/.agents/mcp/runtime.json` 读 `url` / `token`；
- 超时 60s；
- `sync_agent(agent_id, server_ids) -> AgentSession`；
- `list_servers()` / `upsert_server()` 给 MCP 页代理用。

### 7.2 `agent_roles_save` / `agent_mcps_set`

AgentOS 保存 Agent 时只发 `agent_roles_save`：

```json
{ "role": { "id": "role-1", "name": "…", "mcp": ["playwright"], "…": "…" } }
```

流程：

1. 校验 role；
2. 写 `agent_roles.mcp_json`（`cli-layer.db`）；
3. 若 `mcp` 变化，调 `PUT /v1/agents/{roleId}`；
4. 返回角色；同步失败则回滚角色并返回明确错误，不静默清空。

`agent_mcps_set` 保留为独立命令（`{roleId, serverIds}`），但 AgentOS 不再同时发它，避免一次保存写两遍。

### 7.3 `acp_connect`（已落地）

在角色已知、`session/new` 之前：

```rust
let mcp = mcp_layer.sync_agent(&role.id, &role.mcp).await?;
let mcp_servers: Vec<McpServer> =
    serde_json::from_value(mcp.mcp_servers)?;

let request = NewSessionRequest::new(cwd).mcp_servers(mcp_servers);
```

规则：

- `role.mcp` 为空 → 不调 mcp-layer，不注入，正常起会话。
- `role.mcp` 非空且 mcp-layer 不可用 → 连接失败，错误码 `mcp_layer_unavailable`；不要静默起一个没有 MCP 的会话。
- `role.mcp` 非空但返回 `server_missing` → 连接失败，UI 显示缺失项。
- 不要写任何 CLI 全局 MCP 配置。
- 不要读旧 `~/.agents/mcp.json` 或旧 CLI 配置。

### 7.4 Agent 修改

保存 Agent 的 `mcp` 变化时会重启该 Agent 的 proxy：

- 新会话拿新 endpoint/token；
- 活跃会话仍连着旧实例，可能被停止；UI 提示「MCP 变更，重连会话后生效」；
- 不重启其他 Agent。

### 7.5 启动顺序

```
AgentOS → mcp-layer → cli-layer →（ACP 会话）
```

- AgentOS 先探活 `~/.agents/mcp/runtime.json`；已有健康 mcp-layer 就复用，否则在 cli-layer 之前启动它。
- cli-layer 启动时读取一次 runtime 文件；mcp-layer 必须先就绪。
- `mcp-layer` 启动不自动拉起所有 Agent proxy；`acp_connect` / 保存 Agent MCP 绑定按需启动。

---

## 8. AgentOS 页面映射

### MCP 页

- 列表：`mcp_list`（cli-layer 代理 `GET /v1/servers`）。
- 新建/编辑：`mcp_upsert`（`PUT /v1/servers/{id}`），详情内联编辑并自动保存。
- 删除：`mcp_remove`（`DELETE /v1/servers/{id}`）；cli-layer 同时清理花名册引用。
- 启停：详情里的 Toggle 走 `mcp_upsert` 的 `enabled`；不单独调 enable/disable。
- 不再有「写入哪些 CLI」的 `apps` 多选。
- 旧 `mcp_scan_local` / `mcp_upsert_local_server` / `mcp_remove_server` 不再兼容。

### Agents 页

- MCP 多选：`mcp_list`，只列 `enabled` 的 server。
- 保存：`agent_roles_save` 的 `mcp` 字段；由 cli-layer 检测变化并同步 mcp-layer。
- 可用性：`agent_mcp_status`（cli-layer 代理 `GET /v1/agents/{id}` + `/v1/gateway`）。
- 空选择 = 没有 MCP；提供「全选」按钮但保存显式列表。

---

## 9. 验收清单

1. 两个 Agent 用同一个 CLI：
   - A 选 server A，B 选 server B；
   - 两边 `tools/list` 只出现自己的 backend 工具；
   - 不出现 `proxy/*` 管理工具。
2. Agent 选缺失 server → `409 server_missing`，不启动半套 proxy。
3. Agent 改 MCP → 只重启该 Agent，另一个 Agent 不受影响。
4. mcp-layer 停止 + Agent 有 MCP → `acp_connect` 明确失败；Agent 空 MCP 仍可起会话。
5. Claude / Codex / OpenCode 至少各跑一次真实 `session/new` 注入。
6. 控制面 token 错误 → 401；Agent token 错误 → proxy 401。

---

## 10. curl 示例

```sh
TOKEN=$(jq -r .token ~/.agents/mcp/runtime.json)
BASE=http://127.0.0.1:3461

curl -s -H "Authorization: Bearer $TOKEN" $BASE/v1/servers | jq

curl -s -X PUT -H "Authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name":"Echo","transport":"stdio","command":"python3","args":["/tmp/fake.py"]}' \
  $BASE/v1/servers/echo | jq

curl -s -X PUT -H "Authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' \
  -d '{"serverIds":["echo"]}' \
  $BASE/v1/agents/role-1 | jq
```

---

## 11. 限制

- 每个 Agent 一个 proxy 实例，stdio backend 可能重复启动。
- Agent 修改会重启该实例，活跃会话需要重连。
- 当前只做 server 级 allowlist；工具级 allow/deny 后续在 `mcp-proxy` 上二次开发。
- 旧数据不迁移。
