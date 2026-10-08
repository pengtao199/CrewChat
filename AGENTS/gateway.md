# 模型网关

## 适用范围

改动模型接入、上游端点、LiteLLM 配置，或遇到"某 CLI 不支持某端点"类问题时。

## 目标

让任意 OpenAI 兼容端点驱动任意身体；协议差异由网关吸收，不进入业务代码。

## 为什么需要网关

| 客户端 | 需要的线格式 | | 上游常见能力 |
| --- | --- | --- | --- |
| codex | `/v1/responses` | | 多数第三方端点只有 `/v1/chat/completions` |
| claude | `/v1/messages` | | 同上 |
| opencode / llm | `/v1/chat/completions` | | 一致 |

LiteLLM 一个进程同时暴露三种格式，因此所有身体都能接任意上游。

## 原则

- **网关是基础设施，不是依赖**：以独立工具安装（`uv tool install litellm[proxy]`），通过 HTTP 通信。不得把它加进项目依赖（其 `mcp>=2.2` 与 Chainlit 等冲突的历史原因同样适用于未来依赖）。
- **优先使用网关的原生开关，不修改其源码**：例如强行走 chat 转换用模型别名前缀 `openai/chat_completions/<model>`，而不是给 Python 打补丁。
- **协议差异集中在网关配置**：业务代码只认 OpenAI 兼容语义。
- **端点能力差异要显式表达**：不支持 Responses 的上游就在配置里走 chat 转换，不靠试错。

## 具体规则

- 新增推理端点时，改 `gateway/litellm.yaml` 的 `model_list`，并让该条目的 `model` 使用 `openai/chat_completions/<model>` 前缀（除非确认上游原生支持 Responses）。
- 环境变量传密钥（`AGNES_API_KEY` 等），配置文件里只写 `os.environ/...` 引用，不写明文。
- 网关只监听 `127.0.0.1`。对外开放必须设置 master key，不得依赖"本机可信"。
- 身体的模型指定通过大脑池（`Provider.model`）传递；`codex` 的 provider 名放 `settings.codex_provider`，不硬编码在身体里。
- 排查"某个身体接不上"时，先确认该身体需要的线格式，再确认网关是否暴露了它，最后才怀疑端点。

## 禁止项

- 不修改 LiteLLM 已安装的源码作为长期方案；确需临时验证时，验证后必须还原并在本文件记录结论。
- 不在身体里手写 Responses ↔ Chat 的转换（那是网关的职责）。
- 不绕过网关直连端点去"验证一下能不能用"然后写进代码。
- 不把网关暴露到非本机地址而不设鉴权。

## 检查

- 这个上游支持哪种线格式？网关有没有对应暴露？
- 换一个新的 OpenAI 兼容端点，需要改几处？理想是只加大脑池一条。
- 这次改动是否动了 LiteLLM 的安装文件？如果是，为什么不能用配置解决？
