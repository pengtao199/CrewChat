//! 「盘上的客户端配置现在就是这份投影吗」——只读判定，不写盘。
//!
//! [`super::controller::write_proxy`] 决定要不要写盘时，以前只问 cc-switch 自己的
//! live 记账（契约 key 相同就认为没变）。但记账是**自己的记忆**，不是盘上的事实：
//! 配置文件被客户端刷新、被别的程序改回、被用户手改之后，记账还说「没变」，
//! 于是永远跳过写入。这正是多 CLI 并行时「codex 好好的、claude 一直 Not logged in」
//! 的来源——两边都记账说没问题，但盘上只有一个被写进去过。
//!
//! 这里的判定不信任记账：把补丁投影到**当前盘上内容**上，投影结果和盘上内容一样，
//! 才算已经收敛。不一样就是漂移，必须重写。
//!
//! 判定用的补丁和真正写入用的是同一个（`direct_patch` / `CodexConfigPatch`），所以
//! 「判定为已收敛」等价于「再写一次不会有任何字节变化」。

use std::path::Path;

use serde_json::Value;
use toml_edit::DocumentMut;

use crate::live::engine::read_current;
use crate::live::patch::json::JsonPatch;
use crate::live::project::claude::ClaudeProjection;
use crate::live::project::codex::CodexConfigPatch;

/// 读盘上的 live 文件。读不出（不存在 / 格式非法）返回 `None`——按未收敛处理。
fn read_json(path: &Path) -> Option<Value> {
    let bytes = read_current(path).ok().flatten()?;
    serde_json::from_slice::<Value>(&bytes).ok()
}

fn read_toml(path: &Path) -> Option<DocumentMut> {
    let bytes = read_current(path).ok().flatten()?;
    std::str::from_utf8(&bytes).ok()?.parse::<DocumentMut>().ok()
}

/// Claude Code 的 `settings.json` 已经收敛了吗。
///
/// `prev` 是 live 记账里「上一家写进去的」投影：删旧键要靠它算，不能拿目标顶替。
pub(crate) fn claude_converged(
    path: &Path,
    prev: Option<&ClaudeProjection>,
    target: &ClaudeProjection,
) -> bool {
    let Some(live) = read_json(path) else {
        return false;
    };
    let patch: JsonPatch = crate::live::project::claude::direct_patch(prev, target);
    let mut projected = live.clone();
    // 补丁用不了就当没收敛：多写一次是安全的，漏写不是。
    if patch.apply_to(path, &mut projected).is_err() {
        return false;
    }
    live == projected
}

/// Codex 的 `config.toml` 已经收敛了吗。
pub(crate) fn codex_converged(path: &Path, patch: &CodexConfigPatch) -> bool {
    let Some(live) = read_toml(path) else {
        return false;
    };
    let mut projected = live.clone();
    if patch.apply_to(path, &mut projected).is_err() {
        return false;
    }
    live.to_string() == projected.to_string()
}