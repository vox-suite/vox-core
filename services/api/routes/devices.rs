/**
* HTTP handlers for device pairing, tokens, and registration.
*/
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use vox_core::{
    application::devices::{DeviceService, RegisterDeviceInput},
    domain::{devices::local_llm_capable, identity::Actor},
};

#[derive(Clone)]
pub struct DeviceApiState {
    pub devices: DeviceService,
    pub pool: PgPool,
}

pub async fn register_device(
    State(state): State<DeviceApiState>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<RegisterDeviceInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let device = state
        .devices
        .register_device(&actor, input)
        .await
        .map_err(|err| {
            tracing::warn!(user_id = %actor.user_id, %err, "Device registration failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    tracing::info!(
        device_id = %device.id,
        user_id = %actor.user_id,
        platform = %device.platform,
        label = %device.label,
        local_llm_capable = local_llm_capable(&device.capabilities),
        "Device registered"
    );
    Ok((StatusCode::CREATED, Json(device)))
}

pub async fn heartbeat(
    State(state): State<DeviceApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let ok = state
        .devices
        .heartbeat(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if ok {
        Ok(StatusCode::OK)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

#[derive(Debug, Deserialize)]
pub struct ClaimJobsRequest {
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ClaimedExecution {
    pub attempt_id: Uuid,
    pub execution_id: Uuid,
    pub action_type: String,
    pub target: String,
    pub payload: serde_json::Value,
}

pub async fn claim_device_jobs(
    State(state): State<DeviceApiState>,
    Extension(actor): Extension<Actor>,
    Path(device_id): Path<Uuid>,
    Json(body): Json<ClaimJobsRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let limit = body.limit.unwrap_or(5).clamp(1, 20);

    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let executions = sqlx::query(
        "WITH candidates AS ( \
            SELECT id FROM executions \
            WHERE user_id = $1 AND (device_id = $2 OR device_id IS NULL) AND state = 'pending' \
            ORDER BY created_at \
            FOR UPDATE SKIP LOCKED LIMIT $3 \
         ) \
         UPDATE executions \
         SET state = 'running', device_id = $2, updated_at = now() \
         FROM candidates \
         WHERE executions.id = candidates.id \
         RETURNING executions.id, executions.action_type, executions.target, executions.payload",
    )
    .bind(actor.user_id)
    .bind(device_id)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut claimed = Vec::new();
    for row in executions {
        let exec_id: Uuid = row.get("id");
        let action_type: String = row.get("action_type");
        let target: String = row.get("target");
        let payload: serde_json::Value = row.get("payload");

        let attempt_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO execution_attempts (execution_id, device_id, attempt_number, status) \
             VALUES ($1, $2, 1, 'running') \
             RETURNING id",
        )
        .bind(exec_id)
        .bind(device_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        claimed.push(ClaimedExecution {
            attempt_id,
            execution_id: exec_id,
            action_type,
            target,
            payload,
        });
    }

    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(claimed))
}

#[derive(Debug, Deserialize)]
pub struct SubmitJobResultRequest {
    pub status: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

pub async fn submit_job_result(
    State(state): State<DeviceApiState>,
    Extension(actor): Extension<Actor>,
    Path(attempt_id): Path<Uuid>,
    Json(body): Json<SubmitJobResultRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let attempt_info = sqlx::query(
        "SELECT ea.execution_id, e.user_id \
         FROM execution_attempts ea \
         JOIN executions e ON e.id = ea.execution_id \
         WHERE ea.id = $1",
    )
    .bind(attempt_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (execution_id, owner_id) = match attempt_info {
        Some(row) => {
            let exec_id: Uuid = row.get("execution_id");
            let user_id: Uuid = row.get("user_id");
            (exec_id, user_id)
        }
        None => return Err(StatusCode::NOT_FOUND),
    };

    if owner_id != actor.user_id {
        return Err(StatusCode::FORBIDDEN);
    }

    let status_str = match body.status.as_str() {
        "completed" => "completed",
        _ => "failed",
    };

    sqlx::query(
        "UPDATE execution_attempts \
         SET status = $1, result = $2, error = $3, finished_at = $4 \
         WHERE id = $5",
    )
    .bind(status_str)
    .bind(body.result)
    .bind(body.error)
    .bind(Utc::now())
    .bind(attempt_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    sqlx::query(
        "UPDATE executions \
         SET state = $1, updated_at = now() \
         WHERE id = $2",
    )
    .bind(status_str)
    .bind(execution_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::OK)
}
