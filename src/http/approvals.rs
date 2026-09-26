/**
* HTTP endpoints for reviewing and resolving pending approvals.
*/
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    approvals::{ApprovalError, CreateProposalRequest},
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
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ProposeRequest {
    pub host_context: HostContextRequest,
    pub proposal: CreateProposalRequest,
}

#[derive(Deserialize)]
pub struct ApproveRequest {
    pub host_context: HostContextRequest,
    pub details: serde_json::Value,
}

pub async fn propose(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<ProposeRequest>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.approvals.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(
        service.propose(&context, r.proposal, Utc::now()).await,
        StatusCode::CREATED,
    )
}

pub async fn approve(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<ApproveRequest>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.approvals.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(
        service.approve(&context, id, r.details, Utc::now()).await,
        StatusCode::OK,
    )
}

fn reply(r: Result<crate::approvals::Proposal, ApprovalError>, ok: StatusCode) -> Response {
    match r {
        Ok(v) => (ok, Json(v)).into_response(),
        Err(ApprovalError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ApprovalError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ApprovalError::UnauthorizedCapability) => StatusCode::FORBIDDEN.into_response(),
        Err(ApprovalError::Expired | ApprovalError::NotApprovable | ApprovalError::Consumed) => {
            StatusCode::CONFLICT.into_response()
        }
        Err(ApprovalError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn context(
    trust: Option<&HostTrustService>,
    headers: &HeaderMap,
    request: HostContextRequest,
) -> Option<crate::identity::ResolvedUserContext> {
    let trust = trust?;
    let assertion = assertion_from_headers(headers).ok()?;
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    trust
        .resolve_authenticated_context(&assertion, &request, origin, Utc::now())
        .await
        .ok()
}
