# Skill 层：统一管理、注册与安装

Updated: 2026-10-23
Status: Proposed（调研完成，未开工）
关系：`cli-layer-migration.md` 的 capabilities/skills；`model-layer` 不感知技能；`AgentOS` 只渲染。
目标读者：实现 `skill-layer`、改 `cli-layer` 技能命令、重做 AgentOS Skills 页的人。

---

## 0. 结论

- 技能只保留**一份**：`~/.agents/skills/<id>/`；注册表 `~/.agents/skill-layer/registry.sqlite`。
- 每个 CLI 通过**逐技能软链接**看到它：`~/.claude/skills/<id>` → `~/.agents/skills/<id>`，Codex / OpenCode / Gemini 同理；默认所有检测到的 CLI 都链接，允许按技能/CLI 关闭。
- `skill-layer` 做成 **Rust library + CLI**，由 `cli-layer` 进程内调用；**不新增常驻 sidecar**。文件/软链操作不需要第三个 daemon；如果过渡期 codeg 也要用，再加薄 HTTP shim。
- 格式采用 **Agent Skills**（`SKILL.md` + YAML frontmatter）；安装源支持 Git / 本地目录 / 压缩包 / URL / 市场；市场先接 **skills.sh** 与 Claude 插件市场清单，v1 只装 skills，不装 agents/commands/hooks/MCP。
- 先做「扫描 + 迁移 + 软链 + 注册表」，再做「安装/更新/卸载」，最后做「市场 + UI」。

---

## 1. 目标与边界

### 目标

- 技能在本机只有一份，所有 CLI 共享；不再出现「Claude 一份、Codex 一份、OpenCode 一份」。
- 注册：每个技能有稳定 ID、来源、版本/commit、内容哈希、许可、安装时间。
- 安装/更新/卸载：从 Git、GitHub/GitLab/Azure、本地目录、zip/tar、URL、市场安装；可回滚。
- 每 CLI 可见性：默认全开；可按技能 × CLI 关闭；系统内置技能只读。
- 市场：搜索、预览、安装、更新；显示来源、许可、脚本、哈希。
- 旧技能迁移：把现有各 CLI 目录里的技能收敛到 `~/.agents/skills`，原位置留软链；可 dry-run、可回滚。

### 不做什么

- 不做 Agent、模型路由、MCP、会话；这些分别归 `cli-layer` / `model-layer`。
- v1 不安装插件里的 agents / commands / hooks / MCP，只装 skills；插件包后续再评估。
- 不在安装时执行技能里的脚本；只落盘、标记 `has_scripts`、展示给用户。
- 不做 Windows（macOS 优先）；不做云同步/团队 RBAC。
- 不复制 Anthropic 的 source-available 文档技能；只引用来源。

---

## 2. 现状

### 2.1 技能散落在各 CLI

| CLI | 技能目录（全局） | 备注 |
| --- | --- | --- |
| Claude Code | `~/.claude/skills` | 不读 `~/.agents/skills` |
| Codex | `~/.codex/skills` + `~/.codex/skills/.system` | `.system` 是内置只读 |
| OpenCode | `$XDG_CONFIG_HOME/opencode/skills` 或 `~/.config/opencode/skills` | 也读 `~/.agents/skills` |
| Gemini CLI | `~/.gemini/skills` | 也读 `~/.agents/skills` |
| 共享 | `~/.agents/skills` | Codex/OpenCode/Gemini 已把它当共享根 |

现状问题：

- 同名技能在不同 CLI 下有多份，内容可能已经分叉；
- 没有统一的来源/版本/哈希，更新靠手工；
- codeg 的 `acp_list_agent_skills` / `acp_save_agent_skill` 是**按引擎**操作文件，AgentOS 的 Skills 页也是按引擎看，无法回答「这个技能装没装、哪些 CLI 在用」；
- `cli-layer` 的 skills 目前是 501（Phase 3）；
- `custom_skills` / skill-packs 是另一套存储，和 CLI 目录没有统一注册表。

