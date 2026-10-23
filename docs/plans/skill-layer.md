# Skill 层：全局一份库，按 Agent 启用

Updated: 2026-10-23
Status: Paused（后端核心完成；等待 CLI 真实会话验证）
关系：`cli-layer-migration.md` 的 capabilities/skills；`model-layer` 不感知技能；`AgentOS` 只渲染。

---

## 0. 结论

- **全局只有一份技能库**：`~/.agents/skills`（用户视角；内部版本原件在 `~/.agents/skill-store`）。
- **技能归属 Agent，不属于 CLI**：Agents 页给每个 Agent 选择启用的技能/集合；同一技能可被多个 Agent、多个 CLI 使用。
- **每个 Agent 会话一个私有 home**：只把该 Agent 选中的技能软链进 `CLAUDE_CONFIG_DIR` / `CODEX_HOME` 等，绝不共用全局 CLI 目录。这样两个 Claude Agent 可以同时跑不同技能集。
- **全局 per-CLI 软链只服务“终端裸跑 CLI”**，不是 Agent 的路径；默认仍可让所有 CLI 看到全部技能。
- **`skill-layer` 做成小 daemon（`crewchat-skill`，`:3460`）+ client**：注册表、版本库、安装/更新/卸载、session materialize。它不是网关，不感知 Agent。
- 先验证 4 个 CLI 的 per-session 技能目录能力，再做 store/daemon，最后做 Agents 页与市场。

---

## 1. 关键模型：技能属于 Agent

```
Agent = CLI 身体 + Model 大脑 + SkillSet 能力 + MCP + 记忆 + 规则
```

| 例子 | CLI | 模型 | 技能 |
| --- | --- | --- | --- |
| 后端专家 | Claude Code | 顶级模型 | backend skills |
| 前端专家 | Codex | 另一个模型 | frontend skills |
| iOS 专家 | Claude Code | 最顶级模型 | iOS skills |

后端和 iOS 都用 Claude Code，但技能集不同。如果两者都读 `~/.claude/skills`，必然串味。因此：

- **全局库**：所有技能的唯一定义，`~/.agents/skills`。
- **Agent 选择**：`agent_skills(role_id, skill_id)`，在 Agents 页维护。
- **会话注入**：启动 CLI 前，为这个 Agent 建独立 home，只软链它选中的技能。
- **全局链接**：`~/.claude/skills/<id>` 等只给“用户在终端裸跑 CLI”用；Agent 会话不读它。

---

## 2. 目标与边界

### 目标

- 全局一份技能内容，不再出现「Claude 一份、Codex 一份、OpenCode 一份」。
- Agents 页选择技能/集合；每 Agent 独立生效，可并发。
- 注册：稳定 skill ID、来源、版本/commit、内容哈希、许可、安装时间。
- 安装/更新/卸载：Git、本地目录、zip/tar、URL、市场；可回滚。
- 全局终端可用：默认把所有技能链接到检测到的 CLI；可按 CLI 关闭。
- 市场：搜索、预览、安装、更新；显示来源、许可、脚本、哈希。
- 旧技能迁移：各 CLI 目录收敛到全局库；原位置留软链；可 dry-run、可回滚。

### 不做什么

- 不做 Agent、模型路由、MCP、会话；分别归 `cli-layer` / `model-layer`。
- v1 不安装插件里的 agents / commands / hooks / MCP，只装 skills。
- 不在安装时执行技能里的脚本；只落盘、标记、展示。
- 不做 Windows（macOS 优先）；不做云同步/团队 RBAC。
- 不复制 Anthropic 的 source-available 文档技能。

---

## 3. 现状

| CLI | 全局技能目录 | 备注 |
| --- | --- | --- |
| Claude Code | `~/.claude/skills` | 不读 `~/.agents/skills` |
| Codex | `~/.codex/skills` + `.system` | `.system` 内置只读 |
| OpenCode | `$XDG_CONFIG_HOME/opencode/skills` 或 `~/.config/opencode/skills` | 也读 `~/.agents/skills` |
| Gemini CLI | `~/.gemini/skills` | 也读 `~/.agents/skills` |
| 共享 | `~/.agents/skills` | Codex/OpenCode/Gemini 已当共享根 |

