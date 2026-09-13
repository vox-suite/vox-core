use super::{AppState, auth};
use crate::actions::ActionResultRequest;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

pub async fn record_result(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<ActionResultRequest>,
) -> Response {
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(db) = state.db.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    let status_str = request.status.as_str();
    let mut tx = match db.pool().begin().await {
        Ok(tx) => tx,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let attempt_number = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(attempt_number), 0) + 1 FROM action_attempts WHERE action_id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .unwrap_or(1);

    let _ = sqlx::query(
        "INSERT INTO action_attempts (action_id, attempt_number, state, error_code, provider_metadata) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(attempt_number)
    .bind(status_str)
    .bind(request.error_code.as_deref())
    .bind(serde_json::json!({
        "provider_call_id": request.provider_call_id
    }))
    .execute(&mut *tx)
    .await;

    let result = sqlx::query(
        "UPDATE actions SET state = $1, completed_at = now(), updated_at = now() \
         WHERE id = $2 AND (provider_call_id = $3 OR provider_call_id IS NULL)",
    )
    .bind(status_str)
    .bind(id)
    .bind(&request.provider_call_id)
    .execute(&mut *tx)
    .await;

    match result {
        Ok(_) => {
            if tx.commit().await.is_ok() {
                StatusCode::OK.into_response()
            } else {
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
