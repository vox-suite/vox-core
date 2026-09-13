use super::{AppState, auth};
use crate::actions::{ActionResultRequest, ActionResultStatus};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use sqlx::Row;
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
    let result: Result<StatusCode, sqlx::Error> = async {
        let mut tx = db.pool().begin().await?;
        let row = sqlx::query("SELECT state, provider_call_id FROM actions WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            return Ok(StatusCode::NOT_FOUND);
        };
        let current_state: String = row.get("state");
        let provider_call_id: Option<String> = row.get("provider_call_id");
        if provider_call_id.as_deref() != Some(request.provider_call_id.as_str()) {
            return Ok(StatusCode::CONFLICT);
        }
        let target_state = request.status.as_str();
        if matches!(current_state.as_str(), "succeeded" | "failed") {
            return Ok(if current_state == target_state {
                StatusCode::OK
            } else {
                StatusCode::CONFLICT
            });
        }
        sqlx::query(
            "UPDATE actions SET state = $1, completed_at = now(), updated_at = now() WHERE id = $2",
        )
        .bind(target_state)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let attempt_state = match request.status {
            ActionResultStatus::Succeeded => "accepted",
            ActionResultStatus::Failed => "failed",
        };
        sqlx::query(
            "UPDATE action_attempts SET state = $1, error_code = $2, completed_at = now() \
             WHERE action_id = $3 AND attempt_number = (SELECT MAX(attempt_number) FROM action_attempts WHERE action_id = $3)",
        )
        .bind(attempt_state)
        .bind(request.error_code.as_deref())
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(StatusCode::OK)
    }
    .await;
    match result {
        Ok(status) => status.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
