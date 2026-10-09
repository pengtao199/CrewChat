# Vendored: cc-switch 核心

本目录是 [cc-switch](https://github.com/farion1231/cc-switch) 的 **Rust 核心源码**，
按需固化进本仓库，使 CrewChat 自带网关核心、**不依赖外部克隆**。

- **Upstream**：`farion1231/cc-switch`（MIT）
- **Vendored commit**：`2db86e94da13365caae55bb08d09295e31500d21`
- **只搬了 `src-tauri/`**（Rust 核心）；cc-switch 的前端不搬（我们用自己的 UI）。
- 许可见 `LICENSE`（原样保留）。

## 我们对上游做的改动

全部带 `[crewchat]` 标记，`rg '\[crewchat\]'` 可列出，便于升级时重放：

| 文件 | 改动 | 原因 |
| --- | --- | --- |
| `src-tauri/src/config.rs` | `get_app_config_dir()` 支持 `CC_SWITCH_HOME` 环境变量 | headless 下把核心的 home 指到我们的目录，不碰用户 `~/.cc-switch/` |
| `src-tauri/src/commands/misc.rs` | `VALID_TOOLS` 改为 `pub` | 我们的 bin 需要它作为扫描范围 |
| `src-tauri/src/lib.rs` | `mod services` / `mod model_capabilities` / `mod database` 改为 `pub mod` | 让 bin 能复用模型拉取、能力判定、价格目录，无需重写 |
| `src-tauri/src/model_capabilities.rs` | `ImageInputCapability` 与 `resolve_image_input_capability` 由 `pub(crate)` 改为 `pub` | 同上（仅可见性，未改判定逻辑） |
| `src-tauri/src/services/model_pricing.rs` | 新增 `list_model_pricing(&Database)` | 与 `commands::usage::get_model_pricing` 同源，但接受 `&Database` 而非 Tauri `State`，供无 Tauri 上下文的 bin 复用 |
| `src-tauri/Cargo.toml` | 新增 `[[bin]] ccsw-proxy` | 独立的 headless 入口 |
| `src-tauri/src/bin/ccsw-proxy.rs` | 新增文件 | 复用 `ProxyService` + `mode::controller` + 工具层 + 模型元数据层 + Stack（`mode::stack/state`），暴露本地控制口 |
| `src-tauri/` 之外的 `dist/index.html` | 占位 | 让 `tauri::generate_context!()` 能编译（我们不打包其 UI） |

模型元数据相关的三个控制口（`/models/fetch`、`/models/capabilities`、`/models/catalog`）全部是**对既有能力的暴露**，判定逻辑与抓取逻辑均未改动，只增加了可见性与路由。

聚合（Stack）控制口 `/stack/route`（`{app, provider_id, model}` → `{model_id}`）同样是**对既有能力的暴露**：
它按需 `controller::enter_with_route(stack=true)` + `controller::set_stack_member` 后，用
`mode::state::stack` + `mode::stack::encode` 计算发布 id。路由判定（`decode`/`resolve`）与发布逻辑均未改动。

## 构建

```bash
cargo build --release --bin ccsw-proxy --manifest-path gateway/cc-switch/src-tauri/Cargo.toml
# 产物：gateway/cc-switch/src-tauri/target/release/ccsw-proxy
```

`target/` 已被 gitignore（构建产物不入库）。

## 升级上游

1. 记下新的 upstream commit；
2. 用 `git diff` 对照本目录与上游同文件，重放上表各处改动（`rg '\[crewchat\]'` 定位）；
3. 更新本文件的 Vendored commit；
4. 重新构建并跑 `scripts/` 的检查。
