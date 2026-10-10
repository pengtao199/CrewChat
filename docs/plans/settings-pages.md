# 设置页：Agents、引擎、模型

Updated: 2026-10-10 (Asia/Shanghai)
Status: Implemented。浏览器里还没按 Done Criteria 逐项点过。

只覆盖设置里这三个页面的界面和交互。协作循环仍以 [crewchat.md](crewchat.md) 为准，这里不改它。

## Goal

三个页面都能看懂、改得动。模型供应商的名字能保存。同一个供应商下的多个 API Key 各自一行，能备注、能用眼睛查看，不再塞进一个多行输入框。

## Current Facts

页面在 http://127.0.0.1:8000 ，2026-10-10 打开看过。骨架约束在 `AGENTS/ui.md`。

- **模型**（`/settings/model`）：列表在 `ModelModalContent.tsx`，编辑弹窗是 `EditModeModal.tsx`。供应商默认全部展开，自定义一家就有 38 个模型。弹窗里「模型供应商」输入框可打字，但 `EditModeModal` 的 `onChange` 在 `ModelModalContent` 里只调用 `refresh()`，丢弃表单。名字、地址、密钥都不会写入 `PUT /api/providers/{id}`。
- 列表接口把密钥收成 8 个点（`providers_api._masked`）。弹窗把这串点填进 `Input.TextArea`，占位文案是「多个 Key 请每行一个」。没有眼睛。文案 `settings.models.revealKey` 和 `GET /api/providers/{id}/secret` 已存在，页面没接。
- 多个 Key 存在同一个 `api_key` 字符串里，按换行或逗号切开只用于计数（`Provider.key_count`）。网关投影用整段字符串当一个密钥（`gateway_ccswitch.py`），并没有轮询。界面上的「自动轮询」与运行时不符。
- 单个模型的显示名走另一条路：行上的 `…` 打开 `ModelDetailModal`，`display_name` 经 `POST /api/providers/{id}/models` 保存，列表标题用 `Model.label`（有显示名就不用 id）。这次没改到那条。
- **Agents**（`/settings/agents`）：左列副标题是 `身体 · 端点id/模型id`，长了就被截断（例如 `75f800be/deepseek-v4…`）。标识是表情、名称、id 三个框排成一行，没有字段名。新建用 `window.prompt`，未保存切换用 `window.confirm`。大脑的模型下拉是原始 id。规则区下面还有一块只读文本，把角色、目标和背景再拼一遍。
- **引擎**（`/settings/engines`）：已安装 9 个，未安装 0。每行副标题是来源、版本和本机绝对路径连在一起。九行图标都是同一个终端符号。行不能点进已有的修复页（`/settings/engines/:id/repair`）。有新版本的行才出现「更新」。

## Non-Goals

- 不实现多 Key 自动轮询。一个供应商多个账号时，指定一个默认 Key 给网关和对话用。
- 不改模型 id、标签、价格的自动推导，不在列表里手改标签。
- 不改技能页、工具页、用量页。
- 不把密钥写进计划、日志或列表接口。

## Approach

先让模型弹窗的保存真正落盘，再把 Key 从一段文本改成带备注的多行。Agents 和引擎只改展示和入口，不改它们的数据形状。新文案走 i18n，控件用 Arco，列表继续用 `SettingsCardList` / `SettingsCardRow`。

## Work Slices

### Slice 1：供应商名字和地址能保存

- Intent：编辑弹窗里改的名字和 API 地址，刷新后还在。
- Files：`ui/packages/desktop/src/renderer/components/settings/SettingsModal/contents/ModelModalContent.tsx`。必要时核对 `ipcBridge.mode.updateProvider`。
- Steps：`onChange` 把表单交给 `updateProvider`。请求里的 `api_key` 若仍是掩码就不要传，避免把点写回 `.data/providers.jsonc`。保存成功后再 `refresh()`。
- Verify：把一个供应商改名，刷新页面，列表标题是新名字。密钥行数不变，对话仍能用原来的 Key。
- Checkpoint：单独提交。

### Slice 2：每个 API Key 单独一行，可备注、可查看

