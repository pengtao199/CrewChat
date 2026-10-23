# Skill 层 API：前端交付说明

Updated: 2026-10-23
Status: `skill-layer` daemon 第一版可用；core 代理与 Agent 绑定命令尚未接线。
读者：AgentOS 前端工程师。

---

## 1. 谁跟谁说话

```
AgentOS (SwiftUI)
  │  只调核心命令
  ▼
core（当前 codeg / 之后 cli-layer）
  │  内部 HTTP
  ▼
skill-layer (127.0.0.1:3460)
```

- 正式形态：**前端不直接连 skill-layer**，只调 core 的 `skill_*` / `agent_skills_set`。
- 开发形态：core 代理还没接时，前端可以临时直连 `3460` 联调；token 在
  `~/.agents/skill-layer/runtime.json`（0600）。这层直连只用于开发，不要写进产品路径。

---

## 2. skill-layer daemon（已实现）

Base：`http://127.0.0.1:3460`
鉴权：`Authorization: Bearer <runtime.json.token>`

| 方法 | 路径 | 返回 |
| --- | --- | --- |
| GET | `/v1/health` | `{status, version}` |
| GET | `/v1/skills` | `{skills: SkillSummary[]}` |
| GET | `/v1/skills/{id}` | `{skill, content, files}` |
| POST | `/v1/skills/install` | `{skill}` |
| POST | `/v1/skills/{id}/check` | `{id, currentHash, latestHash, updateAvailable}` |
| POST | `/v1/skills/{id}/update` | `{skill}` |
| DELETE | `/v1/skills/{id}` | `{removed}` |
| POST | `/v1/sessions/materialize` | `{home, skillsDir, links}` |
| GET | `/v1/collections` | `{collections}` |
| POST | `/v1/collections` | `{collection}` |
| DELETE | `/v1/collections/{id}` | `{deleted}` |
| GET | `/v1/global-links` | `{links}` |
| POST | `/v1/global-links/sync` | `{links}` |

### 2.1 类型

```ts
type SkillSummary = {
  id: string;              // stable slug，来自 frontmatter name
  name: string;
  description: string;
  currentHash: string;     // 当前版本 hash
  license: string | null;
  sourceKind: string;      // local | git | url（marketplace 以后）
  sourceUrl: string | null;
  sourceRef: string | null;
  sourceSubdir: string | null;
  hasScripts: boolean;
  sizeBytes: number;
  installedAt: string;     // RFC3339
  updatedAt: string;
};

type SkillDetail = {
  skill: SkillSummary;
  content: string;         // SKILL.md 全文
  files: { path: string; size: number }[];
};

type MaterializedLink = { id: string; path: string; target: string };
type Materialized = { home: string; skillsDir: string; links: MaterializedLink[] };
```

### 2.2 安装

```http
POST /v1/skills/install
{ "path": "/Users/me/src/backend-api" }
```

或：

```http
POST /v1/skills/install
{
  "source": "owner/repo",
  "gitRef": "main",
  "subdir": "skills/backend-api"
}
```

- 支持本地目录、Git/GitHub/GitLab/Azure、zip/tar.gz/tar、直链 `SKILL.md`。
- 目录必须包含 `SKILL.md`；frontmatter 必须有 `name`、`description`。
- 同名同内容幂等；同名新内容 = 新版本，`currentHash` 更新。
- 安装**不会**自动绑定 Agent；绑定在 Agents 页。

### 2.3 materialize（核心调用，前端不直接调）

```http
POST /v1/sessions/materialize
{
  "home": "/…/sessions/backend",
  "agentType": "claude_code",
  "skills": [
    { "id": "backend-api" },
    { "id": "backend-db", "hash": "<optional pin>" }
  ]
}
```

返回：

```json
{
  "home": "/…/sessions/backend",
  "skillsDir": "/…/sessions/backend/skills",
  "links": [
    { "id": "backend-api", "path": "…/skills/backend-api", "target": "…/skill-store/backend-api/<hash>" }
  ]
}
```

- `home` 必须已存在；服务只创建 `<home>/skills`。
- 不传 `hash` = 当前版本；传了则 pin。
- 任一技能缺失 → `409 skill_missing`，不部分建链。
- 同名真实目录/文件 → `400`，不删除用户内容。

### 2.4 更新 / 删除 / 全局链接

```http
POST /v1/skills/{id}/check
POST /v1/skills/{id}/update
DELETE /v1/skills/{id}
GET  /v1/global-links
POST /v1/global-links/sync   { "agents": ["claude_code", "codex"]? }
```

- `check` 只比较 hash，不落盘。
- `update` 拉取新版本并移动 `current`；已 materialize 的 session 仍 pin 旧 hash。
- `remove` 删全局/current 链接，版本移入 `skill-layer/trash`。
- `global-links` 是终端裸跑用的 per-CLI 链接；Agent 会话不用它。

### 2.5 集合

```http
GET /v1/collections
POST /v1/collections
{ "name": "Backend", "description": "Backend skills", "skills": ["backend-api"] }
```

集合用于 Agents 页一键选 Backend / Frontend / iOS；安装技能本身不会自动进集合。

### 2.5 错误

