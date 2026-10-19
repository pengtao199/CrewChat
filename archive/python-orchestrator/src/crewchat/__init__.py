"""CrewChat —— v1 的 Python 编排层，**冻结为参考**。

产品运行时不是这里：

- 界面 / 会话 / 委派 / 权限 / 持久化 → `codeg/`（Codeg 二次开发，含它自己的 Rust 后端）
- 模型统一管理 → `ccr/`（CCR）
- 本包 → 只保留可读的历史实现，**不参与运行时**，也不接任何新功能

为什么保留而不是删除：它是产品语义的原始出处（Agent = 大脑 + 身体 + 能力 + 人格、
管理员先计划再委派、串行执行防同工作区互写），读它与跑它的旧测试仍有用；
`scripts/check_layers.py` 也继续约束它的分层契约。

要改产品行为，改 Codeg 侧；不要把这里的代码接回运行时。
见 `docs/plans/agents-core-collaboration.md` 拍板 #6 与 `AGENTS/architecture.md`。
"""
