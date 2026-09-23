/**
* HTTP endpoints for querying security audit logs.
*/
use super::{AppState, auth};
use crate::audit::{AuditError, AuditQuery};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct QueryParams {
    after: Option<i64>,
    limit: Option<i64>,
    user_context_id: Option<Uuid>,
    aggregate_id: Option<Uuid>,
    execution_id: Option<Uuid>,
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<QueryParams>,
) -> axum::response::Response {
    let Some(admin) = state.admin.as_ref() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if admin.token().trim().is_empty() || !auth::authorized(&headers, admin.token()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(audit) = state.audit.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let request = AuditQuery {
        after: query.after,
        limit: query.limit,
        user_context_id: query.user_context_id,
        aggregate_id: query.aggregate_id,
        execution_id: query.execution_id,
    };
    match audit.list(request).await {
        Ok(events) => {
            let next_cursor = events
                .last()
                .map(|event| event.cursor)
                .unwrap_or(query.after.unwrap_or(0));
            let _ = audit.record_operator_access("audit-admin", "audit.accessed", serde_json::json!({"operation":"list","result_count":events.len(),"next_cursor":next_cursor})).await;
            (
                [(header::CACHE_CONTROL, "no-store")],
                Json(serde_json::json!({"events":events,"next_cursor":next_cursor})),
            )
                .into_response()
        }
        Err(AuditError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(AuditError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
