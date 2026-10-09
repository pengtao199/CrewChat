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
| `src-tauri/Cargo.toml` | 新增 `[[bin]] ccsw-proxy` | 独立的 headless 入口 |
| `src-tauri/src/bin/ccsw-proxy.rs` | 新增文件 | 复用 `ProxyService` + `mode::controller` + 工具层，暴露本地控制口 |
| `src-tauri/` 之外的 `dist/index.html` | 占位 | 让 `tauri::generate_context!()` 能编译（我们不打包其 UI） |

## 构建

```bash
cargo build --release --bin ccsw-proxy --manifest-path gateway/cc-switch/src-tauri/Cargo.toml
# 产物：gateway/cc-switch/src-tauri/target/release/ccsw-proxy
```

`target/` 已被 gitignore（构建产物不入库）。

## 升级上游

1. 记下新的 upstream commit；
2. 用 `git diff` 对照本目录与上游同文件，重放上表 5 处改动（`rg '\[crewchat\]'` 定位）；
3. 更新本文件的 Vendored commit；
4. 重新构建并跑 `scripts/` 的检查。
