# AgentOS 壳与核心的边界

## 适用范围

涉及 `AgentOS/`（macOS Swift 壳）、「AgentOS 怎么跟核心说话」、「这件事该不该在壳里做」、或判断某个功能该不该进壳时。

## 定位

AgentOS 是 CrewChat 的 macOS 原生客户端，只做壳：

- **核心是 Rust**（`codeg/src-tauri`，以 `codeg-server` 二进制作为本机 sidecar 运行）：拥有 ACP 会话、委派、权限、持久化、用量与模型接入。
- **AgentOS 是 Swift**：SwiftUI 渲染与交互，把用户意图翻译成对核心的调用。

判定标准一句话：**壳里出现「协议解析」或「状态判定」就是走错层。**
名词提醒：本仓库里「引擎」指 CLI 身体（claude / codex / …），不是 Rust 核心；Rust 侧统一叫**核心**。

## 核心的事实

| 项 | 值 |
| --- | --- |
| 源码 | `codeg/src-tauri/`（Apache-2.0 二次开发基座） |
| 产物 | `codeg-server`（Rust 二进制），AgentOS 把它当 sidecar 拉起 |
| 构建 | `cd codeg/src-tauri && cargo build --release --bin codeg-server --no-default-features --features server-bin` |
| 启动参数 | 环境变量 `CODEG_HOST` / `CODEG_PORT` / `CODEG_TOKEN` / `CODEG_DATA_DIR` / `CODEG_HOME` / `CODEG_STATIC_DIR` |
| 监听 | 默认 `127.0.0.1:3080`（**核心自身默认 `0.0.0.0`，壳必须显式收紧到 `127.0.0.1`**） |
| 鉴权 | HTTP `Authorization: Bearer <token>`；WS `Sec-WebSocket-Protocol: codeg-events, codeg-token.<base64url(token)>` |
| 接口 | `POST /api/<command>`（命令名同 Tauri command，snake_case）；事件走 `GET /ws/events` |
| 错误 | `{ code(snake_case), message, detail?, i18n_key?, i18n_params? }` |

- **token 由壳生成**：启动核心时传 `CODEG_TOKEN`，壳把同一值存 Keychain。核心只在没收到时才自己生成并落库——壳永不读核心数据库。
- 不传 `CODEG_STATIC_DIR` 也可运行（核心只警告），所以 AgentOS 不需要 Next.js 前端产物。
- 参考实现可读不可依赖：`codeg/src/lib/transport/`（TS）。

```text
codeg-server
  CODEG_HOST=127.0.0.1
  CODEG_PORT=<壳选的空闲端口>
  CODEG_TOKEN=<壳生成，存 Keychain>
  CODEG_DATA_DIR=~/Library/Application Support/<App>/core
```

## 壳允许做的

- 渲染核心返回的数据；把用户操作翻译成核心命令。
- 维护纯展示态：选中项、输入框草稿、面板开合、滚动位置。
- 维护 AgentOS 自身设置：窗口状态、核心地址与 token、上次打开的会话 id（UserDefaults / Keychain）。
- 对核心事件按 `seq` 去重、按 `subscription_id` 分派、按命令结果合并（见 `agentos-transport.md`）。

## 壳禁止做的

- 解析 ACP 或任何 CLI 的私有协议；自己 spawn `claude-agent-acp` 之类的 CLI 进程。
- 直接读写 `codeg.db`、`.data/`、`~/.claude`、`~/.codex` 等核心或 CLI 的地盘。
- 判定「某 CLI 是否可用 / 某模型是否可达 / 某技能是否已安装」——这些是核心的结论，壳只显示。
- 实现编排、路由、重试循环、权限裁决。
- 把核心当远程服务暴露；**仅 127.0.0.1，禁止监听 LAN / 公网**。

## 功能范围

**做（v1）**：

- 主界面：以文件夹为工作区；工作区内可折叠的会话侧栏；主对话区；可折叠 inspector（次要信息）。
- 会话：列表、进入、发送、流式渲染、取消、权限审批。
- 协作可见性：成员 / 谁在说 / 委派 / 工具调用 / 用量。
- 设置：**通用 / 核心 / 模型 / Agents / 能力 / 关于**——展示与编辑在壳，存储与校验在核心。

**不做**：内嵌浏览器、computer-use、内嵌终端、forge / canvas / pets / office / science / automation、多标签工作台、Monaco 与 diff 审阅、壳本地业务数据库（会话 / 消息 / 工作区不落壳）、多语言界面（先只 `zh-Hans`）、远端与局域网访问。

## 设置项与核心命令

- 每个设置项的读取与写入都必须落到一个**已登记的核心命令**上（见 `AgentOS/engine-api.md`）。
- 核心没有对应命令 → 先在核心侧加（走 `AGENTS/upstream.md` 的分档），**不在壳里自存一份**。
- 界面显示核心返回的真值，不显示壳「想象的默认值」。唯一例外是一次性初始化：首次运行把某个核心设置置为产品默认（如委派默认开启），必须落一个「已初始化」标记，之后一律以核心为准。
- 壳只做表单与就近反馈；校验规则、默认值、副作用都在核心。

## 本地状态白名单

| 位置 | 只允许放 |
| --- | --- |
| UserDefaults | 外观、启动时自动拉起、侧栏折叠状态、inspector 开合、上次选中的工作区 / 会话 id |
| Keychain | 本机核心的访问 token |

禁止在壳里持久化：会话、消息、工作区、Agent、模型、MCP、技能等业务数据。

## 判定流程（新增能力时按序问）

1. 核心有对应命令或事件吗？有 → 只做渲染，登记 `AgentOS/engine-api.md`。
2. 核心没有但属于「执行 / 真相」→ **不自己造**：先在核心侧加命令（走 `AGENTS/upstream.md` 的改动分档），再回壳接线。
3. 核心没有且属于「纯展示」→ 壳里做。

## 失败与降级

- 核心未启动 / 端口不可达：显示可操作错误（含地址与「如何启动」），不静默、不假装可用。
- `401`：token 失效 → 引导重新获取，不清空用户数据。
- 版本不匹配：壳与核心同仓发布、同版本号；契约有破坏性变更时**同一次改动**内更新壳与 `AgentOS/engine-api.md`。

## 检查

- 这次改动把哪条「真相」搬进了壳？应该是零。
- 新增的核心调用登记进 `AgentOS/engine-api.md` 了吗？
- 核心挂掉时界面给的是可操作错误，还是空白 / 静默？
- 这个功能在「做」清单里吗？不在就先立计划，不要顺手实现。