```json
{
  "error": { "code": "skill_missing", "message": "skills not installed: x" },
  "missing": ["x"]
}
```

| code | 场景 |
| --- | --- |
| `unauthorized` | 缺/错 token |
| `invalid_input` | 缺 path/home 等 |
| `install_failed` | 目录无效、无 SKILL.md、超限、软链 |
| `skill_missing` | materialize 选中的技能不存在（带 `missing`） |
| `materialize_failed` | home 越界、同名非软链冲突 |
| `collection_failed` | 集合名空、引用未知技能 |
| `not_found` | 技能 id 不存在 |
| `internal` | 数据库/IO |

---

## 3. core 对前端的命令（契约草案，未实现）

前端按这个写，core 接好后切换。所有命令走现有 `/api/<command>`，Bearer core token。

### `skill_list`

```json
{ "skills": SkillSummary[], "collections": Collection[], "globalLinks": [] }
```

> 需要 core 把「哪些 Agent 用了这个技能」补进来；skill-layer 本身不感知 Agent。

### `skill_install`

```json
{ "path": "/Users/me/src/backend-api" }
```

返回 `{ skill }`。安装后**不自动启用**；前端跳 Agents 页或提示「去启用」。

### `skill_market_search` / `skill_market_install`

```json
{ "query": "backend" }
{ "source": "owner/repo", "skill": "backend-api", "ref": "main" }
```

返回市场条目/安装结果；市场未实现前可返回 501，但前端可以先做空态。

### `agent_skills_set`

```json
{
  "roleId": "role-uuid",
  "skillIds": ["backend-api", "backend-db"],
  "collectionIds": ["collection-uuid"]
}
```

返回保存后的选择。集合和技能可以同时存在；最终启用以并集为准。

### `agent_skill_status`

```json
{ "roleId": "role-uuid" }
```

返回：

```json
{
  "skills": [
    { "id": "backend-api", "name": "backend-api", "available": true },
    { "id": "old-skill", "name": "old-skill", "available": false }
  ],
  "missing": ["old-skill"]
}
```

前端在 Agent 行/详情显示「技能不可用」，不静默回退。

### `skill_link_status`

```json
{ "agentType": "claude_code" }
```

返回终端裸跑用的全局链接状态（`linked/disabled/missing/broken/foreign`）；Agent 会话不使用它。

---

## 4. 前端页面

### Agents 页（核心入口）

- 身份 / 引擎 / 模型 / **技能** / MCP / 权限。
- 技能选择器：
  - 数据来自 `skill_list`；
  - 多选 + 搜索；
  - Backend / Frontend / iOS 集合一键选；
  - 显示已选数量；不可用技能标红/黄；
  - 保存走 `agent_skills_set`。
- Agent 列表行显示技能数 / 不可用数。
- **不要**按引擎分技能页；引擎只是 Agent 的一个字段。

### Skills 页（全局库）

- 列表：全部技能、来源、版本 hash、许可、脚本标记、更新时间。
- 详情：`SKILL.md` 预览、文件列表、来源、许可。
- 「被哪些 Agent 使用」：由 core 补；没有 core 数据时先显示空。
- 安装：本地目录/（以后 Git、市场）；安装后不自动绑定。
- 全局 CLI 链接：终端裸跑用，与 Agent 选择分开。
- 冲突/缺失/只读：单独状态。

### 市场页

- 搜索、预览、安装；显示来源/许可/hash/脚本。
- 安装后提示「去 Agents 页启用」。
- 市场未接时显示「暂未接入」。

---

## 5. 三个例子（验收用）

| Agent | CLI | 技能选择 | 会话 home |
| --- | --- | --- | --- |
| 后端专家 | Claude Code | Backend 集合 | `sessions/backend`，`skills/` 只有 backend |
| 前端专家 | Codex | Frontend 集合 | `sessions/frontend`，`skills/` 只有 frontend |
| iOS 专家 | Claude Code | iOS 集合 | `sessions/ios`，`skills/` 只有 iOS |

两个 Claude Agent 并发时，各自 `CLAUDE_CONFIG_DIR` 不同；全局 `~/.claude/skills` 不参与。

---

## 6. 当前可用 / 不可用

**可用（可以直接联调）**

- skill-layer daemon 的 health/list/get/install（本地/Git/归档/URL）、check/update/remove、materialize、collections、global-links。
- 全局库 + 版本 hash + session home 软链。
- 错误形状、409 missing。

**不可用（先做 UI 空态或 mock）**

- core 的 `skill_*` / `agent_skills_set` 代理。
- 市场搜索/安装。
- 按技能 × CLI 的全局链接开关（目前 sync 全部）。
- 审计/垃圾回收/脚本执行策略。
- 「被哪些 Agent 使用」的数据（需要 core 绑定表）。

---

## 7. 开发联调

```sh
cd skill-layer
SKILL_LAYER_ROOT=/tmp/skill-dev \
SKILL_LAYER_PORT=3460 \
SKILL_LAYER_TOKEN=dev-token \
cargo run
```

```sh
curl -H 'Authorization: Bearer dev-token' http://127.0.0.1:3460/v1/skills
```

正式联调时 token 从 `~/.agents/skill-layer/runtime.json` 读取；前端不应把 token 持久化到业务数据。