### 2.2 已有可复用事实

- `.agents/skills` 已经是 Codex/OpenCode/Gemini 的共享根；把 Claude 也接进来，主要是补软链。
- 当前能读到 82（OpenCode）/97（Codex）条技能，说明共享目录已经承载了大量内容。
- 软链方案在本机是可行的：Claude/Codex/OpenCode 都按目录扫描，不要求技能是真实目录。

---

## 3. 开源调研

### 3.1 结论先行

- **「中央库 + 逐技能软链」已经被多个成熟项目验证**，不是我们发明的：`vercel-labs/skills`、`xingkongliang/skills-manager`、`jiweiyeah/Skills-Manager`、`yibie/skills-manager` 都是这个模型。
- **Agent Skills 格式**（`SKILL.md` + `name`/`description`）是事实标准；Claude / Codex / OpenCode / Gemini 都兼容。
- **市场**已经有现成发现源：`skills.sh`、`anthropics/claude-plugins-community`（`.claude-plugin/marketplace.json`）、`wshobson/agents`、`buildwithclaude`。
- **企业级 registry**（`iflytek/skillhub`、`Observal`）功能全但太重（Docker/K8s、RBAC/审计），不适合桌面 App 的第一版；可作为未来团队版参考。
- **不要直接依赖 `npx skills`**（Node 22 + 遥测）；镜像它的**安装源格式、软链/复制策略、体积限制**，用 Rust 原生实现。

### 3.2 项目对比

