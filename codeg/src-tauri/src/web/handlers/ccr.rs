//! [crewchat] Web-mode handlers for the CCR management proxy and the
//! management-process supervisor.
//!
//! See `crate::ccr::client` for the transport and environment contract and
//! `crate::ccr::supervisor` for probe / adopt / start / guard.

use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;
use serde_json::Value;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::ccr::client;
use crate::ccr::supervisor::CcrDaemonStatus;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcrCallParams {
    pub method: String,
    #[serde(default)]
    pub args: Option<Vec<Value>>,
}

pub async fn ccr_health() -> Json<Value> {
    Json(client::health())
}

pub async fn ccr_call(Json(params): Json<CcrCallParams>) -> Result<Json<Value>, AppCommandError> {
    let value = client::call(&params.method, params.args).await?;
    Ok(Json(value))
}

/// 探活: probe the management process and report its state (starts nothing).
pub async fn ccr_supervisor_state(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<CcrDaemonStatus>, AppCommandError> {
    Ok(Json(state.ccr_supervisor.refresh().await))
}

/// 接管 / 拉起: adopt a running instance, or start one when it is down.
pub async fn ccr_supervisor_start(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<CcrDaemonStatus>, AppCommandError> {
    state
        .ccr_supervisor
        .ensure_running()
        .await
        .map(Json)
        .map_err(AppCommandError::dependency_missing)
}
