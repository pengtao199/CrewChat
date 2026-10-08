# 界面

## 适用范围

新建或修改 React 组件、设置页、样式、文案，或迁移上游页面时。

## 目标

五个设置页（Agent / CLI 身体 / Model / Skills / Tools）与所有后继页面共享同一套骨架和组件，改一处全局生效。

## 上游硬性规范（fork 后同样生效）

来自 `ui/AGENTS.md`，以下两条是**阻断项**，违反即不合格：

| 规则 | 要求 |
| --- | --- |
| 组件库 | 只用 `@arco-design/web-react`；**禁止裸交互 HTML**（`<button>` / `<input>` / `<select>` / `<textarea>`） |
| 国际化 | 新增或修改的用户可见文案必须走 i18n key；**禁止硬编码字符串** |

其余既定规范：

- 图标只用 `@icon-park/react`。
- 颜色只用语义 token（`t-primary` / `bg-1..10` / `border-b-*`，定义在 `ui/uno.config.ts`），禁止硬编码色值。
- 优先 UnoCSS 原子类；复杂样式用 CSS Module。
- 优先 `type` 而非 `interface`；严格模式，无 `any`。

## 组件分层

| 用途 | 位置 | 规则 |
| --- | --- | --- |
| 通用原语（无业务） | `renderer/components/base/` | 不得依赖业务逻辑 |
| 业务共享（≥2 页面用） | `renderer/components/<domain>/` | 按业务域分目录 |
| 单页私有 | `renderer/pages/<Page>/components/` | 起点在这里；第二个消费者出现才上移 |

目录根 ≤ 10 个直接子项；目录型组件必须有 `index.tsx` 作为唯一入口。

## 共享组件（统一骨架的载体）

这些组件已经存在，**新页面必须复用，不得再复制类名**：

| 组件 | 位置 | 作用 |
| --- | --- | --- |
| `SettingsPageHeader` | 上游已有 | 标题 + 说明 + 右侧操作 |
| `SettingsCardList` | `components/base/SettingsCardList.tsx` | 卡片列表容器（此前同一个类名被复制 6 次） |
| `SettingsCardRow` | `components/base/SettingsCardRow.tsx` | 列表行：图标 + 标题 + 副文本 + 右侧操作；`onClick` 用 div，操作按钮用 Arco `Button` |
| `StatusBadge` | `components/base/StatusBadge.tsx` | 四档状态语义词：`ready` / `warning` / `blocked` / `neutral` |
| `EmptyState` | `components/base/EmptyState.tsx` | 空态：居中灰字 |
| `DataLocationNote` | `components/settings/DataLocationNote.tsx` | 「保存到 → 路径」，`shared` 标记跨工具共享 |

**迁移状态**：Agent 编排页已全面使用上述组件；`CLI 身体` / `模型` / `技能` / `工具` 四个上游页面仍各自复制类名，**在改动它们时顺带迁移**（不为此单独开任务）。

## 骨架模板

```
SettingsPageHeader(标题, 说明, 右侧操作：[刷新] [+ 添加])
└── AionScrollArea(flex-1 min-h-0)
     └── SettingsCardList
          ├── SettingsCardRow × N
          └── EmptyState（无数据时）
页脚：DataLocationNote(真相源路径)
```

编排型页面（Agent）在右侧使用分组卡片，每一组对应领域模型的一个组成。

## 领域规则（上游没有，我们必须守）

- **状态三层**：任何资源都按「存在 → 可用 → 被引用」表达，不用各页各说（"可用/不可用/就绪/缺 key"混用）。
- **受约束选择**：编排页的选择器只能选**可用**的资源；不可用的显示但禁用，并给出原因与指引。
- **引用可见**：资源行显示「被 N 个 Agent 引用」；删除被引用的资源要阻止或明确警告。
- **数据位置可见**：会写文件的页面标注「保存到 → 路径」，并说明是否跨工具共享。
- **实时优先**：可选值应来自实际探测/请求（如模型列表请求端点），不依赖用户手输。

## 禁止项

- 不在页面里复制粘贴卡片/列表/空态类名；抽组件或复用已有组件。
- 不新增内联 `style={{}}`（除动态计算值），布局用原子类。
- 不把上游用语（`assistant`、`backend`、`acp`）暴露给最终用户。
- 不为单个页面另起一套视觉语言（自定义按钮尺寸、自造颜色）。

## 检查

- 页面是否用了 `SettingsPageHeader` + `SettingsCardList` + `SettingsCardRow`？
- 有没有裸 HTML 交互元素？有没有硬编码文案或颜色？
- 不可用的资源在选择器里是禁用的吗？原因可见吗？
- 页面是否标注了数据写入位置？
