//! [crewchat] Web-mode handlers for the CCR management proxy.
//!
//! See `crate::commands::ccr` for the transport and environment contract.

use axum::Json;
use serde::Deserialize;
use serde_json::Value;

use crate::app_error::AppCommandError;
use crate::commands::ccr as ccr_commands;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CcrCallParams {
    pub method: String,
    #[serde(default)]
    pub args: Option<Vec<Value>>,
}

pub async fn ccr_health() -> Json<Value> {
    Json(ccr_commands::ccr_health_core())
}

pub async fn ccr_call(Json(params): Json<CcrCallParams>) -> Result<Json<Value>, AppCommandError> {
    let value = ccr_commands::ccr_call_core(&params.method, params.args).await?;
    Ok(Json(value))
}
