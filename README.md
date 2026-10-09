# CrewChat

多 Agent 协作桌面应用：Python 后端（crewai 编排 + 身体抽象）+ fork 自 AionUi 的前端外壳 +
以 cc-switch 代理核心为网关。

## 分层

| 层 | 路径 | 职责 |
| --- | --- | --- |
| 前端外壳 | `ui/` | 渲染与交互（fork 自 AionUi，Apache-2.0）；不含业务规则 |
| 后端 | `src/crewchat/` | 领域模型、编排、执行器、持久化、HTTP/WS |
| 网关 | `gateway/cc-switch/` | cc-switch Rust 核心（headless `ccsw-proxy`），协议归一化 |

后端为 `core`（L0 基础 / L1 基础设施）→ `executors`（L2 身体）→ `orchestration`（L3）→
`api`（L4 契约），依赖只向下。详见 `AGENTS/architecture.md`。

## 快速开始

```bash
uv sync                       # 安装后端依赖
uv run crewchat --no-browser  # 启动后端（默认 http://127.0.0.1:8000）
```

前端源码在 `ui/`，构建产物由后端托管：

```bash
cd ui && npm install && npm run build   # 产出 ui/dist
```

网关二进制构建方式见 `gateway/cc-switch/NOTICE.md`。

## 自检

```bash
uv run python scripts/check_layers.py    # 依赖方向（分层契约）
uv run python scripts/check_imports.py   # 相对导入可解析
```

## 数据与规则

- 数据落点（真相源）见 `AGENTS/truth-sources.md`；`.data/` 为本地私有状态，不入库。
- 改动前请遵循 `AGENTS.md` 的执行入口与子规则索引。
- 进行中的方案见 `docs/plans/`。
