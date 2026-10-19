# Git

## 适用范围

执行 Git 初始化、暂存、提交、推送、分支操作，或修改 `.gitignore` 时。

## 目标

提交历史可读、可按主题回滚；密钥与本地状态永不入库。

## 原则

- **分批提交**：一个提交只做一件事。跨层的大改动拆成「后端 / 前端 / 规则」多个提交。
- **本地优先**：默认只提交到本地，**不推送**，除非用户明确要求。
- **密钥零容忍**：任何含密钥的文件必须在首次提交前就写进 `.gitignore`。

## 具体规则

- 提交信息用 Conventional Commits：`<type>(<scope>): <subject>`。允许的 type：`feat` `fix` `perf` `refactor` `docs` `style` `chore` `test`。
- `scope` 用层次名：`core`（codeg）、`ccr`、`agentos`、`agents`（花名册）、`rules`、`archive`、`repo`。
- 主体用中文或英文均可，但要说清**为什么改**，不只说改了什么。
- 提交前检查 `git status` 与 `git diff --staged`，只暂存本次意图内的文件。
- 新增本地产物时同步扩展 `.gitignore`（`.data/`、`node_modules/`、`*.db`；vendored 基座内的产物由各自的 `.gitignore` 覆盖）。
- 不添加 AI 署名（`Co-Authored-By`、`Generated with` 等）。

## 必须忽略

```
.env
.data/
node_modules/
*.db
.DS_Store
```

## 禁止项

- 不使用 `git push --force`、`--no-verify`，不跳过钩子。
- 不提交密钥、令牌、本机绝对路径配置。
- 不把 `Reference/` 里的第三方参考资料当成本仓库代码提交（它是只读参考）。
- 不把多个无关改动塞进同一个提交以便"少提交几次"。

## 检查

- 这个提交的标题能让未来的我一眼知道做了什么吗？
- `git status` 里还有没有不该进来的文件（产物、密钥、本地状态）？
- 是否需要推送？用户有没有明确要求？
