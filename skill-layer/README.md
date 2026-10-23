# crewchat-skill

CrewChat 的 **Skill 层**：全局技能库、版本注册表，以及按 Agent 会话 materialize 的
软链接。它是本地 daemon，不感知 Agent，也不做模型/ACP。

## 边界

- 技能内容只有一份：`~/.agents/skills`（当前版本）。
- 版本原件：`~/.agents/skill-store/<id>/<hash>`。
- 注册表：`~/.agents/skill-layer/registry.sqlite`。
- Agent → 技能的绑定在 `cli-layer`；本服务只接收 `skill_ids`，为 session home 建链。
- 全局 per-CLI 链接（终端裸跑）由 `POST /v1/global-links/sync` 同步；Agent 会话不使用全局目录。

## 构建与运行

```sh
cd skill-layer
cargo build
cargo run
```

默认监听 `127.0.0.1:3460`。启动后写 `~/.agents/skill-layer/runtime.json`（0600）：
`{pid, host, port, url, token}`。客户端从该文件读 token；不要把 token 打进日志。

### 环境变量

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `SKILL_LAYER_ROOT` | `~/.agents` | 技能库根目录 |
| `SKILL_LAYER_HOST` | `127.0.0.1` | 只允许回环 |
| `SKILL_LAYER_PORT` | `3460` | 控制端口 |
| `SKILL_LAYER_TOKEN` | 随机生成 | `Authorization: Bearer <token>` |
| `SKILL_LAYER_SESSION_ROOT` | 无 | 设置后，materialize 的 home 必须在此目录下 |

## API（第一版）

| 方法 | 路径 | 作用 |
| --- | --- | --- |
| `GET` | `/v1/health` | 存活 |
| `GET` | `/v1/skills` | 全局库列表 |
| `GET` | `/v1/skills/{id}` | 技能详情 + `SKILL.md` + 文件列表 |
| `POST` | `/v1/skills/install` | 从本地/Git/归档/URL 安装：`{ "path" }` 或 `{ "source", "gitRef?", "subdir?" }` |
| `POST` | `/v1/skills/{id}/check` | 检查上游是否有新版本 |
| `POST` | `/v1/skills/{id}/update` | 拉取新版本并移动 current |
| `DELETE` | `/v1/skills/{id}` | 删除（版本移入 trash） |
| `POST` | `/v1/sessions/materialize` | 在 session home 建选中技能软链 |
| `GET` | `/v1/collections` | 集合列表（Backend/Frontend/iOS） |
| `POST` | `/v1/collections` | 新建集合：`{ "name", "description?", "skills": [] }` |
| `DELETE` | `/v1/collections/{id}` | 删除集合 |
| `GET` | `/v1/global-links` | 终端裸跑用的全局链接状态 |
| `POST` | `/v1/global-links/sync` | 同步全局链接：`{ "agents": ["claude_code", ...]? }` |

所有请求都要 `Authorization: Bearer <runtime.json token>`。错误体：

```json
{ "error": { "code": "skill_missing", "message": "..." }, "missing": ["id"] }
```

### materialize

```json
{
  "home": "/…/sessions/backend",
  "agentType": "claude_code",
  "skills": [
    { "id": "backend-api" },
    { "id": "backend-db", "hash": "<optional pin>" }
  ]
}
```

- `home` 必须已经存在（由 `cli-layer` 创建）；本服务创建 `<home>/skills/`。
- 不传 `hash` = 当前版本；传了则 pin 到该版本。
- 任一技能不存在 → `409 skill_missing`，**不会**部分建链。
- 已存在的同名软链会复用/替换；同名真实目录/文件 → `400`，不删用户内容。
- 返回 `{ home, skillsDir, links: [{ id, path, target }] }`。

## 当前状态（2026-10-23）

已完成：

- SQLite registry（skills / versions / collections / global links）。
- 全局库 + 版本原件 + `current` 软链。
- 安装源：本地目录、Git/GitHub/GitLab/Azure、zip/tar.gz/tar、直链 `SKILL.md`。
- `SKILL.md` frontmatter 校验；目录树 SHA-256；软链/大小/文件数限制；归档路径穿越/软链拒绝。
- `check` / `update` / `remove`；版本 pin；旧版本移入 trash。
- `materialize`：按 session home 只链选中的技能；session root 与 skills dir 限制。
- 全局 per-CLI 链接同步（claude_code / codex / open_code / gemini）。
- 集合 CRUD（含删除）。
- 7 个单元/集成测试；clippy 通过；HTTP 冒烟（本地/Git/归档安装、全局链接、update、remove）。

未完成：

- 市场搜索/安装（skills.sh / Claude 市场）。
- 按技能 × CLI 的全局链接开关（目前是同步全部）。
- `cli-layer` 代理与 AgentOS 接线（已完成）。
- 技能脚本执行策略、垃圾回收、备份/审计。

## 测试

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

## 许可

MIT。第三方参考与署名见 [`NOTICE.md`](NOTICE.md)。