问题：

- 同名技能多份，内容可能分叉；
- 没有统一来源/版本/哈希，更新靠手工；
- 旧核心的 `acp_*_agent_skill` 按引擎操作文件，AgentOS Skills 页按引擎看；
- 没有「技能被哪些 Agent 使用」的视图；
- `cli-layer` 的 skills 仍是 501。

已有可复用事实：

- `.agents/skills` 已是 Codex/OpenCode/Gemini 的共享根；Claude 只差链接。
- 本机能读到 82（OpenCode）/97（Codex）条技能，说明共享目录已承载大量内容。
- 软链对四个 CLI 的目录扫描是可行的，但 **per-session 技能目录** 需要逐个验证。

---

## 4. 开源调研

### 4.1 结论

- 「中央库 + 软链」已被多个成熟项目验证：`vercel-labs/skills`、`xingkongliang/skills-manager`、`jiweiyeah/Skills-Manager`、`yibie/skills-manager`。
- 但它们的默认模型主要是 **全局/项目级**：`vercel-labs/skills` 是 project/global，**没有 Agent 级技能集**；`yibie` 的 Library → Collection → Mount 最接近我们的「按 Agent/集合挂载」。
- 我们的关键差异是：**同一 CLI 的多个 Agent 并发、技能集不同**。这要求 per-session home，而不是全局 per-CLI 软链。
- 市场发现源成熟：`skills.sh`、`anthropics/claude-plugins-community`、`wshobson/agents`、`buildwithclaude`。
- 企业级 registry（`iflytek/skillhub`、`Observal`）功能全但太重（Docker/K8s、RBAC/审计），桌面第一版不采用。
- **不要直接依赖 `npx skills`**（Node 22 + 遥测）；镜像它的安装源格式、软链/复制策略、体积限制，用 Rust 原生实现。

### 4.2 项目对比

