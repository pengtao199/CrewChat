use crate::store::{MaterializeError, SkillRef, Store};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub token: Arc<String>,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    missing: Option<Vec<String>>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            missing: None,
        }
    }

    fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid bearer token",
        )
    }

    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "error": {
                "code": self.code,
                "message": self.message,
            }
        });
        if let Some(missing) = self.missing {
            body["missing"] = json!(missing);
        }
        (self.status, Json(body)).into_response()
    }
}

#[derive(Debug, Deserialize)]
pub struct InstallRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterializeRequest {
    pub home: String,
    pub skills_dir: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub skills: Vec<SkillRef>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCollectionRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/skills", get(list_skills))
        .route("/v1/skills/install", post(install_skill))
        .route("/v1/skills/{id}", get(get_skill))
        .route("/v1/sessions/materialize", post(materialize))
        .route(
            "/v1/collections",
            get(list_collections).post(create_collection),
        )
        .with_state(state)
}

async fn health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&headers, &state.token)?;
    Ok(Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    })))
}

async fn list_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let skills = lock_store(&state)?.list().map_err(internal)?;
    Ok(Json(json!({ "skills": skills })))
}

async fn get_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let detail = lock_store(&state)?
        .get(&id)
        .map_err(internal)?
        .ok_or_else(|| ApiError::not_found(format!("skill not found: {id}")))?;
    Ok(Json(serde_json::to_value(detail).map_err(internal)?))
}

async fn install_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<InstallRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&headers, &state.token)?;
    if request.path.trim().is_empty() {
        return Err(ApiError::bad_request("invalid_input", "path is required"));
    }
    let skill = lock_store(&state)?
        .install_local(std::path::Path::new(request.path.trim()))
        .map_err(|error| ApiError::bad_request("install_failed", error.to_string()))?;
    Ok(Json(json!({ "skill": skill })))
}

async fn materialize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<MaterializeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&headers, &state.token)?;
    if request.home.trim().is_empty() {
        return Err(ApiError::bad_request("invalid_input", "home is required"));
    }

    let home = std::path::PathBuf::from(request.home.trim());
    let skills_dir = request
        .skills_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from);

    let result = lock_store(&state)?.materialize(&home, skills_dir.as_deref(), &request.skills);
    match result {
        Ok(materialized) => Ok(Json(serde_json::to_value(materialized).map_err(internal)?)),
        Err(MaterializeError::Missing(missing)) => {
            let mut error = ApiError::new(
                StatusCode::CONFLICT,
                "skill_missing",
                format!("skills not installed: {}", missing.join(", ")),
            );
            error.missing = Some(missing);
            Err(error)
        }
        Err(MaterializeError::Message(message)) => {
            Err(ApiError::bad_request("materialize_failed", message))
        }
    }
}

async fn list_collections(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let collections = lock_store(&state)?.list_collections().map_err(internal)?;
    Ok(Json(json!({ "collections": collections })))
}

async fn create_collection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateCollectionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(&headers, &state.token)?;
    let collection = lock_store(&state)?
        .create_collection(
            &request.name,
            request.description.as_deref().unwrap_or_default(),
            &request.skills,
        )
        .map_err(|error| ApiError::bad_request("collection_failed", error.to_string()))?;
    Ok(Json(json!({ "collection": collection })))
}

fn authorize(headers: &HeaderMap, token: &str) -> Result<(), ApiError> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    if value == Some(token) {
        Ok(())
    } else {
        Err(ApiError::unauthorized())
    }
}

fn lock_store(state: &AppState) -> Result<std::sync::MutexGuard<'_, Store>, ApiError> {
    state
        .store
        .lock()
        .map_err(|_| ApiError::internal("skill store lock is poisoned"))
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError::internal(error.to_string())
}
