# CLI 输出噪音：模型元数据警告混进正文

Updated: 2026-10-10 (Asia/Shanghai)
Status: Analyzed。根因已定位到症状层，未修。推荐方案见 Approach。

只处理「CLI 自己打的诊断信息被当成模型正文输出」这一类问题，不改思考流、不改正文流式契约（见 `AGENTS/api-contract.md` 的「累积 + replace」）。权限系统是另一件事，不在本计划内。

## Goal

`opencode` 身体每次回答的开头都会被一行英文警告污染。这行字会流进 `delta`、落进 `.data/crewchat.db` 的 assistant 正文，用户在界面上看到的是自己的回答前面挂着一句与自己无关的告警。目标是这类诊断信息不再出现在正文里，同时一条真实输出都不丢。

## Symptoms

界面上的气泡长这样：

```
⚙️ 后端工程师
Warning: Model metadata for `ccs-opencode-go/deepseek-flash` not found. Defaulting to fallback metadata; this can degrade performance and cause issues.

收到。用户要的是「对话页里其他 Agent 的状态怎么展示……
```

对应数据库（`.data/crewchat.db`，只读排查，未改动）：

```
sqlite> SELECT author, substr(content,1,120) FROM messages WHERE content LIKE '%metadata%';
🧭 管理员 | Warning: Model metadata for `ccs-opencode-go/deepseek-flash` not found. Defaulting to fallback metadata…
🧭 管理员 | Warning: Model metadata for `ccs-opencode-go/deepseek-flash` not found. …
🧭 管理员 | Warning: Model metadata for `ccs-opencode-go/step-5-preview-free` not found. …
```

`deepseek-flash` 和 `step-5-preview-free` 都中过，即**每次调用必现**，不是偶发。

## Current Facts

2026-10-10 实测与读码结论。

- **不是 UI 层问题。** 这行字已经进了数据库正文（见上），所以是后端把它当成了模型输出，与 `MessageText` / `hooks.ts` 的渲染无关。
- **不是 stderr。** `AcpBody.run` 用 `_discard_stderr`（`executors/acp_body.py`）把子进程 stderr 读掉丢弃，警告不会从那里进来。
- **走的是 ACP 正文帧。** 它随 `delta` 进 `buffer`，被 `chat.py` 的 `persist_turn` 落库。`chat.py` 里 `thinking` 已有单独分支，正文分支不做任何清洗。
- **实跑复现（部分）。** 用同形状的内联配置驱动 `opencode acp`（脚本见 Verification），会话建立成功、没有出现该警告；但 `session/prompt` 被网关拒绝：
  `聚合的模型 ccs-opencode-go/deepseek-flash 在 CC Switch 里不存在`。
  即那次根本没走到产生警告的那一步。**警告的确切注入点（哪个 ACP 帧、哪个 opencode 内部函数）仍未确认**，这是本计划最大的未验证项。
- **配置里本来就在规避它，但没挡住。** `acp_body.py` 的 `_inline_config` 注释写着「发布 id 不在它的模型目录里，所以同时写上 limit，避免 "metadata not found" 警告」，并在 `model_entry` 里写了 `limit: {context, output}`。实测该写法不足以消除警告。
- **触发条件是「模型 id 不在 CLI 自己的模型目录里」。** 我们发给 opencode 的 `modelID` 是网关发布 id `ccs-opencode-go/deepseek-flash`（`_opencode_wire_model` 返回目录键 `main` + 发布 id），带 `OPENCODE_DISABLE_MODELS_FETCH=1`、`OPENCODE_TEST_HOME` 隔离 home，CLI 本地没有任何该模型的元数据，于是走 fallback。
- **不在二进制明文里。** `strings` / `grep -a` 在 `opencode` 2.0.20 的 176MB 二进制和 `ccsw-proxy` 里都找不到该英文原句，说明 opencode 的 JS 段是压缩存储的（`grep -a "Defaulting to fallback"` 计数为 0）。所以无法用静态搜索定位，**必须实跑复现**。
- 落库路径与本项目其它正文一致（`.data/crewchat.db` → `messages.content`），所以任何清洗都要发生在 `persist_turn` 之前。

## Non-Goals

