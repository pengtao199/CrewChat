# 所有 CLI 经 cc-switch 反代：调研报告与实施计划

> 状态：**调研记录**。实施以 [cli-model-binding.md](cli-model-binding.md) 为准（按进程配置，不改用户全局，不换网关）。本文保留官方分档与 opencode 实测，不再当作待执行计划。
> 上游依据：cc-switch 官方 v4.0.x（仓库 `farion1231/cc-switch`，docs v3.20.4 / 4.0 聚合指南）、opencode 官方 v2 文档、ACP 注册表。

## 1. 目标

所有 CLI 身体都经 cc-switch 反代，**不各自直连 provider**。网关是唯一持有 provider 与 key 的地方，负责协议归一、逐请求路由、故障转移、统一记账。身体只是壳——否则网关白建。

## 2. 官方能力矩阵（本次调研的核心结论）

cc-switch 对每个 app 的支持**不是一刀切**，官方明确分三档：

| 模式 | 请求怎么走 | 官方支持的 app |
| --- | --- | --- |
| **直连** | 客户端直连供应商，CC Switch 只改配置 | 全部 10 个 |
| **路由（route）** | 请求先经本地路由（15721），可做**协议转换 + 故障转移 + 用量记录** | **Claude Code、Codex、Gemini CLI、Grok Build** |
| **聚合（Stack）** | 请求先经本地路由，按客户端里选的模型**逐请求**分给多家供应商（不做故障转移） | **仅 Claude Code、Codex** |

其余 app 官方定位是**「共存」**而非「接管」：

> —— cc-switch README：`OpenCode \| 共存 \| …`、`OpenClaw \| 共存 \| …`。共存的含义是"多个供应商同时写入工具自身的配置，在工具里选择使用"。
>
> —— 官方 4.1/4.2：`支持本地路由的应用：Claude Code、Codex、Gemini CLI、Grok Build`。
>
> —— 官方聚合指南：`目前，聚合模式支持 Claude Code 和 Codex`。

**这与我们仓库里的现状完全吻合**（`PROXY_APPS = [Claude, Codex, Gemini, GrokBuild]`、`supports_stack = claude|codex`），说明上游设计如此，不是我们的疏漏。

### 官方路由改写的配置（照抄自官方 4.2）

| App | 改写后的端点 |
| --- | --- |
| Claude Code | `env.ANTHROPIC_BASE_URL = http://127.0.0.1:15721` |
| Codex | `[model_providers.custom].base_url = http://127.0.0.1:15721/v1` |
| Gemini CLI | `GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:15721` |
| Grok Build | `~/.grok/config.toml` 模型表地址改为 `http://127.0.0.1:15721/grokbuild/v1` |

要点：**真实 key 留在 CC Switch 里**，客户端配置只写代理地址；退出 CC Switch 时自动写回直连，下次启动再接上。

## 3. 实测：为什么 opencode 之前走不通

用本地回显 server 抓到 opencode 实际发出的请求，三个事实：

```
PATH: /opencode/v1/chat/completions     ← 我们注入的路径生效了
HEADERS: authorization: Bearer sk-Dz2x… ← opencode 用了用户 auth.json 里的明文 key
BODY: {"model": "agnes-2.5-flash", …}   ← 只发 <model> 段，丢掉 provider 前缀
```

三个后果：

1. **模型丢失 provider 段** → "id 自带 app 段"的方案在 opencode 上失效（claude/codex 发整串才行）。
2. **key 泄漏**：我们 spawn 的子进程拿着用户明文 key（比"没走反代"更严重）。
3. **模型选择不受我们控制**：注入的 provider 没出现在 ACP 的模型选项里，opencode 用它自己的默认（openrouter）。

关键认知：对 opencode 而言，客户端自己怎么选模型/拿凭据是**它自己的领域**。对抗它的模型发现机制是打错仗。

## 4. 解决方案：opencode 有两个官方杠杆，正好对症

opencode 官方配置（`opencode.json`）提供两个能力，逐一解决上面两个问题：

### 4.1 `modelID` 别名 —— 解决"发什么模型"

```jsonc
{
  "model": "gw/main",
  "providers": {
    "gw": {
      "models": {
        "main": { "modelID": "ccs-opencode-agnes-ai/agnes-2.5-flash" }
      }
    }
  }
}
```

> 官方："Use **`modelID`** to give an API model a friendlier catalog ID … Here `opencode/coding-default` is the selectable reference and **`gpt-5.2` is sent to the provider**."