- Intent：一个供应商可以有多把 Key。每把有备注，默认隐藏，眼睛只揭开这一把。
- Files：`src/crewchat/core/providers.py`、`src/crewchat/api/providers_api.py`、`EditModeModal.tsx`、`AddPlatformModal.tsx`。网关只改「用哪一把」这一处：`src/crewchat/core/gateway_ccswitch.py` 里今天直接拿整段 `api_key` 的地方。
- Steps：真相源增加 Key 列表，每项是 id、备注、密钥、是否默认。读旧数据时按换行和逗号拆开，第一把为默认，备注为空。列表接口仍只给掩码、备注和个数；明文只走现有的 secret 接口，并带上是哪一把。弹窗里一行一个：备注、`Input.Password`（Arco 的眼睛）、删除。底部「添加一把」。去掉「每行一个 / 自动轮询」的文案。网关和 `llm` 只用默认那把，不把多把拼成一个字符串。
- Verify：给同一供应商加两把，备注分别是两个账号名。刷新后备注还在，默认是隐藏的点。点眼睛只显示被点的那把。用默认 Key 发一条最短对话，回复仍是模型文本。
- Checkpoint：后端形状和弹窗分开提交。

### Slice 3：模型列表别一次铺开

- Intent：打开页面先看到供应商，而不是几十个模型。
- Files：`ModelModalContent.tsx`。筛选胶囊 `FilterChip` 现为裸 `<button>`，这次顺手改成 Arco `Button` 或已有胶囊，不另开任务。
- Steps：供应商默认折叠，标题上保留「已启用/总数 · Key 数」。搜索或选标签时自动展开有命中的组。
- Verify：刷新后各组是收起的。搜索一个模型 id，只有含它的组展开，并且能点到该行。

### Slice 4：Agents 列表和表单对得上人话

- Intent：左列能认出谁，新建和未保存切换不再用浏览器自带对话框。
- Files：`AgentComposerContent.tsx`，以及它取模型列表的 `AgentComposer/api.ts`（若下拉目前只返回 id）。
- Steps：左列副标题用供应商名称和模型显示名，过长时整段放在 Tooltip，不露出无意义的端点 id。标识三个框各自有标签。新建改成 Arco 弹窗，收集 id 和名称。未保存时切换或新建，用 Arco `Modal.confirm`。模型下拉优先显示 `display_name`，值仍是 id。删掉与上面三栏重复的只读规则预览。
- Verify：选中一个大脑不是 Agnes 的 Agent，左列能看出供应商名和模型名。新建取消后列表不变。改名称不保存就点另一个，确认框出现，取消后草稿还在。

### Slice 5：引擎行只显示状态，路径进详情

- Intent：列表表达装没装、版本、被谁用。本机路径不要铺在每一行上。
- Files：`EngineSettings/LocalAgents.tsx`。修复页已在 `AgentRepairPage.tsx`，这里只接入口。
- Steps：副标题保留来源和版本。路径放到 Tooltip，或作为行点击后进入修复页的只读信息。行可点击进入 `/settings/engines/:id/repair`。图标仍全部是终端符号时，先确认 `/api/engines/management` 有没有图标；没有就保持终端符号，不在前端硬编码 logo 路径。
- Verify：已安装列表上看不到 `/Users/...` 整段路径。点一行能打开修复页并回到列表。有更新的行仍能点「更新」。

## Risks

- Slice 1 若把掩码一并提交，会把真 Key 盖成点。保存前必须丢掉掩码。
- Slice 2 把旧的一整段字符串拆开时，Key 中间若本身含逗号会被切开。拆分只按换行；逗号仅在整段没有换行时才当作分隔，并在计划里按这个规则迁移。
- 默认 Key 若选错，已有 Agent 的对话会换一把钥匙。迁移时第一把保持今天字符串里的第一段。

## Done Criteria

- 供应商改名并刷新后，列表标题是新名字。
- 两把 Key 各有备注；眼睛只显示被点的那把；列表接口响应里没有明文。
- 默认 Key 的一条最短对话有模型回复。
- Agents 左列不出现截断的端点 id；新建不用 `window.prompt`。
- 引擎列表不展示完整本机路径；行能进入修复页。
- `uv run pytest -q` 通过。三个页面在浏览器里按上面的 Verify 各走一遍。
