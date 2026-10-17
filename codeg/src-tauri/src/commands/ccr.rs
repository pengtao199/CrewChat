//! [crewchat] Tauri command surface for the CCR management proxy and the
//! management-process supervisor.
//!
//! The transport lives in [`crate::ccr::client`] and the supervision in
//! [`crate::ccr::supervisor`]; this file only adapts them to commands. The
//! server-mode equivalents are the `/api/ccr_*` handlers in
//! `crate::web::handlers::ccr`.

#[cfg(feature = "tauri-runtime")]
use serde_json::Value;

#[cfg(feature = "tauri-runtime")]
use crate::app_error::AppCommandError;
#[cfg(feature = "tauri-runtime")]
use crate::ccr::supervisor::CcrDaemonStatus;

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn ccr_health() -> Value {
    crate::ccr::client::health()
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn ccr_call(method: String, args: Option<Vec<Value>>) -> Result<Value, AppCommandError> {
    crate::ccr::client::call(&method, args).await
}

/// 探活: the live management-process status (probes, starts nothing).
#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn ccr_supervisor_state(
    supervisor: tauri::State<'_, std::sync::Arc<crate::ccr::CcrSupervisor>>,
) -> Result<CcrDaemonStatus, AppCommandError> {
    Ok(supervisor.refresh().await)
}

/// 接管 / 拉起: adopt a running instance, or start one when it is down.
#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn ccr_supervisor_start(
    supervisor: tauri::State<'_, std::sync::Arc<crate::ccr::CcrSupervisor>>,
) -> Result<CcrDaemonStatus, AppCommandError> {
    supervisor
        .ensure_running()
        .await
        .map_err(AppCommandError::dependency_missing)
}
