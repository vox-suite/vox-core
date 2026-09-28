/**
* HTTP endpoints for connecting remote MCP apps through provider OAuth.
*/
use super::{AppState, remote_extensions::context};
use crate::{
    connected_apps::ConnectedAppError,
    execution::{AdapterOutcome, StartExecutionRequest},
    host_trust::HostContextRequest,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct AuthorizeRequest {
    pub host_context: HostContextRequest,
    pub redirect_uri: String,
}

#[derive(Deserialize)]
pub struct CallbackRequest {
    pub host_context: HostContextRequest,
    pub state: String,
    pub code: String,
    pub iss: Option<String>,
}

#[derive(Deserialize)]
pub struct StatusRequest {
    pub host_context: HostContextRequest,
}

#[derive(Deserialize)]
pub struct ReadToolRequest {
    pub host_context: HostContextRequest,
    pub agent_external_key: String,
    pub connection_id: Uuid,
    pub tool_name: String,
    pub arguments: Value,
}

#[derive(Deserialize)]
pub struct ExecuteToolRequest {
    pub host_context: HostContextRequest,
    pub approval_id: Uuid,
    pub idempotency_key: String,
}

pub async fn authorize(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<AuthorizeRequest>,
) -> Response {
    let Some(apps) = s.connected_apps.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match apps.begin(&c.request_context(), id, &r.redirect_uri).await {
        Ok(start) => (StatusCode::OK, Json(start)).into_response(),
        Err(e) => error(e),
    }
}

pub async fn connect_public(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<StatusRequest>,
) -> Response {
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(apps) = s.connected_apps.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match apps.connect_public(&c, id).await {
        Ok(extension) => (StatusCode::OK, Json(extension)).into_response(),
        Err(e) => error(e),
    }
}

pub async fn callback(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<CallbackRequest>,
) -> Response {
    let Some(apps) = s.connected_apps.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match apps
        .complete_with_issuer(&c.request_context(), &r.state, &r.code, r.iss.as_deref())
        .await
    {
        Ok(extension) => (StatusCode::OK, Json(extension)).into_response(),
        Err(e) => error(e),
    }
}

pub async fn status(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<StatusRequest>,
) -> Response {
    let Some(apps) = s.connected_apps.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match apps.connections(&c.request_context()).await {
        Ok(connected) => (
            StatusCode::OK,
            Json(json!({
                "configured_hosts": apps.configured_hosts(),
                "connected": connected,
            })),
        )
            .into_response(),
        Err(e) => error(e),
    }
}

pub async fn read_tool(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<ReadToolRequest>,
) -> Response {
    let Some(apps) = s.connected_apps.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match apps
        .read_tool(
            &c.request_context(),
            &r.agent_external_key,
            r.connection_id,
            &r.tool_name,
            r.arguments,
        )
        .await
    {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => error(e),
    }
}

pub async fn execute_tool(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<ExecuteToolRequest>,
) -> Response {
    let (Some(apps), Some(executions), Some(db)) = (
        s.connected_apps.as_ref(),
        s.execution.as_ref(),
        s.db.as_ref(),
    ) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let row = match sqlx::query(
        "SELECT p.actor_key,p.capability,p.connection_id,p.details \
         FROM action_approvals a JOIN action_proposals p ON p.id=a.proposal_id \
           AND p.user_context_id=a.user_context_id \
         JOIN external_connections x ON x.id=p.connection_id \
           AND x.user_context_id=p.user_context_id \
         WHERE a.id=$1 AND a.user_context_id=$2 AND p.state='approved' \
           AND x.remote_extension_id IS NOT NULL",
    )
    .bind(r.approval_id)
    .bind(c.id.0)
    .fetch_optional(db.pool())
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return StatusCode::CONFLICT.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let details: Value = row.get("details");
    let capability: String = row.get("capability");
    let arguments = details.pointer("/invocation/arguments").cloned();
    if details
        .pointer("/invocation/tool_name")
        .and_then(Value::as_str)
        != Some(capability.as_str())
        || !arguments.as_ref().is_some_and(Value::is_object)
    {
        return StatusCode::CONFLICT.into_response();
    }
    let execution = match executions
        .start(
            &c,
            StartExecutionRequest {
                approval_id: r.approval_id,
                idempotency_key: r.idempotency_key,
            },
            chrono::Utc::now(),
        )
        .await
    {
        Ok(execution) => execution,
        Err(_) => return StatusCode::CONFLICT.into_response(),
    };
    if execution.state != "pending" {
        return (StatusCode::OK, Json(execution)).into_response();
    }
    // This transition is the dispatch claim. A concurrent request with the
    // same idempotency key cannot move reconciling back to dispatchable.
    if executions
        .record_outcome(
            &c,
            execution.id,
            AdapterOutcome::Reconciling {
                provider_reference: None,
            },
            chrono::Utc::now(),
        )
        .await
        .is_err()
    {
        return match executions.get(&c, execution.id).await {
            Ok(current) => (StatusCode::OK, Json(current)).into_response(),
            Err(_) => StatusCode::CONFLICT.into_response(),
        };
    }
    let result = apps
        .approved_tool(
            &c.request_context(),
            &row.get::<String, _>("actor_key"),
            row.get("connection_id"),
            &capability,
            arguments.unwrap_or(Value::Null),
        )
        .await;
    let outcome = match result {
        Ok(value) if value.get("isError").and_then(Value::as_bool) != Some(true) => {
            // The MCP server is the actor making the change. Its own result
            // cannot independently prove that the provider committed it.
            AdapterOutcome::Unknown {
                provider_reference: value
                    .pointer("/_meta/provider_reference")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                code: "unconfirmed_provider_result".into(),
            }
        }
        Ok(_) => AdapterOutcome::Unknown {
            provider_reference: None,
            code: "provider_reported_error".into(),
        },
        Err(e) => {
            tracing::warn!(error=%e, execution_id=%execution.id, "MCP execution needs reconciliation");
            AdapterOutcome::Unknown {
                provider_reference: None,
                code: "dispatch_uncertain".into(),
            }
        }
    };
    match executions
        .record_dispatched_outcome(&c, execution.id, outcome, chrono::Utc::now())
        .await
    {
        Ok(current) => (StatusCode::OK, Json(current)).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// Errors carry a short user-facing reason; provider details stay in logs.
fn error(e: ConnectedAppError) -> Response {
    let (status, code) = match &e {
        ConnectedAppError::Invalid => (StatusCode::BAD_REQUEST, "invalid_request"),
        ConnectedAppError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        ConnectedAppError::NotConfigured => (StatusCode::SERVICE_UNAVAILABLE, "not_configured"),
        ConnectedAppError::ClientNotConfigured => {
            (StatusCode::PRECONDITION_FAILED, "client_not_configured")
        }
        ConnectedAppError::Unauthorized => (StatusCode::BAD_GATEWAY, "provider_rejected"),
        ConnectedAppError::GrantRequired => (StatusCode::FORBIDDEN, "grant_required"),
        ConnectedAppError::WriteRequiresApproval => (StatusCode::FORBIDDEN, "approval_required"),
        ConnectedAppError::Expired => (StatusCode::GONE, "authorization_expired"),
        ConnectedAppError::Provider(_)
        | ConnectedAppError::SessionExpired
        | ConnectedAppError::UnknownTool => (StatusCode::BAD_GATEWAY, "provider_error"),
        ConnectedAppError::Timeout => (StatusCode::GATEWAY_TIMEOUT, "provider_error"),
        ConnectedAppError::Extension(_) => (StatusCode::CONFLICT, "extension_state"),
        ConnectedAppError::Crypto | ConnectedAppError::Database(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
        }
    };
    tracing::warn!(error = %e, code, "connected app request failed");
    let message = match &e {
        ConnectedAppError::Provider(_)
        | ConnectedAppError::Database(_)
        | ConnectedAppError::Crypto => {
            "The connected service is unavailable or returned an error".to_string()
        }
        _ => e.to_string(),
    };
    (status, Json(json!({"error": code, "message": message}))).into_response()
}
