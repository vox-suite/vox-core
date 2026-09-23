/**
* HTTP endpoints for triggering agent workflow execution.
*/
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    execution::{ExecutionError, StartExecutionRequest},
    host_trust::{HostContextRequest, HostTrustService},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct StartRequest {
    pub host_context: HostContextRequest,
    #[serde(flatten)]
    pub execution: StartExecutionRequest,
}

#[derive(Deserialize)]
pub struct GetRequest {
    pub host_context: HostContextRequest,
}

pub async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<StartRequest>,
) -> Response {
    let (Some(service), Some(context)) = (
        state.execution.as_ref(),
        context(state.host_trust.as_deref(), &headers, request.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(
        service.start(&context, request.execution, Utc::now()).await,
        StatusCode::CREATED,
    )
}

pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<GetRequest>,
) -> Response {
    let Some(context) = context(state.host_trust.as_deref(), &headers, request.host_context).await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(db) = state.db.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match sqlx::query("SELECT id,state,provider_reference,confirmation_evidence FROM executions WHERE id=$1 AND user_id=$2").bind(id).bind(context.user_id.0).fetch_optional(db.pool()).await { Ok(Some(row))=>(StatusCode::OK,Json(crate::execution::Execution{id:row.get("id"),state:row.get("state"),provider_reference:row.get("provider_reference"),confirmation_evidence:row.get("confirmation_evidence")})).into_response(),Ok(None)=>StatusCode::NOT_FOUND.into_response(),Err(_)=>StatusCode::SERVICE_UNAVAILABLE.into_response()}
}

fn reply(result: Result<crate::execution::Execution, ExecutionError>, ok: StatusCode) -> Response {
    match result {
        Ok(value) => (ok, Json(value)).into_response(),
        Err(ExecutionError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ExecutionError::Unavailable) => StatusCode::NOT_FOUND.into_response(),
        Err(ExecutionError::FreshApproval) => StatusCode::CONFLICT.into_response(),
        Err(ExecutionError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn context(
    trust: Option<&HostTrustService>,
    headers: &HeaderMap,
    request: HostContextRequest,
) -> Option<crate::identity::ResolvedUserContext> {
    let trust = trust?;
    let assertion = assertion_from_headers(headers).ok()?;
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    trust
        .resolve_authenticated_context(&assertion, &request, origin, Utc::now())
        .await
        .ok()
}