即：catalog id（`gw/main`）与**实际发给上游的 model 字段**可以不同。我们把发给上游的值设成网关发布 id，反代据此逐请求路由——**不需要 opencode 的模型选择器配合**。

### 4.2 顶层 `model` 默认 —— 解决"用哪个模型"

> 官方："Set **`model`** in opencode.json … The configured model becomes the catalog default when it is enabled and its provider is available."

新会话不依赖 `set_config_option`（那个选项列的是 models.dev 全域模型，我们的 provider 不在其中），直接由配置默认值兜底。

### 4.3 唯一 provider 名 + 占位凭据 —— 解决 key 泄漏

用**不与任何已有 provider 重名**的 id（如 `gw`），opencode 没有它的存档凭据，只能用配置里的 `apiKey`。我们把 `apiKey` 写成反代认的占位符（`PROXY_MANAGED`，转发时 cc-switch 换成真实 key），明文 key 就不进子进程。

### 4.4 app 身份：两条路都通

- **模型 id 自带 app 段**（`ccs-opencode-…`）：反代从 model 推出 app。已实现（`app_of_stack_id`）。
- **URL 路径段**（`/opencode/v1/chat/completions`）：反代从路径推出 app。已实现（`/:app/v1/chat/completions` + 从 uri 取首段），实测 opencode 确实会走这条路径。

对 opencode 用 **模型 id**（因为 `modelID` 让我们能完全控制发给上游的值）；路径段作为兜底与 klarheit。

## 5. 分档实施计划

### Tier 1 — 全量反代 + 逐请求路由（已通，保稳）

`claude`、`codex`

- 现状：Stack 模式，`activate` 返回 `ccs-claude-…` / `ccs-codex-…`，端点与模型由我们注入。
- 动作：**回归验证**即可，不扩语义。
- 已知：claude 经 ACP 桥时 `ANTHROPIC_MODEL` 注入发布 id；codex 读 cc-switch 写的 config.toml。

### Tier 2 — 路由模式反代（官方支持，需实现）

`gemini`、`grok`

官方明确支持路由模式，语义比 Stack 弱一档：**单个当前供应商 + 协议转换 + 故障转移 + 用量记录**，不做逐请求多供应商路由。

- gemini：把 `GOOGLE_GEMINI_BASE_URL` 指向反代（cc-switch 已有 gemini 投影，`write_proxy` 已有分支）。
- grok：`~/.grok/config.toml` 模型表地址指向 `http://127.0.0.1:15721/grokbuild/v1`（cc-switch 已有 grokbuild 分支）。
- 身体侧：把该 CLI 的端点指向反代、模型用它自己的模型名（路由目标由反代的当前路由决定）。
- 价值：协议转换（grok/gemini 的上游格式 ≠ 我们 app 发的格式）、故障转移、统一记账。
- 注意：官方账号（Claude Official / Codex OpenAI Official 除外）不能经路由。

### Tier 3 — 反代但按共存语义（官方不支持接管，靠注入配置实现）

`opencode`、`openclaw`、`hermes`、`pi`、`mcode`

官方定位是共存：**没有**本地路由接管、**没有**聚合。但我们**仍然可以让它们经反代**，因为它们是 openai 兼容的、接受自定义 provider：

```
inline opencode.json:
  model:        "gw/main"                          ← 默认模型（§4.2）
  provider.gw:  { npm: "@ai-sdk/openai-compatible",
                  options: { baseURL: <反代>, apiKey: PROXY_MANAGED },
                  models: { main: { modelID: <ccs-opencode-…> } } }   ← 发给上游的值（§4.1）
```

- app 身份：模型 id 的 app 段（或路径段兜底）。
- 逐请求路由：**单供应商语义**（由 `activate` 时加入 Stack 的那家决定），因为 opencode 只发一个固定 modelID。多供应商在同一 CLI 内切换 = 重新生成 inline 配置。
- 凭据：provider 名唯一 → 用占位符，**明文 key 不落子进程**（修掉 §3.2 的泄漏）。

### Tier 0 — 需要先修的两处（当前代码与官方语义冲突）