- 不改思考流（thinking）的累积 + replace 契约。那是另一条链路，已修好（见本计划末尾「已完成的前置项」）。
- 不删数据库里已经存进去的历史脏数据。要不要清由人决定。
- 不为了消警告而给 opencode 放开全局模型目录或联网拉取元数据（`OPENCODE_DISABLE_MODELS_FETCH=1` 是隔离要求，见 `AGENTS/bodies.md`）。
- 不动网关侧的模型登记逻辑。

## Approach

推荐 **B 为主 + C 兜底**，A 留作后续根治。三者互不冲突，可分批落地。

**A. 消除根因（最稳，但易随 CLI 升级碎掉）**
让 OpenCode 认得这个模型：在 `model_entry` 里补齐它 2.0.20 实际读取的元数据字段（除 `limit` 外的 `cost` / `release_date` / `options` 等，以实测为准），或改 `modelID` 成 CLI 能识别的形式。
代价：要摸清 schema，且随 CLI 版本变动。**不建议单独依赖它。**

**B. 噪声隔离（稳、通用）**
在 ACP 边界 `acp_body.py` 的 `_AcpClient.session_update` 里识别诊断前缀（当前已知形态：`Warning: Model metadata for … not found.`），把它路由成 `notice` 事件而**不是** `delta`。
`ExecRequest.emit` 本来就有 `notice` 这一类（见 `AGENTS/bodies.md` 契约），语义上正合适。
好处：以后任何 CLI 噪声都自动挡住，不需要为每个 CLI 单独适配。
代价：模式匹配有误伤风险，需要一份「已知诊断行」清单，且必须能容忍正文里真的出现同样字样的开头。

**C. 后端清洗（兜底，防漏网）**
`chat.py` 已有 `strip_control_calls` 用来在落库前剥控制调用，在同一层加一条规则剥掉已知诊断行。零误伤到正常正文，但治标：界面仍会短暂显示被剥掉的那段（取决于清洗发生在流式广播前还是落库前——按现状应放在广播前才不留残影）。

**实施顺序建议**：先按 B 在身体边界丢弃并改走 `notice`，同时把这份「已知诊断行」清单写进本计划；再在 C 加一条断言作为回归保护。

## Work Slices

（待方案定稿后按 B / C 拆。改动只在 Python 侧：`executors/acp_body.py` + `api/chat.py`，配 `tests/`，不碰前端。）

## Open Questions

1. 警告的确切来源帧仍未确认（见 Current Facts 第 4 条）。落地 B 之前需要一次成功跑通网关的实跑，把 `session_update` 全量打出来，确认它是 `AgentMessageChunk` 还是别的 update 类型。
2. `notice` 事件目前有没有前端消费？如果没有，是先只丢弃不打日志，还是补一个前端展示位（按 `AGENTS/ui.md`，新增用户可见文案要走 i18n key）。

## Verification

（本次只做分析，实测证据如下。）

- 复现脚本：`/var/folders/ll/hrgzfh413h14vww9wc1zt_5m0000gn/T/opencode/probe_acp.py`（临时目录，不入库）。它以与生产同形状的配置拉起 `opencode acp`，打印每个 `session_update` 的类型与文本。
- 实跑输出：
  ```
  [UPDATE] AvailableCommandsUpdate: None
  [SESSION] ses_ed996bc1effetGttrdDPso2Klh
  [PROMPT ERR] RequestError: Internal error: 聚合的模型 ccs-opencode-go/deepseek-flash 在 CC Switch 里不存在…
  ```
  会话建立正常、无警告；提示被网关拦下。**因此这条路径尚未复现出警告本身**，脚本要留着改用已登记模型再跑一次。
- 静态搜索结论：`opencode` 二进制与 `ccsw-proxy` 均无该英文原句（压缩存储，见 Current Facts）。
- 数据库只读查询确认污染已入库。

## 已完成的前置项

同一次排查里顺带修掉并已验证的是另一件事，记录在此避免混淆：

- **思考流重复**：后端思考帧是「累积 + replace」，前端两处 reducer 对 thinking 无条件字符串相加，导致整段推理反复叠加。已改为按 `replace` 覆盖（`ui/.../Messages/hooks.ts`、`ui/.../chat/chatLib.ts`），后端补了累积契约测试，`AGENTS/api-contract.md` 标注思考流同属「累积 + replace」。与本计划无关，但同属「流式帧语义」这条线。