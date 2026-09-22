/**
* HTTP endpoints for managing execution policies and rules.
*/
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    execution_policy::{ExecutionPolicyError, OperationalQuotaRequest, SpendingPolicyRequest},
    host_trust::{HostContextRequest, HostTrustService},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct SetSpendingPolicyRequest {
    pub host_context: HostContextRequest,
    pub policy: SpendingPolicyRequest,
}

#[derive(Deserialize)]
pub struct SetOperationalQuotaRequest {
    pub host_context: HostContextRequest,
    pub quota: OperationalQuotaRequest,
}

pub async fn set_spending_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SetSpendingPolicyRequest>,
) -> Response {
    let (Some(service), Some(context)) = (
        state.execution_policy.as_ref(),
        context(state.host_trust.as_deref(), &headers, request.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(service.set_spending_policy(&context, request.policy).await)
}

pub async fn set_operational_quota(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SetOperationalQuotaRequest>,
) -> Response {
    let (Some(service), Some(context)) = (
        state.execution_policy.as_ref(),
        context(state.host_trust.as_deref(), &headers, request.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(service.set_operational_quota(&context, request.quota).await)
}

fn reply(result: Result<(), ExecutionPolicyError>) -> Response {
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(ExecutionPolicyError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(
            ExecutionPolicyError::ApprovalRequired
            | ExecutionPolicyError::FreshProposalRequired
            | ExecutionPolicyError::SpendingPolicyExceeded
            | ExecutionPolicyError::QuotaExhausted,
        ) => StatusCode::CONFLICT.into_response(),
        Err(ExecutionPolicyError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
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
