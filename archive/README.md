# archive

冻结与历史，**只读，不参与任何构建与运行**。改动产品行为请去 `codeg/`（核心）或 `AgentOS/`（壳）。

| 目录 | 是什么 | 为什么留着 |
| --- | --- | --- |
| `python-orchestrator/` | v1 的 Python 编排层原样（`src/crewchat/` L0–L4 + 测试 + 两个契约检查脚本 + `pyproject.toml` / `uv.lock` / `.env`）；分层契约见 [CONTRACT.md](python-orchestrator/CONTRACT.md) | 2026-10 起产品运行时移交 Codeg，该层「冻结为参考（不删不接）」。保留可读，便于对照旧实现的领域语义与 HTTP 契约。 |

## 使用

```bash
cd archive/python-orchestrator
python3 scripts/check_layers.py      # 分层依赖方向
python3 scripts/check_imports.py     # 导入可解析性
```

两个脚本只用标准库，不需要虚拟环境。`archive/` 下的历史文件不参与导入检查。

若要重新运行这一层，需在 `archive/python-orchestrator/` 里用 `uv sync` 重建环境（仓库已不再保留 `.venv`）。

## 注意

- 本目录内的文档引用的是**移动前的路径**（当时 `src/`、`scripts/` 在仓库根），不要按它们去找文件。
- 活的规则在 `AGENTS/`，活的计划在 `docs/plans/`。