1. **回退 `supports_local_proxy`**：我们曾把它改成"除 ClaudeDesktop 全支持"，这与官方 `PROXY_APPS`（Claude/Codex/Gemini/GrokBuild）冲突，会让 cc-switch 去接管它接不住的应用。应恢复成官方四类，Tier 3 的接入靠**身体侧注入**而非 cc-switch 侧接管。
2. **`stack_route` 的返回**：Tier 3 的 app 走 `/stack/route` 时，cc-switch 会因非 `PROXY_APPS` 而拒绝。需要决定：要么把 Tier 3 的接入从"cc-switch 接管"改为"只登记 provider 行 + 由身体侧拼 `ccs-<app>-<key>/<model>`"，要么显式放行并按只记契约处理（后者接近我们已加的 `write_proxy` 通用分支）。

## 6. 统一记账

不开路由的应用也能记账——官方 4.4：`不开路由时，CC Switch 也会从各工具的本地会话记录统计用量`。因此 Tier 3 即便只做到"经反代转发"，用量仍可回溯；而经反代的请求会带 `usage` 明细（§4.4 路由记录），质量更高。

## 7. 验收标准

对一个 Tier 3 app（以 opencode 为样板）：

1. 子进程环境**不含**任何真实 provider key（`grep -c sk-` 为 0）。
2. 反代日志能看到该 CLI 的请求，且 `model` 为 `ccs-<app>-…`。
3. app 端到端回复正确（模型确实是该 agent 的大脑，不是 opencode 默认的 openrouter）。
4. 同一会话内 `activate` 换一家 provider，下一个 agent 的请求路由到新家（逐请求/或重生成 inline 配置）。
5. Tier 2 的 gemini/grok：反代完成协议转换，返回结构正确。
6. `uv run pytest -q` 通过；cc-switch release 构建无新增 warning。

## 8. 风险与开放问题

| # | 问题 | 影响 | 处置 |
| --- | --- | --- | --- |
| 1 | `OPENCODE_CONFIG_CONTENT` 与全局配置是**合并**还是**替换**未确证（实测全局 `agnes` 仍出现，倾向合并） | Tier 3 能否完全隔离用户配置 | 实施时验证；用唯一 provider 名规避冲突 |
| 2 | 配置默认 `model` 与"会话已选模型"的优先级（官方：会话已选 > 配置默认） | 复用旧 session 时可能不生效 | 新建 session 每轮重生成配置；必要时显式 `set_config_option` |
| 3 | `modelID` 的值为带 `/` 的长串是否被 opencode 正常透传 | §4.1 成立与否 | 首个 spike 用回显 server 验证（已有 harness） |
| 4 | Tier 3 单 CLI 内多供应商并发切换 | 同 CLI 多 agent 不同大脑时 | 每 agent 独立 inline 配置（进程级隔离，本就是这么设计的） |
| 5 | Tier 2 的官方账号限制 | gemini/grok 官方订阅不经路由 | 文档提示，产品上要求第三方供应商 |
| 6 | 归档的 bespoke 适配器仍在仓库 | 认知负担 | 已移入 `executors/archive/`，待 Tier 3 稳定后删除 |

## 9. 建议的实施顺序

1. **Tier 0**：回退 `supports_local_proxy` 到官方语义；确定 Tier 3 的登记方式（不接管、只注入）。
2. **Tier 3 spike（opencode）**：用 §4 的配置形状 + 回显 server 验证 `modelID` 透传与占位凭据；随后 app 级 E2E。
   - 这是全部关键假设的验证点，且 opencode 是我们唯一装了 ACP 的非 claude/codex CLI。
3. **Tier 3 铺开**：openclaw / hermes / pi / mcode 按同一形状（各自的 inline 配置生成器 + 反代端点 + `ccs-<app>-…` id）。
4. **Tier 2**：gemini、grok 走官方路由模式，补齐协议转换的 E2E。
5. **Tier 1 回归** + 删除归档适配器 + 更新 `AGENTS/bodies.md` 与本文状态。

## 10. 结论

- **不是能力缺失，是分档设计**：官方只对 4 个 app 提供路由接管、对 2 个 app 提供聚合。我们此前把"其它 app 也能接管"当成目标，方向就偏了。
- **其它 5 个 app 仍可经反代**，正确姿势是**身体侧注入 openai 兼容 provider**（唯一 provider 名 + `modelID` 别名 + 顶层 `model` 默认 + 占位凭据），而不是改 cc-switch 去接管它们。
- 顺带修掉一个真问题：**明文 key 泄漏到 opencode 子进程**。
- opencode 的两个官方配置杠杆（`modelID` / `model`）使 Tier 3 不需要与它的模型选择器对抗。
