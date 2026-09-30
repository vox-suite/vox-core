use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    host_trust::HostContextRequest,
    memory::{MemoryOperation, MemoryService},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRequest {
    pub host_context: HostContextRequest,
    pub change: MemoryOperation,
}

pub async fn manage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(agent_key): Path<String>,
    Json(request): Json<MemoryRequest>,
) -> Response {
    let Ok(assertion) = assertion_from_headers(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (Some(trust), Some(db)) = (state.host_trust.as_ref(), state.db.as_ref()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let Ok(context) = trust
        .resolve_authenticated_context(&assertion, &request.host_context, origin, Utc::now())
        .await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match MemoryService::new(db.clone(), None)
        .manage(context.owner(), &agent_key, request.change)
        .await
    {
        Ok(view) => (StatusCode::OK, Json(view)).into_response(),
        Err(sqlx::Error::RowNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn agent_memory_requires_authenticated_host() {
        for change in [
            MemoryOperation::Read,
            MemoryOperation::Clear,
            MemoryOperation::SetRetention { enabled: false },
        ] {
            let request=MemoryRequest{host_context:serde_json::from_value(serde_json::json!({"host_user_id":"untrusted","organization_external_key":null})).unwrap(),change};
            assert_eq!(
                manage(
                    State(AppState::new(true)),
                    HeaderMap::new(),
                    Path("general".into()),
                    Json(request)
                )
                .await
                .status(),
                StatusCode::UNAUTHORIZED
            );
        }
    }
}
