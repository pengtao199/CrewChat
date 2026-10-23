# crewchat-mcp

CrewChat 的 **MCP 层**：全局 MCP 注册表、每个 Agent 一个本地网关实例，以及 ACP 会话配置。
对外统一叫 `mcp-layer`；网关内核复用 Rust `mcp-proxy`，不再依赖 Go 或 SQLite/Postgres。

## 形态

```
AgentOS / cli-layer
  → mcp-layer (127.0.0.1:3461)
      ├── ~/.agents/mcp/servers/<id>.json
      ├── ~/.agents/mcp/registry.sqlite
      ├── ~/.agents/mcp/runtime/proxies/<agent>/proxy.toml
      └── 每个 Agent 一个 mcp-proxy 实例
            └── 只挂该 Agent 选中的 MCP，loopback + bearer token
```

- Agent 的 `mcps` 是 allowlist；空 = 没有 MCP。
- 每个 Agent 一个 proxy 实例，工具、prompts、resources 都随实例隔离。
- 旧 CLI MCP 配置**不迁移、不读取**。
- 网关内核是 `mcp-proxy`（MIT OR Apache-2.0），没有 Go sidecar。

## 构建与运行

```sh
cargo build
cargo run
```

默认 `127.0.0.1:3461`，运行时文件 `~/.agents/mcp/runtime.json`（0600）。

环境变量：

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `MCP_LAYER_ROOT` | `~/.agents/mcp` | 注册表与运行时根目录 |
| `MCP_LAYER_HOST` | `127.0.0.1` | 只允许回环 |
| `MCP_LAYER_PORT` | `3461` | 控制端口 |
| `MCP_LAYER_TOKEN` | 随机生成 | 控制面 Bearer token |
| `MCP_LAYER_NPX` | PATH 中的 npx | stdio bridge 的绝对路径 |

每个 Agent 的 proxy 使用动态 loopback 端口，不需要固定端口配置。

## API（后端核心）

所有请求：`Authorization: Bearer <runtime.json token>`。

| 方法 | 路径 | 作用 |
| --- | --- | --- |
| `GET` | `/v1/health` | 控制面 + 所有 proxy 状态 |
| `GET` | `/v1/servers` | MCP 库 |
| `GET` | `/v1/servers/{id}` | 单个 MCP |
| `PUT` | `/v1/servers/{id}` | 新建/更新（写 `servers/<id>.json`） |
| `DELETE` | `/v1/servers/{id}` | 删除并清理 Agent 引用 |
| `POST` | `/v1/servers/{id}/enable` / `disable` | 启停 |
| `GET` | `/v1/agents` | Agent proxy 映射 |
| `GET` | `/v1/agents/{id}` | 单个 Agent |
| `PUT` | `/v1/agents/{id}` | 设置 allowlist，启动/重启 proxy，返回会话配置 |
| `DELETE` | `/v1/agents/{id}` | 停止 proxy，删除映射 |
| `GET` | `/v1/gateway` | 所有 proxy 实例状态 |
| `POST` | `/v1/gateway/sync` | 重启所有实例 |

### Agent 会话返回

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

- `mcpServers` 是 ACP `session/new` 可直接消费的形状。
- 每个 Agent 的 proxy 只配置选中的 backends。
- 任一 server 缺失 → `409 server_missing`，不部分绑定。
- 空 `serverIds` → 停止 proxy，返回空 `mcpServers`。

## 当前状态

- 控制面、注册表、Agent proxy 实例、动态端口、bearer token 已实现。
- 网关内核复用 `mcp-proxy` 0.6（`default-features = false`），不启用 OTel/OAuth/OpenAPI/Redis/SQLite。
- 7 个测试通过（registry / spec / proxy config / proxy 启动 / Agent 隔离），clippy 通过。
- `cli-layer` 的 `agent_mcps_set` / `session/new.mcpServers` 接线不在本 crate 内。
- 不做旧数据迁移。

## 后续二次开发

在 `mcp-proxy` 之上补：

- 相同 backend 集合的 Agent 共享 runtime；
- 动态更新 backend，不重启实例；
- tool / prompt / resource 级 Agent 权限；
- 健康、日志、审计、Keychain；
- 市场与来源校验。
