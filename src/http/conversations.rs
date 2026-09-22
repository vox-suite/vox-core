/**
* HTTP endpoints for conversation turns, audio streams, and history.
*/
use super::{AppState, host_apps::assertion_from_headers};
use crate::conversations::{
    CompleteConversationRequest, RespondRequest, RespondResponse, service::ConversationError,
};
use crate::host_trust::{HostContextRequest, HostTrustService};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use futures_util::StreamExt;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AuthenticatedRespondRequest {
    pub host_context: HostContextRequest,
    #[serde(flatten)]
    pub conversation: RespondRequest,
}

#[derive(Deserialize)]
pub struct AuthenticatedCompleteRequest {
    pub host_context: HostContextRequest,
    #[serde(flatten)]
    pub conversation: CompleteConversationRequest,
}

#[derive(Deserialize)]
pub struct AuthenticatedSpeculateRequest {
    pub host_context: HostContextRequest,
    #[serde(flatten)]
    pub conversation: crate::conversations::SpeculateRequest,
}

pub async fn respond(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedRespondRequest>,
) -> Response {
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(context) = authenticated_channel_context(
        state.host_trust.as_deref(),
        &headers,
        &request.host_context,
        &request.conversation.identity,
    )
    .await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service
        .respond_for_owner(context.owner(), request.conversation)
        .await
    {
        Ok(response) => (StatusCode::OK, Json::<RespondResponse>(response)).into_response(),
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ConversationError::Agent(_)) => StatusCode::BAD_GATEWAY.into_response(),
        Err(
            ConversationError::Database(_)
            | ConversationError::Identity(_)
            | ConversationError::IdentityConflict
            | ConversationError::NotFound,
        ) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn respond_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedRespondRequest>,
) -> Response {
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(context) = authenticated_channel_context(
        state.host_trust.as_deref(),
        &headers,
        &request.host_context,
        &request.conversation.identity,
    )
    .await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service
        .respond_stream_for_owner(context.owner(), request.conversation)
        .await
    {
        Ok(stream) => {
            let sse_stream = stream.map(|item| match item {
                Ok(delta) if delta == crate::conversations::speculation::LOOKUP_PENDING => {
                    Ok("event: lookup_pending\ndata: {}\n\n".to_string())
                }
                Ok(delta) => {
                    let data = serde_json::json!({ "delta": delta }).to_string();
                    Ok::<_, std::convert::Infallible>(format!("data: {data}\n\n"))
                }
                Err(_) => Ok::<_, std::convert::Infallible>(
                    "event: error\ndata: {\"error\":\"agent error\"}\n\n".to_string(),
                ),
            });
            let done_stream = futures_util::stream::once(async move {
                Ok::<_, std::convert::Infallible>("data: [DONE]\n\n".to_string())
            });
            let full_stream = sse_stream.chain(done_stream);

            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .header("connection", "keep-alive")
                .body(axum::body::Body::from_stream(full_stream))
                .unwrap()
        }
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ConversationError::Agent(_)) => StatusCode::BAD_GATEWAY.into_response(),
        Err(
            ConversationError::Database(_)
            | ConversationError::Identity(_)
            | ConversationError::IdentityConflict
            | ConversationError::NotFound,
        ) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedCompleteRequest>,
) -> Response {
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(context) = authenticated_channel_context(
        state.host_trust.as_deref(),
        &headers,
        &request.host_context,
        &request.conversation.identity,
    )
    .await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service
        .complete_for_owner(context.owner(), request.conversation)
        .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn speculate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedSpeculateRequest>,
) -> Response {
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if authenticated_channel_context(
        state.host_trust.as_deref(),
        &headers,
        &request.host_context,
        &request.conversation.identity,
    )
    .await
    .is_none()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match service.speculate(request.conversation).await {
        Ok(status) => Json(serde_json::json!({"status":status})).into_response(),
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn authenticated_channel_context(
    trust: Option<&HostTrustService>,
    headers: &HeaderMap,
    context: &HostContextRequest,
    identity: &crate::identity::ChannelIdentity,
) -> Option<crate::identity::ResolvedUserContext> {

    let phone = normalize_channel_phone(&identity.external_id)?;
    let expected_host_user_id = format!("{}:{phone}", identity.channel);
    if context.host_user_id != expected_host_user_id {
        return None;
    }
    let (Some(trust), Ok(assertion)) = (trust, assertion_from_headers(headers)) else {
        return None;
    };
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    trust
        .resolve_authenticated_context(&assertion, context, origin, Utc::now())
        .await
        .ok()
}

fn normalize_channel_phone(value: &str) -> Option<String> {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    if !(7..=15).contains(&digits.len()) {
        return None;
    }
    Some(format!("+{digits}"))
}
