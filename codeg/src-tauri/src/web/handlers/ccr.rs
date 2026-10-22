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
use crate::ccr::provider_toggles;
use crate::ccr::providers;
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

#[derive(Deserialize)]
pub struct CcrProviderIdParams {
    pub id: String,
}

/// Create or update one provider (read → merge → saveConfig in the core).
pub async fn ccr_provider_save(
    Json(params): Json<providers::ProviderSaveRequest>,
) -> Result<Json<Value>, AppCommandError> {
    Ok(Json(providers::save_provider(params).await?))
}

/// Remove one provider; refuses while a profile still references it.
pub async fn ccr_provider_delete(
    Json(params): Json<CcrProviderIdParams>,
) -> Result<Json<bool>, AppCommandError> {
    Ok(Json(providers::delete_provider(params.id).await?))
}

/// [crewchat] 模型页行内开关：启用/停用单个模型（模型清单里的 Toggle）。
pub async fn ccr_provider_set_model(
    Json(params): Json<provider_toggles::ModelToggleRequest>,
) -> Result<Json<bool>, AppCommandError> {
    Ok(Json(provider_toggles::set_model_enabled(params).await?))
}

/// [crewchat] 已停用模型清单：{ "<provider id/name>": ["model", …] }。
pub async fn ccr_disabled_models() -> Result<Json<Value>, AppCommandError> {
    provider_toggles::disabled_models().map(Json)
}

/// [crewchat] 模型页行内开关：启用/停用单条凭据（凭据清单里的 Toggle）。
pub async fn ccr_provider_set_credential(
    Json(params): Json<provider_toggles::CredentialToggleRequest>,
) -> Result<Json<bool>, AppCommandError> {
    Ok(Json(provider_toggles::set_credential_enabled(params).await?))
}

/// Probe a provider base URL and return the detected models/protocol.
pub async fn ccr_provider_probe(
    Json(params): Json<providers::ProviderProbeRequest>,
) -> Result<Json<Value>, AppCommandError> {
    Ok(Json(providers::probe_provider(params).await?))
}
