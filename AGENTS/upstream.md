# 上游同步

## 适用范围

需要跟进 AionUi 上游修复、对比上游实现、修改 `ui/` 下的上游文件时。

## 目标

在复用上游前端的同时，保持可回退、可对比、可升级，不让 fork 变成无法维护的黑盒。

## 背景

`ui/packages/desktop/src/{renderer,common}` 与 `ui/uno.config.ts` 等文件 fork 自 [AionUi](https://github.com/iOfficeAI/AionUi)（Apache-2.0）。上游持续活跃，我们的改动越少越利于跟进。

## 原则

- **改动最小化**：能用新增文件解决的不改上游文件；必须改时集中、注释、可辨识。
- **上游文件可辨识**：所有对上游文件的改动加 `[crewchat]` 前缀注释，便于搜索与升级时重放。
- **命名空间隔离**：我们的后端接口用 `/api/crewchat/*`，我们的页面放在 `pages/settings/AgentComposer/`，不占用上游既有路径。
- **署名保留**：`ui/NOTICE.md` 记录来源与改动清单；`ui/LICENSE` 保留原许可证。

## 参考仓库

上游参考放在 `Reference/`（只读）：以 sparse clone 获取，避免整仓体积（上游全量约 679MB）。

```
# 只取代码，不取资源
git clone --filter=blob:none --sparse --depth 1 <upstream> aionui-ref
cd aionui-ref && git sparse-checkout set packages/ public/ scripts/
```

## 具体规则

- 改上游文件前，先确认能否用「新增文件 + 引用」实现同等效果。
- 上游文件里已改的点登记在 `ui/NOTICE.md`，包括：文件、改动原因、若升级需要重做什么。
- 升级上游时按 `NOTICE.md` 逐条重放，而不是整体覆盖。
- 上游的规范（`ui/AGENTS.md`、`.claude/skills/`）视为本项目的既定规范，见 `AGENTS/ui.md`。
- 上游定义重复处要一并修改（例如设置导航同时定义在 `SettingsSider.tsx` 与 `SettingsPageWrapper.tsx`，只改一处会导致整页崩溃）。

## 禁止项

- 不整体替换 `ui/` 目录来"同步最新"；那会丢掉我们的改动。
- 不删除上游版权头与 `@license` 注释。
- 不在上游文件里塞入我们的业务逻辑；我们的逻辑放 `pages/settings/AgentComposer/` 或 `components/` 新增文件。
- 不把上游的 `AGENTS.md` 当作我们的规则文件使用；我们的规则在仓库根 `AGENTS.md`。

## 检查

- 这个改动是新增文件还是改了上游文件？改上游的必要性写下来了吗？
- 改动处是否有 `[crewchat]` 注释，能否被 `rg "\[crewchat\]"` 找到？
- `ui/NOTICE.md` 是否同步更新？
- 如果上游下周改了同一处，我知道怎么重放吗？