| 项目 | Stars | 许可 | 关键能力 | 我们取什么 |
| --- | ---: | --- | --- | --- |
| [Agent Skills 规范](https://agentskills.io) / [anthropics/skills](https://github.com/anthropics/skills) | 183k | 混合（多为 Apache-2.0；doc 技能 source-available） | `SKILL.md` + frontmatter；模板；插件市场 | 格式、校验、模板；不复制 doc 技能 |
| [vercel-labs/skills](https://github.com/vercel-labs/skills) | 36.2k | MIT | `npx skills add/find/update/remove`；80+ agent；project/global；symlink/copy；Git/GitLab/Azure/URL/本地；10MiB/25MiB/1000 文件 | 安装源、体积限制、软链/复制策略、`skills.sh` 发现 |
| [yibie/skills-manager](https://github.com/yibie/skills-manager) | 459 | MIT | 原生 macOS SwiftUI/SwiftData；Library → Collection → Mount；软链；稳定 ID；provider ownership/recovery；冲突；skills.sh 发现（不需要 npx） | **最接近**：集合挂载、ID/状态/冲突/回滚；但它是全局 agent 目录，不是 per-session |
| [xingkongliang/skills-manager](https://github.com/xingkongliang/skills-manager) | 6.7k | MIT | 中央库 `~/.skills-manager`；50+ 工具；symlink/copy；市场；presets；更新；备份/同步 | 中央库、presets、每 agent 开关 |
| [jiweiyeah/Skills-Manager](https://github.com/jiweiyeah/Skills-Manager) | 1.0k | MIT | 中央库；32 工具软链；市场；Windows junction | 软链矩阵、Windows 降级 |
| [wshobson/agents](https://github.com/wshobson/agents) | 41.6k | MIT | 多 harness 插件市场：94 plugins / 185 skills；Claude/Codex/Cursor/OpenCode/Antigravity/Copilot/Pi | 市场清单格式、多 harness 安装差异 |
| [anthropics/claude-plugins-community](https://github.com/anthropics/claude-plugins-community) | 4.7k | Apache-2.0 | 官方社区市场只读镜像；`.claude-plugin/marketplace.json` | 官方市场发现源 |
| [iflytek/skillhub](https://github.com/iflytek/skillhub) | 5.3k | Apache-2.0 | 自托管 registry；发布/版本；RBAC/审计；Docker/K8s | 版本/审计模型；不采用部署形态 |
| [Observal](https://github.com/Observal/Observal) | 4.5k | Apache-2.0 | 自托管 Skills/MCP/Agents registry；ARD | 资源注册思路；过重 |
| [davepoon/buildwithclaude](https://github.com/davepoon/buildwithclaude) | 3.7k | MIT | Skills/Agents/Commands/Hooks/Plugins 发现 | 发现源候选 |

### 4.3 关键事实

- `vercel-labs/skills` 默认安装到 **project**，`-g` 才是用户目录；安装方式可选 **symlink（推荐）** 或 **copy**；这验证「canonical copy + agent links」，但它是 CLI 级的 links，不是 Agent 级。
- `yibie/skills-manager` 的 `Library → Collection → Mount` 是最接近的抽象：Library 受管来源，Collection 可复用分组，Mount 软链到 agent；卸载只删链接。我们可以把它扩展为 **Mount 到 session home**，而不是 agent 全局目录。
- `xingkongliang/skills-manager` 的 central library + presets 说明「按集合批量启用」是用户需要的；我们的 Backend/Frontend/iOS 就是 collections。
- `wshobson/agents` 说明多 harness 的差异在安装位置/格式，技能内容本身共享；我们的 per-session home 只改变安装位置。
- `anthropics/claude-plugins-community` 的市场清单是 `.claude-plugin/marketplace.json`；可作为官方发现源，安装仍走我们自己的校验。

---

## 5. 架构

```
AgentOS (SwiftUI)
  │  Agents 页：选 CLI / 模型 / 技能/集合
  ▼
cli-layer (Rust, ACP host)
  ├─ agent_roles / agent_skills / agent_collections
  ├─ 每个 session 建 home（模型注入已有）
  └─ 调 skill-layer materialize(session_home, agent_type, skill_ids)
        │ HTTP 127.0.0.1:3460
        ▼
crewchat-skill (Rust daemon)
  ├─ registry.sqlite
  ├─ ~/.agents/skill-store/<id>/<hash>/
  ├─ ~/.agents/skills/<id> → current
  ├─ install/update/remove/market
  └─ global links（终端裸跑用）
        │
        ▼
Claude Code / Codex / OpenCode / Gemini
```

### 5.1 为什么是 daemon

- 会同时存在旧核心（过渡）、cli-layer 多个宿主；技能库/注册表必须单写入者。
- 未来 Swift 不能直接调用 Rust library；HTTP 是唯一稳定的跨语言面。
- 安装/更新/软链是共享资源，daemon 统一加锁，避免多进程写 registry/links。
- 它不处理模型、不做协议转换、不常驻长连接；只是一个本地控制面。

**如果确认 cli-layer 是唯一宿主**，library 也能工作；但按当前多宿主/未来 Swift 的方向，daemon 更稳。文档按 daemon 设计，保留 library 作为可选实现（daemon 内部仍是一个 crate）。

### 5.2 skill-layer 不感知 Agent

- `skill-layer` 只知道 skill ID、版本、路径、安装源、全局链接。
- Agent → skill 的绑定在 `cli-layer`（花名册）。
- `materialize` 只接收 `{session_home, agent_type, skill_ids}`，不查 Agent 表。
- 这样模型层、技能层、Agent 层边界清晰，未来换运行时不用重写技能库。

---

## 6. 数据模型

### 6.1 文件布局

```text
~/.agents/
├── skills/                    # 全局库（用户视角，当前版本）
│   ├── <skill-id> -> ../skill-store/<id>/<hash>
│   └── ...
├── skill-store/               # 版本原件（内部）
│   └── <skill-id>/<hash>/
│       ├── SKILL.md
│       ├── scripts/
│       └── assets/
└── skill-layer/
    ├── registry.sqlite
    ├── backups/<timestamp>/
    └── trash/
```

- 用户/CLI 看到的全局库是 `~/.agents/skills/<id>`；实际内容是 `skill-store/<id>/<hash>`。
- 每个 session home 的 `skills/<id>` 直接软链到 `skill-store/<id>/<hash>`，锁定版本。
- 全局 per-CLI 链接指向 `~/.agents/skills/<id>`（current）。

### 6.2 skill-layer registry.sqlite

```sql
skills(id TEXT PRIMARY KEY, name, description, created_at, updated_at);
skill_versions(
  skill_id TEXT, hash TEXT, path TEXT, source_kind, source_url,
  source_ref, source_subdir, commit_sha, license, size_bytes,
  has_scripts, metadata_json, installed_at,
  PRIMARY KEY(skill_id, hash)
);
collections(id TEXT PRIMARY KEY, name, description);
collection_skills(collection_id, skill_id, PRIMARY KEY(collection_id, skill_id));
global_links(skill_id, agent_type, path, state, linked_at,
  PRIMARY KEY(skill_id, agent_type));
operations(id INTEGER PRIMARY KEY AUTOINCREMENT, kind, skill_id, agent_type,
  detail_json, created_at);
```

- `skills.current_hash` 可加一列，指向当前版本。
- `global_links.state`: linked | disabled | missing | broken | foreign。
- 不给 Agent 存技能：Agent 绑定在 cli-layer。

### 6.3 cli-layer 侧

```sql
agent_roles(id, name, body, provider, model, ...);
agent_skills(role_id, skill_id, pinned_hash, PRIMARY KEY(role_id, skill_id));
agent_collections(role_id, collection_id, PRIMARY KEY(role_id, collection_id));
```

- 现有 `custom_agent.skills_json` 可继续作为兼容字段；新 UI 写 `agent_skills`，迁移时把 JSON 展开。
- `pinned_hash` 可选；默认空 = 会话启动时取当前版本。

---

## 7. 会话 materialize 与 CLI 矩阵

### 7.1 启动流程

`cli-layer` 在 `acp_connect`：

1. 读 Agent：`body`（CLI）、`provider/model`（已有）、`agent_skills` + `agent_collections`。
2. 建 session home（已有模型 overlay 逻辑）：
   - Claude：`<home>/settings.json`（模型）
   - Codex：`<home>/config.toml`（模型）
   - OpenCode/Gemini：按各自配置目录。
3. 调 `skill-layer`：
   ```json
   POST /v1/sessions/materialize
   {
     "session_id": "...",
     "agent_type": "claude_code",
     "home": ".../sessions/<id>",
     "skills": [{"id": "backend-api", "hash": "..."}]
   }
   ```
4. `skill-layer` 在 `<home>/skills/` 建逐技能软链，指向 `skill-store/<id>/<hash>`。
5. `cli-layer` 设置 `CLAUDE_CONFIG_DIR` / `CODEX_HOME` 等，启动 CLI。

**不静默降级**：选中的技能缺失/损坏 → 会话启动失败并提示；不要偷偷用 CLI 全局技能。

### 7.2 CLI 矩阵

| CLI | session home 环境变量 | 技能目录 | 状态 |
| --- | --- | --- | --- |
| Claude Code | `CLAUDE_CONFIG_DIR` | `<home>/skills` | 已确认可用 |
| Codex | `CODEX_HOME` | `<home>/skills` | 已确认可用（`.system` 需处理） |
| OpenCode | `OPENCODE_CONFIG` / XDG | `<home>/…/skills` | **待验证** |
| Gemini CLI | `GEMINI_HOME` / 配置目录 | `<home>/skills` | **待验证** |
| 其他长尾 | 按 registry | — | 后置 |

首版只保证 Claude / Codex 的 per-Agent 技能；OpenCode/Gemini 在 P0 验证，不能隔离就明确标注「不支持 per-Agent 技能」，不要假装支持。

### 7.3 全局 per-CLI 链接（终端裸跑）

- 默认把全局库所有技能链接到检测到的 CLI：
  - `~/.claude/skills/<id>` → `~/.agents/skills/<id>`
  - `~/.codex/skills/<id>` → `~/.agents/skills/<id>`
  - OpenCode/Gemini 同理。
- 这些链接只给终端直接运行 CLI 用；Agent 会话使用自己的 home，不受影响。
- 首版由显式 `POST /v1/global-links/sync` 同步；安装后自动同步、按 CLI 关闭后置。
- Codex `.system` 不碰；整目录已是软链时识别并标 `foreign`，不重复建。

---

## 8. 安装 / 更新 / 卸载 / 迁移

### 8.1 安装源

| 来源 | 示例 | 说明 |
| --- | --- | --- |
| GitHub shorthand | `owner/repo` | 先 API/`gh`，再 git clone |
| Git URL | `https://…/repo.git`、`git@…` | 私有库走本机 git 凭据，不落库 |
| 子目录 | `…/tree/main/skills/foo` | 只取该目录 |
| 本地目录 | `~/src/my-skill` | 复制到 store |
| 归档 | `.zip` / `.tar` / `.tar.gz` / `.tgz` | 解压前校验 |
| 直链 | `https://…/my-skill` | 单 `SKILL.md` 或归档 |
| 市场 | skills.sh / Claude 市场条目 | 解析到上述来源 |

### 8.2 校验

- 必须有 `SKILL.md`；frontmatter 必须有 `name`、`description`。
- 拒绝绝对路径、`..`、越界软链、设备文件。
- 下载 10 MiB、解压 25 MiB、最多 1000 文件（可配置；参考 vercel）。
- 目录树哈希；记录 license；扫描常见密钥模式；列出 scripts/assets，标 `has_scripts`。
- **安装时不执行脚本**。

### 8.3 安装 / 更新 / 卸载

```text
install: 下载 → 临时目录 → 校验 → skill-store/<id>/<hash>
         → 更新 current → registry → 全局链接（可选）→ operation
update:  重新拉取 → 新 hash → 展示文件 diff → 备份旧版
         → 新版本入 store → 移动 current；运行中 session 仍指向旧 hash
remove:  删除全局链接 / session 链接由会话结束清理
         → 版本移入 trash（被运行中 session 引用则延后）→ registry
```

- 默认手动更新；`check` 只提示。
- 备份 + trash + operations 审计。
- Agent 会话 pin hash，更新不影响运行中的 Agent。

### 8.4 旧技能迁移

1. 扫描：`~/.agents/skills`、四个 CLI 目录、`custom_agent.skills_json`。
2. 按内容哈希 + 名称去重，生成 dry-run：
   - 已在全局库且一致 → 建全局链接；
   - 仅在 CLI → 复制到 store，原位置备份后改软链；
   - 同名不同内容 → 保留两份，标冲突；
   - 系统/只读 → 跳过（Codex `.system`）；
   - 未知/损坏 → 跳过并报告。
3. 用户确认后执行；备份目录与链接状态。
4. 把 `custom_agent.skills_json` 展开成 `agent_skills`；缺失技能标「不可用」。
5. 幂等；可回滚。

---

## 9. 市场

### 9.1 发现源

| 源 | 形态 | 用途 |
| --- | --- | --- |
| [skills.sh](https://skills.sh) | 公开目录/搜索 | 主发现源；先确认 API/条款，失败则缓存索引 |
| [anthropics/claude-plugins-community](https://github.com/anthropics/claude-plugins-community) | `.claude-plugin/marketplace.json` | 官方社区 |
| [wshobson/agents](https://github.com/wshobson/agents) | 多 harness 市场清单 | 大量 skills/plugins |
| [davepoon/buildwithclaude](https://github.com/davepoon/buildwithclaude) | 聚合目录 | 补充 |
| GitHub topic/search | `agent-skills` / `claude-skills` | 兜底 |

### 9.2 条目与安装

- 名称、描述、来源、作者、stars、license、最近更新。
- 技能列表；预览 `SKILL.md`、scripts/assets、哈希、许可。
- 安装前确认；安装后进全局库，**不自动绑定 Agent**；用户在 Agents 页选择。
- v1 只装 skills；plugin 条目提示「只安装其中的 skills」或拒绝。
- 信任：固定 ref/hash；不自动更新；更新展示 diff。
- 记录 license；`NOASSERTION`/source-available 警告。

---

## 10. 安全

技能会被 Agent 读取，可能包含脚本；按不可信代码处理。

| 风险 | 措施 |
| --- | --- |
| 路径穿越/绝对路径 | realpath 必须在临时目录内；拒绝 `..`、设备文件 |
| 软链逃逸 | 归档内软链拒绝；已有技能内软链校验目标在技能目录内 |
| zip bomb/大文件 | 10 MiB / 25 MiB / 1000 文件上限 |
| 脚本执行 | 安装不执行；标 `has_scripts`；UI 展示；用户确认 |
| Prompt injection | 预览 `SKILL.md`；来源/哈希；不静默安装 |
| 密钥泄露 | 扫描常见 key 模式；不写进 registry |
| 许可 | 记录 SPDX；source-available/NOASSERTION 警告；不复制 doc 技能 |
| 软链误删 | 只删 registry 中属于本层的链；foreign/broken 不自动删 |
| daemon 被滥用 | 仅回环 + token；materialize 只允许写 session root 下的路径 |

---

## 11. API 与 UI

前端交付说明（可直接发给前端工程师）：[`skill-layer-api.md`](skill-layer-api.md)。

### 11.1 `skill-layer` HTTP（草案）

| 端点 | 作用 |
| --- | --- |
| `GET /v1/skills` | 全局库 + 版本 + 全局链接状态 |
| `POST /v1/skills/install` | 从 Git/本地/归档/URL 安装 |
| `POST /v1/skills/{id}/update` | 检查/执行更新 |
| `DELETE /v1/skills/{id}` | 移入 trash |
| `GET /v1/collections` | Backend/Frontend/iOS 集合 |
| `POST /v1/sessions/materialize` | 在 session home 建选中技能软链 |
| `POST /v1/global-links/sync` | 终端裸跑用的全局链接 |
| `POST /v1/market/search` | 市场搜索 |
| `POST /v1/market/install` | 市场安装 |
| `POST /v1/import/scan` / `migrate` | 旧数据扫描/迁移 |

### 11.2 `cli-layer` 对 AgentOS 的命令

| 命令 | 作用 |
| --- | --- |
| `skill_list` | 全局库 + 每技能被哪些 Agent 使用 |
| `skill_install` / `skill_update` / `skill_remove` | 代理到 skill-layer |
| `skill_market_search` / `skill_market_install` | 市场 |
| `agent_skills_set` | 保存某 Agent 的技能/集合选择 |
| `agent_skill_status` | 缺失/冲突/可用 |
| `skill_link_status` | 全局 CLI 链接状态 |

### 11.3 AgentOS UI

**Agents 页（核心入口）**

- 身份 / 引擎 / 模型 / **技能** / MCP / 权限。
- 技能选择器：全局库，多选，搜索；Backend / Frontend / iOS 集合一键选；显示已选数量、不可用徽标。
- 保存到 `agent_skills`；不复制文件。
- 列表行显示技能数/不可用数。

**Skills 页（全局库）**

- 全部技能、来源、版本、哈希、许可、更新。
- 「被哪些 Agent 使用」；全局 CLI 链接状态。
- 安装/市场/迁移/冲突。
- 不再按引擎分页。

**市场页**

- 搜索、预览、安装；安装后提示「去 Agents 页启用」。
- 显示来源/许可/脚本/哈希。

---

## 12. 分阶段

| Phase | 内容 | 估算 |
| --- | --- | --- |
| P0 | 验证 Claude/Codex/OpenCode/Gemini 的 per-session 技能目录；两个 Claude Agent 不同技能集 spike | 3–5 天 |
| P1 | `skill-layer` daemon：store、版本、registry、materialize、client、全局链接；单测 | 2–3 周 |
| P2 | 安装（Git/本地/归档/URL）、校验、更新/卸载、旧技能迁移、agent_skills 展开 | 2–3 周 |
| P3 | Agents 页技能选择器 + cli-layer 接线；后端/前端/iOS 三 Agent 并发验收 | 2 周 |
| P4 | 市场（skills.sh/Claude 市场）、更新检查、安全硬化、打包 | 2–3 周 |

合计 **8–12 周**（1 人）；只做 P0–P3 的核心（全局库 + 会话注入 + Agents 选择）约 5–7 周。

---

## 13. 验收

- 全局只有一份技能；`~/.agents/skills` 可见；CLI 全局链接指向它。
- 后端（Claude）、前端（Codex）、iOS（Claude）三个 Agent 并发，各只看到自己的技能；两个 Claude 不串。
- Agents 页选择/取消技能后，新会话生效；运行中的会话不变（pin hash）。
- 全局终端裸跑 CLI 能看到全部技能（默认）。
- 安装/更新/卸载/回滚；冲突/只读/损坏可见。
- 市场搜索/预览/安装；来源/许可/哈希/脚本可见。
- 安全用例：路径穿越、软链逃逸、zip bomb、1000 文件、密钥扫描。
- `cli-layer` skills 501 消失；`engine-api.md` 登记新命令。
- daemon 重启后 registry/链接状态一致；session home 可回收。

---

## 14. 待确认

1. `skill-layer` 是否确定 daemon？（建议 daemon；library 仅适合 cli-layer 单宿主）
2. OpenCode / Gemini 是否有 per-session 技能目录？没有的话这两个 CLI 不做 per-Agent 技能。
3. 集合（Backend/Frontend/iOS）由 skill-layer 管，还是 cli-layer 管？（建议 skill-layer 存集合，cli-layer 存 Agent 绑定）
4. Agent 是否允许 pin 技能版本？（建议允许，默认当前）
5. 项目级技能是否第一期做？（建议后置）
6. 插件包（agents/commands/hooks/MCP）是否 v1 支持？（建议只 skills）
7. 技能脚本执行策略：只展示 + 用户确认，还是禁止？（建议只展示）
8. 许可：NOASSERTION/source-available 是否允许安装？（建议警告 + 确认）
9. 是否识别 `npx skills` 的 lock file 以便导入？（建议只读识别）
10. 旧核心过渡期：继续用 `acp_*_agent_skill`，还是代理到 skill-layer？（已统一到 cli-layer）

---

## 15. 规则变更（实现时同步）

| 文件 | 变更 |
| --- | --- |
| `AGENTS/truth-sources.md` | 技能内容 = `~/.agents/skills`（+ `skill-store` 版本）；registry = `~/.agents/skill-layer/registry.sqlite`；CLI 目录只是全局链接 |
| `AGENTS/agent-model.md` | Agent 引用 skill ID/集合；技能不再按引擎各存一份；技能缺失显式提示 |
| `AGENTS/bodies.md` | CLI 技能目录是 session home 的软链目标；不直接写全局 |
| `AGENTS/agentos-boundary.md` | 技能文件/链接/校验归 `cli-layer`/`skill-layer`；壳只渲染 |
| `AgentOS/engine-api.md` | 登记 `skill_*` / `agent_skills_set`；旧 `acp_*_agent_skill` 标过渡 |
| `AGENTS/architecture.md` | 增加 `skill-layer` 作为技能基础设施；与 model-layer 并列 |

---

## 16. 参考

- Agent Skills 规范：https://agentskills.io
- Anthropic skills / 模板 / 市场：https://github.com/anthropics/skills
- Vercel skills CLI：https://github.com/vercel-labs/skills
- Skills Manager（macOS，Library→Collection→Mount）：https://github.com/yibie/skills-manager
- Skills Manager（跨平台中央库）：https://github.com/xingkongliang/skills-manager
- Skills Manager（32 工具软链）：https://github.com/jiweiyeah/Skills-Manager
- 多 harness 插件市场：https://github.com/wshobson/agents
- 官方社区市场：https://github.com/anthropics/claude-plugins-community
- 企业 registry：https://github.com/iflytek/skillhub
- 自托管资源 registry：https://github.com/Observal/Observal
- 发现聚合：https://github.com/davepoon/buildwithclaude
- 技能目录：https://skills.sh


---

## 17. 后端核心进展（2026-10-23）

`skill-layer/` 已落地第一版 daemon（`crewchat-skill`，`:3460`）：

- SQLite registry：skills / versions / collections / global links。
- 全局库 `~/.agents/skills` + 版本原件 `~/.agents/skill-store/<id>/<hash>`。
- 安装源：本地目录、Git/GitHub/GitLab/Azure、zip/tar.gz/tar、直链 `SKILL.md`。
- `check` / `update` / `remove`；`materialize`（只链选中技能、hash pin、session root）。
- 全局 per-CLI 链接同步（claude_code / codex / open_code / gemini）。
- 集合 CRUD（含删除）。
- 7 个测试、clippy、HTTP 冒烟（本地/Git/归档安装、全局链接、update、remove）。

未做（当时）：市场、按技能 × CLI 的全局开关、AgentOS 接线、真实对话调用验收。
后续状态与计划见 §18。

---

## 18. 现状与后续计划（2026-10-23，skill 暂停）

### 18.1 当前现状

**已落地**

- `skill-layer` daemon：全局库、版本库、注册表、安装/检查/更新/删除、集合、全局 per-CLI 链接、session materialize。
- `cli-layer`：已接 `skill_list` / `skill_read` / `skill_install` / `skill_check` / `skill_update` / `skill_remove` / `skill_link_status` / `agent_skills_set` / `agent_skill_status`；`acp_connect` 前把 Agent 选中的技能 materialize 到该角色的私有 home。
- Agent 绑定：`cli-layer.db` 的 `agent_roles.skills_json`；集合在 `agent_skills_set` 时展开成 skill IDs。
- AgentOS：Skills 页已通过 `skill_list` 读取；Agents 页技能选择器由前端 Agent 继续收口。
- 数据：本机已导入 81 个技能；`gstack` 因 3519 个文件 / 70MB 超过 `skill-layer` 限制未导入。
- 验证：`skill-layer` 7 个测试、`cli-layer` 18 个测试通过；Claude / Codex / OpenCode 真实会话已跑通。**真实对话中让 Agent 调用 skill 还没验收**。

**已知限制**

- 市场搜索/安装未接。
- 全局链接只能整体 sync，不能按「技能 × CLI」单独关闭；状态目前主要是 `linked` / `foreign`。
- 没有垃圾回收、备份、审计、技能脚本执行策略。
- 没有「这个技能被哪些 Agent 使用」的反向查询。
- 大技能（数千文件 / 数十 MB）会被安装限制挡住；`gstack` 是现成案例。
- 两个同 CLI Agent 的技能隔离有单元/集成覆盖，但还没有真实并发对话验收。

### 18.2 后续计划

**P0（CLI 收口后立刻做）：真实对话验收**

1. Claude / Codex / OpenCode 各选一个 Agent，只挂 1 个技能，确认对话里能发现并调用。
2. 两个 Claude Agent 并发，技能集不同，确认 `CLAUDE_CONFIG_DIR` 隔离、互不串味。
3. 缺失技能：Agent 会话应明确报错/降级，不静默拿到别的技能。
4. 全局 `~/.claude/skills` 等终端链接不参与 Agent 会话。

**P1：前端收口（其他 Agent）**

- Skills 页：列表/详情/安装/更新/删除/全局链接状态。
- Agents 页：技能多选、集合、不可用标记、数量。
- 错误态：skill-layer 未启动、409 missing、foreign link、超限技能。

**P2：工程化**

- 按技能 × CLI 的全局链接开关。
- 删除后的版本 GC / trash 清理；备份/恢复。
- 大技能分片/流式安装，或明确上限与提示。
- 市场（skills.sh / Claude 市场）与来源校验。
- 「被哪些 Agent 使用」查询与审计。

**P3：可选**

- 项目级技能、插件包（agents/commands/hooks）、语义搜索。

### 18.3 暂停规则

- 在 P0 通过前，`skill-layer` 不再加新功能；只修真实会话暴露的 bug。
- 任何 API/行为变更同步更新 `docs/plans/skill-layer-api.md` 与 `skill-layer/README.md`。
- MCP 作为独立层推进，不把 MCP 逻辑塞进 `skill-layer`。