| 项目 | Stars | 许可 | 关键能力 | 我们取什么 |
| --- | ---: | --- | --- | --- |
| [Agent Skills 规范](https://agentskills.io) / [anthropics/skills](https://github.com/anthropics/skills) | 183k | 混合（多为 Apache-2.0；doc 技能 source-available） | `SKILL.md` + frontmatter；模板；插件市场 | 格式、校验、模板；**不复制 doc 技能** |
| [vercel-labs/skills](https://github.com/vercel-labs/skills) | 36.2k | MIT | `npx skills add/find/update/remove`；80+ agent；project/global；symlink/copy；Git/GitLab/Azure/URL/本地；下载 10MiB/解压 25MiB/1000 文件 | 安装源、体积限制、软链/复制策略、`skills.sh` 发现 |
| [yibie/skills-manager](https://github.com/yibie/skills-manager) | 459 | MIT | 原生 macOS SwiftUI/SwiftData；Library → Collection → Mount；软链；稳定 skill ID；provider ownership/recovery；冲突状态；skills.sh 发现（不需要 npx） | **最接近的产品模型**；ID/状态/冲突/回滚设计 |
| [xingkongliang/skills-manager](https://github.com/xingkongliang/skills-manager) | 6.7k | MIT | 中央库 `~/.skills-manager`；50+ 工具；symlink/copy；市场；presets；更新跟踪；备份/同步 | 中央库、每 agent 开关、presets、更新 |
| [jiweiyeah/Skills-Manager](https://github.com/jiweiyeah/Skills-Manager) | 1.0k | MIT | 中央库；32 工具软链；市场；Windows junction | 软链矩阵、Windows 降级策略 |
| [wshobson/agents](https://github.com/wshobson/agents) | 41.6k | MIT | 多 harness 插件市场：94 plugins / 185 skills；Claude/Codex/Cursor/OpenCode/Antigravity/Copilot/Pi | 市场清单格式、多 harness 安装差异 |
| [anthropics/claude-plugins-community](https://github.com/anthropics/claude-plugins-community) | 4.7k | Apache-2.0 | 官方社区市场只读镜像；`.claude-plugin/marketplace.json` | 官方市场发现源 |
| [iflytek/skillhub](https://github.com/iflytek/skillhub) | 5.3k | Apache-2.0 | 自托管 registry；发布/版本；RBAC/审计；Docker/K8s | 版本/审计模型；**不采用其部署形态** |
| [Observal](https://github.com/Observal/Observal) | 4.5k | Apache-2.0 | 自托管 Skills/MCP/Agents registry；ARD 标准 | 资源注册/发现思路；**过重** |
| [davepoon/buildwithclaude](https://github.com/davepoon/buildwithclaude) | 3.7k | MIT | Skills/Agents/Commands/Hooks/Plugins 发现 | 发现源候选 |

### 3.3 关键事实

- `vercel-labs/skills` 默认把技能安装到 **project**（`./<agent>/skills/`），`-g` 才是用户目录；安装方式可选 **symlink（推荐）** 或 **copy**；这验证了「canonical copy + agent links」。
- `yibie/skills-manager` 把模型抽象成 **Library → Collection → Mount**：Library 是受管来源，Collection 是可复用分组，Mount 是软链到 agent；卸载只删链接，不删 Library；并保留 missing/shared/partially applied/diverged 状态。这正是我们要的「一份 + 多 CLI」。
- `xingkongliang/skills-manager` 和 `jiweiyeah` 都用中央库 + 软链；前者默认 `~/.skills-manager`，后者支持 32 工具；都带市场。
- `wshobson/agents` 说明多 harness 的差异不在技能内容，而在**安装位置和格式适配**；技能本身是共享 Markdown。
- `anthropics/claude-plugins-community` 的市场清单是 `.claude-plugin/marketplace.json`，由 Claude 侧审核后镜像；可作为官方发现源，但安装仍要走我们的校验。
- `skills.sh` 是多个管理器共用的发现层；`vercel-labs/skills` 的 `find` 与 `yibie` 的 Discover 都用它。需要先确认它的公开 API/条款，再决定是直连还是缓存索引。

---

## 4. 设计决策

| 决策 | 选择 | 理由 |
| --- | --- | --- |
| 规范 | Agent Skills（`SKILL.md` + frontmatter） | 事实标准；Claude/Codex/OpenCode/Gemini 都认 |
| 中央库 | `~/.agents/skills/<id>/` | 用户指定；已在 Codex/OpenCode/Gemini 共享 |
| 元数据 | `~/.agents/skill-layer/registry.sqlite` | 不污染技能目录；隐藏；可备份 |
| CLI 接入 | **逐技能软链**，默认所有检测到的 CLI | 支持按技能/CLI 开关；不覆盖系统技能；不整目录替换 |
| 模块形态 | `skill-layer` Rust library + CLI，`cli-layer` 进程内调用 | 无协议翻译、无长连接；不值得再加一个 daemon |
| 安装源 | Git / 本地 / zip / tar / URL / 市场 | 覆盖 `npx skills` 的主要来源 |
| 市场 | v1 skills-only；skills.sh + Claude 市场清单 | 先解决「找到一个技能并装进来」，插件包后置 |
| 更新 | 默认手动；pin ref/hash；更新前 diff | 技能是可执行上下文，不能静默换内容 |
| 作用域 | 先全局；项目级后置 | 用户要求「本机隐藏 .agents」；项目级需要 CLI 各自的项目目录 |

### 4.1 为什么逐技能软链，而不是整目录软链

整目录软链（`~/.claude/skills` → `~/.agents/skills`）最简单，但：

- 不能按 CLI 关闭某个技能；
- 会覆盖/遮蔽 CLI 自己的内置技能（Codex `.system`）；
- 某个 CLI 的目录可能还有非本层管理的文件；
- 冲突时无法保留「某 CLI 用旧版、其他用新版」。

逐技能软链：`~/.claude/skills/<id>` → `~/.agents/skills/<id>`。默认给所有检测到的 CLI 建链；关闭时只删那条链，中央库不动。整目录已经是软链的情况，扫描时识别并改为逐技能管理或标记为 foreign，不重复建链。

### 4.2 为什么不做独立 daemon

- 技能操作是文件系统 + 网络下载，不是长连接/协议转换；
- `cli-layer` 已经是常驻核心，直接把 `skill-layer` 作为 crate 依赖即可；
- AgentOS 通过 `cli-layer` 的 HTTP 命令访问，不需要知道文件路径；
- 过渡期 codeg 仍可继续用它自己的 skills 命令；等 `cli-layer` 切换后再统一。若 codeg 确实需要共享，另加一个只读 HTTP shim，不默认常驻。

---

## 5. 数据模型

### 5.1 文件布局

```text
~/.agents/
├── skills/
│   ├── <skill-id>/
│   │   ├── SKILL.md
│   │   ├── scripts/
│   │   └── assets/
│   └── ...
└── skill-layer/
    ├── registry.sqlite
    ├── backups/<timestamp>/
    └── trash/
```

### 5.2 registry.sqlite

```sql
skills(
  id TEXT PRIMARY KEY,            -- frontmatter name / 稳定 slug
  name TEXT NOT NULL,
  description TEXT,
  version TEXT,
  source_kind TEXT,               -- git | github | local | archive | url | marketplace | legacy
  source_url TEXT,
  source_ref TEXT,                -- branch/tag/commit
  source_subdir TEXT,
  commit_sha TEXT,
  content_hash TEXT NOT NULL,     -- 目录树哈希（排除 registry/备份）
  license TEXT,
  installed_at TEXT,
  updated_at TEXT,
  read_only INTEGER DEFAULT 0,    -- CLI 内置/系统技能
  has_scripts INTEGER DEFAULT 0,
  size_bytes INTEGER,
  metadata_json TEXT              -- frontmatter 未知字段、平台、标签
);

skill_links(
  skill_id TEXT,
  agent_type TEXT,                -- claude_code | codex | open_code | gemini | ...
  path TEXT,                      -- 实际软链路径
  state TEXT,                     -- linked | disabled | missing | broken | foreign
  linked_at TEXT,
  PRIMARY KEY(skill_id, agent_type)
);

agents(
  agent_type TEXT PRIMARY KEY,
  display_name TEXT,
  skills_dir TEXT,                -- 全局目录
  project_dir TEXT,               -- 项目目录模板（后置）
  supports_symlink INTEGER,
  system_dirs_json TEXT,          -- 只读/内置目录
  enabled INTEGER
);

operations(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT,                      -- install | update | remove | link | unlink | migrate
  skill_id TEXT,
  agent_type TEXT,
  detail_json TEXT,
  created_at TEXT
);
```

### 5.3 ID 与冲突

- ID 取 frontmatter `name`（必须小写、连字符）；缺失时用目录名 slug。
- 同名不同内容：不覆盖；保留 `<id>` 与 `<id>-2`，registry 记录 `conflict_of`；UI 标「冲突」，让用户选。
- 同名同内容：视为同一技能，合并链接。
- 内置/系统技能：`read_only=1`，不迁移、不建链、不删除。

---

## 6. CLI 接入矩阵（首版）

| CLI | 全局技能目录 | 链接方式 | 备注 |
| --- | --- | --- | --- |
| Claude Code | `~/.claude/skills` | 逐技能软链 | 不读 `.agents`，必须链 |
| Codex | `~/.codex/skills` | 逐技能软链 | `.system` 只读；不碰 |
| OpenCode | `~/.config/opencode/skills`（XDG） | 逐技能软链 | 也读 `.agents/skills`；避免重复 |
| Gemini CLI | `~/.gemini/skills` | 逐技能软链 | 也读 `.agents/skills` |
| 其他（Cline/Cursor/Kimi/Pi/…） | 按 registry 补齐 | 软链或复制 | 有些 CLI 可能不接受软链；先探测，失败再复制并标记 |

默认行为：

- 启动时扫描已安装 CLI；新装技能默认链接到所有检测到的 CLI。
- 每个技能卡片显示 CLI 徽标；点徽标开/关；关闭只删链。
- 新检测到 CLI 时，默认把中央库技能链过去（可设置改为询问）。
- 某 CLI 的目录是整目录软链或 foreign 时，不重复建链，标 `foreign` 让用户处理。

---

## 7. 安装 / 更新 / 卸载 / 迁移

### 7.1 安装源

| 来源 | 示例 | 说明 |
| --- | --- | --- |
| GitHub shorthand | `owner/repo` | 先 API/`gh`，再 git clone |
| Git URL | `https://…/repo.git`、`git@…` | 私有库走本机 git 凭据，不落库 |
| 子目录 | `…/tree/main/skills/foo` | 只取该目录 |
| 本地目录 | `~/src/my-skill` | 复制到中央库 |
| 归档 | `.zip` / `.tar` / `.tar.gz` / `.tgz` | 解压前校验 |
| 直链 | `https://…/my-skill` | 单 `SKILL.md` 或归档 |
| 市场 | skills.sh / Claude 市场条目 | 解析到上述来源 |

### 7.2 校验（安装前）

- 必须存在 `SKILL.md`；frontmatter 必须有 `name`、`description`。
- 拒绝绝对路径、`..`、越界软链、设备文件。
- 归档限制：下载 10 MiB、解压 25 MiB、最多 1000 文件（可配置；参考 vercel）。
- 计算目录哈希；记录 license（frontmatter/仓库 LICENSE）。
- 扫描常见密钥模式（`sk-`、`ghp_`、AWS key 等）；命中则警告，不阻断但要求确认。
- 列出 scripts/assets，标 `has_scripts`；**安装时不执行**。

### 7.3 安装 / 更新 / 卸载

```text
install: 下载/读取 → 临时目录 → 校验 → 移到 ~/.agents/skills/<id>
         → 写 registry → 给默认 CLI 建链 → 记录 operation
update:  按 source 重新拉取 → 计算新 hash → 展示 diff/文件变化
         → 备份旧版本 → 原子替换 → 链接不变 → 更新 registry
remove:  删除各 CLI 链接 → 中央库移到 trash（或确认永久删除）→ 更新 registry
enable:  建链；disable: 删链；都不动中央库
```

- 更新默认**手动**；`check` 只提示，不自动替换。
- 备份放 `~/.agents/skill-layer/backups/<timestamp>/`；trash 保留 N 天。
- 操作日志进 `operations`，UI 可看「谁在什么时候装了什么」。

### 7.4 旧技能迁移

1. 扫描：`~/.agents/skills`、四个 CLI 目录、`custom_skills`/skill-packs（如果可读）。
2. 按目录哈希 + 名称去重；生成 dry-run 报告：
   - 已在中央库且一致 → 建链；
   - 仅在某 CLI → 复制到中央库，原位置备份后改为软链；
   - 同名不同内容 → 保留两份，标冲突；
   - 系统/只读 → 跳过，标 `read_only`；
   - 未知/损坏 → 跳过并报告。
3. 用户确认后执行；先备份整个技能目录清单和软链状态。
4. 幂等：重复跑不重复复制；失败可回滚。
5. 迁移完成后，AgentOS Skills 页从「按引擎」切到「统一库 + CLI 徽标」。

### 7.5 回滚

- registry 快照 + 备份目录；`skill-layer rollback <timestamp>` 恢复文件与链接。
- 迁移/安装前记录原路径；回滚时把原文件放回、删除新建的链。

---

## 8. 市场

### 8.1 发现源

| 源 | 形态 | 用途 |
| --- | --- | --- |
| [skills.sh](https://skills.sh) | 公开目录/搜索 | 主发现源；先确认 API/条款，失败则缓存索引 |
| [anthropics/claude-plugins-community](https://github.com/anthropics/claude-plugins-community) | `.claude-plugin/marketplace.json` | 官方社区插件/技能 |
| [wshobson/agents](https://github.com/wshobson/agents) | 多 harness 市场清单 | 大量 skills/plugins |
| [davepoon/buildwithclaude](https://github.com/davepoon/buildwithclaude) | 聚合目录 | 发现补充 |
| GitHub topic/search | `agent-skills` / `claude-skills` | 兜底 |

### 8.2 市场条目

- 名称、描述、来源、作者、stars、license、最近更新。
- 技能列表（一个 repo 可能多个 `SKILL.md`）；选中后预览 `SKILL.md`、scripts/assets、哈希、许可。
- 安装前确认；安装时走 7.2 的校验。
- v1 只装 skills；如果条目是 plugin（含 agents/commands/hooks/MCP），提示「v1 只安装其中的 skills」，或拒绝并给来源链接。

### 8.3 信任与更新

- 展示来源/commit/哈希；支持固定 ref/tag/commit。
- 不自动执行安装脚本；不自动更新。
- 更新时展示文件 diff（至少新增/删除/修改文件列表 + `SKILL.md` diff）。
- 记录 license；`NOASSERTION` / source-available 标黄，要求确认后再装。
- 不把市场内容复制进仓库；只落用户目录。

---

## 9. 安全

技能是**会被 agent 读取并可能执行脚本**的内容，按不可信代码处理。

| 风险 | 措施 |
| --- | --- |
| 路径穿越 / 绝对路径 | 解压后 realpath 必须在临时目录内；拒绝 `..`、绝对路径、设备文件 |
| 软链逃逸 | 归档内软链默认拒绝；已有技能内的软链校验目标在技能目录内 |
| zip bomb / 大文件 | 下载 10 MiB、解压 25 MiB、1000 文件上限；可配置 |
| 脚本执行 | 安装不执行；标 `has_scripts`；UI 展示；用户确认 |
| Prompt injection | 预览 `SKILL.md`；来源/哈希；不静默安装 |
| 密钥泄露 | 扫描常见 key 模式；警告；不把 key 写进 registry |
| 许可 | 记录 SPDX；source-available/NOASSERTION 警告；不复制 Anthropic doc 技能 |
| 供应链 | pin commit/hash；更新需 diff；不自动更新；操作审计 |
| 软链误删 | 只删 registry 里属于本层的链；foreign/broken 不自动删 |

---

## 10. API 与 UI

### 10.1 `cli-layer` 暴露的命令（草案）

| 命令 | 作用 |
| --- | --- |
| `skill_list` | 统一库 + 每个技能的 CLI 链接状态 |
| `skill_get` | 读 `SKILL.md`/元数据/文件列表 |
| `skill_install` | 从 Git/本地/归档/URL/市场安装 |
| `skill_update` | 检查/执行更新（默认只 check） |
| `skill_remove` | 删链 + 移入 trash |
| `skill_set_enabled` | 技能 × CLI 开/关 |
| `skill_link_status` | 某 CLI 的 missing/broken/foreign |
| `skill_market_search` | 市场搜索 |
| `skill_market_install` | 市场条目安装 |
| `skill_scan_import` | 扫描 + dry-run 迁移 |
| `skill_migrate` | 执行迁移 |
| `skill_export` | 导出清单/备份 |

### 10.2 AgentOS Skills 页（重做）

- 左侧：全部技能（搜索、来源、标签、只读/冲突/可更新徽标）。
- 右侧：`SKILL.md` 预览/编辑、来源/版本/哈希/许可、文件列表、CLI 徽标开关。
- 顶部：安装（来源/市场）、检查更新、迁移、刷新。
- 不再按引擎分页；引擎只是「这个技能链接到哪些 CLI」的维度。
- 市场页：搜索、预览、安装；显示信任信息。
- 冲突页：同名不同内容、foreign/broken 链接，逐条处理。

### 10.3 过渡期

- `cli-layer` 的 skills 501 由 `skill-layer` 实现后填上。
- codeg 仍可用现有 `acp_*_agent_skill`；切换前两边都指向同一份 `~/.agents/skills`，避免分叉。
- AgentOS 的 Skills 页先接 `cli-layer` 的新命令；旧按引擎视图保留一个只读入口，迁移完成后删。

---

## 11. 分阶段

| Phase | 内容 | 估算 |
| --- | --- | --- |
| P0 | 规范/目录盘点、dry-run 迁移报告、软链可行性验证（4 个 CLI） | 3–5 天 |
| P1 | `skill-layer` crate：store、registry、link manager、CLI（list/link/enable/disable） | 1–2 周 |
| P2 | 安装：Git/本地/归档/URL、校验、备份、更新/卸载；迁移执行 | 2–3 周 |
| P3 | 市场：skills.sh + Claude 市场清单、搜索/预览/安装/更新 | 2–3 周 |
| P4 | AgentOS Skills 页重做、cli-layer 接线、安全硬化、打包 | 2–3 周 |

合计 **7–11 周**（1 人）；UI/市场可与 store 并行。只做「迁移 + 软链 + 本地安装」约 3–4 周。

---

## 12. 验收

- `~/.agents/skills` 是唯一内容源；四个 CLI 都能看到同一技能；关闭某 CLI 只删链。
- 旧技能迁移：内容不丢、重复合并、冲突显式、系统技能只读、dry-run 可回滚。
- 安装：Git/本地/归档各一条；更新可 diff；卸载只删链+trash。
- 市场：搜索 → 预览 → 安装 → 校验 → 链接；license/hash/scripts 可见。
- 安全：路径穿越、软链逃逸、zip bomb、1000 文件、密钥扫描用例通过。
- AgentOS Skills 页不再按引擎分页；CLI 徽标反映真实链接状态。
- `cli-layer` 的 skills 501 消失；`engine-api.md` 登记新命令。
- 迁移/安装/回滚有操作日志。

---

## 13. 待确认

1. `skill-layer` 是 library 还是独立 daemon？（建议 library + CLI，嵌入 `cli-layer`）
2. 逐技能软链 vs 整目录软链？（建议逐技能；整目录只做兼容识别）
3. `skills.sh` 是否有稳定公开 API/条款？没有就做缓存索引 + GitHub 兜底。
4. 项目级技能是否第一期做？（建议后置；先全局）
5. 插件包（agents/commands/hooks/MCP）是否要支持？（v1 只 skills）
6. 技能内脚本的执行策略：只展示 + 用户确认，还是完全禁止？（建议只展示）
7. 许可策略：`NOASSERTION` / source-available 是否允许安装？（建议警告 + 确认）
8. 是否导入 `npx skills` 的 lock file（v3）以兼容已装技能？（建议支持只读识别）
9. 12 个长尾 CLI 的软链兼容性需要逐个验证。
10. 迁移时的冲突命名/去重规则需要和用户确认一次。

---

## 14. 规则变更（实现时同步）

| 文件 | 变更 |
| --- | --- |
| `AGENTS/truth-sources.md` | 技能内容 = `~/.agents/skills`；registry = `~/.agents/skill-layer/registry.sqlite`；各 CLI 目录只是软链 |
| `AGENTS/agent-model.md` | Agent 引用技能 ID，不再按引擎各存一份；只读/系统技能分离 |
| `AGENTS/bodies.md` | CLI 技能目录是软链目标；不直接写文件 |
| `AGENTS/agentos-boundary.md` | 技能文件/链接/校验归 `cli-layer`/`skill-layer`；壳只渲染 |
| `AgentOS/engine-api.md` | 登记 `skill_*` 命令；旧 `acp_*_agent_skill` 标过渡 |

---

## 15. 参考

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
