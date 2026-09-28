use super::{AppState, remote_extensions::context};
use crate::agents::tools::library::{AgentLibrary, LibraryRequest};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
#[derive(serde::Deserialize)]
pub struct Request {
    pub host_context: crate::host_trust::HostContextRequest,
    pub agent_external_key: String,
    pub operation: LibraryRequest,
}
pub async fn invoke(State(s): State<AppState>, h: HeaderMap, Json(r): Json<Request>) -> Response {
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(db) = s.db else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match AgentLibrary::new(Some(db), s.connected_apps, c, r.agent_external_key)
        .invoke(r.operation)
        .await
    {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(_) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error":"library_operation_unavailable"})),
        )
            .into_response(),
    }
}
