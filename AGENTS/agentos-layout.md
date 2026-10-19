# AgentOS 结构与分层

## 适用范围

新增 `AgentOS/` 下的目录或模块、调整分层、判断代码归属、引入依赖、给壳里的类型命名时。

## 目标

AgentOS 能独立构建、独立测试、独立演进；核心只通过一层薄适配被触达。边界由编译器守，不由自觉守。

## 分层契约

目录即契约，依赖只能向内，违反即错。

| 层 | 目录 | 职责 | 禁止 |
| --- | --- | --- | --- |
| 组装 | `App/` | 场景、依赖装配、生命周期 | 业务规则、界面细节 |
| 表现 | `Features/<Feature>/` | 屏幕、状态、动作（视图 + 视图模型 + 功能内小组件） | 发请求、解码 JSON、import `Core` |
| 领域 | `Domain/` | 壳自己的类型与状态机；**端口协议** | SwiftUI、URLSession、Keychain、文件路径 |
| 适配 | `Core/` | 端口的实现：命令客户端、事件流、sidecar 进程、wire DTO、错误 | 界面逻辑、产品判定 |
| 基础 | `Infrastructure/` | Keychain、UserDefaults、日志 | 领域概念、界面 |

```text
App ──▶ Features ──▶ Domain ◀── Core ──▶ Infrastructure
                       ▲
                       └─ 端口定义在 Domain，实现在 Core
```

## 边界规则

- **端口在 `Domain/`，实现在 `Core/`**：`Features` 只认 `Domain` 的协议；`App` 把 `Core` 的实现注入。换核心 = 换 `Core/`，`Features` 不动。
- **wire DTO 不出 `Core/`**：核心的 JSON 形状只活在 `Core/Wire/`（`Core` 前缀）；`Core` 把它映射成 `Domain` 类型后再向上给。`Features` 看不到也拿不到核心的字段形状。
- **端口按能力分组，起步最多三个**：命令、事件流、进程生命周期。不要为每个接口开一个端口。
- **`Features` 不得 `import Core`**：硬约束（可加 lint），保证核心的 500+ 条命令不会渗进界面。
- 一个类型只有一个归属层；跨层直接调用（视图 → 适配）一律禁止。

## 依赖白名单

默认零第三方依赖。引入前按 `AGENTS/dependencies.md` 调研，并满足：

- 与分发方式兼容（非沙盒、可执行外部进程、Developer ID 签名）；
- 有明确收益：JSON 用 `Codable`，网络用 `URLSession`，不引同类库；
- 结论写进 `docs/plans/` 对应计划一句「选谁、为什么、备选是谁」。

## 开发习惯

- 小切片推进，不做大重写；改前 `git status --short`，不覆盖别人的改动。
- 原生优先：先用 SwiftUI / Foundation / URLSession / Keychain，再加依赖。
- **代码里不写解释性长注释与同义注释**；命名与类型负责表达意图。
- 密钥不落 AgentOS：模型密钥归核心（CCR），壳只持有本机核心的 token。
- 每个多 Agent 运行都必须可取消、可观察、有上限；上限由核心执行，壳只负责显示与触发。

## 检查

- 改动落在正确的层了吗？`Features` 里有没有出现核心字段名？
- 这个新依赖不能由现有能力替代吗？
- 新类型属于哪一层？前缀对吗（见 `AGENTS/naming.md`）？
