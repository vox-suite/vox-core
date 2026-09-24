/**
* HTTP endpoints for conversation turns, audio streams, and history.
*/
use super::{AppState, auth};
use crate::conversations::{
    CompleteConversationRequest, RespondRequest, RespondResponse, service::ConversationError,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AuthenticatedRespondRequest {
    #[serde(flatten)]
    pub conversation: RespondRequest,
}

#[derive(Deserialize)]
pub struct AuthenticatedCompleteRequest {
    #[serde(flatten)]
    pub conversation: CompleteConversationRequest,
}

#[derive(Deserialize)]
pub struct AuthenticatedSpeculateRequest {
    #[serde(flatten)]
    pub conversation: crate::conversations::SpeculateRequest,
}

pub async fn respond(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedRespondRequest>,
) -> Response {
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.respond(request.conversation).await {
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
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let started = std::time::Instant::now();
    let turn_id = request.conversation.turn_id.clone();
    match service.respond_stream(request.conversation).await {
        Ok(stream) => {
            let preparation_ms = started.elapsed().as_millis();
            let mut first_text_seen = false;
            let sse_stream = stream.map(move |item| {
                if let Ok(delta) = &item
                    && !first_text_seen
                    && !delta.is_empty()
                    && delta != crate::conversations::speculation::LOOKUP_PENDING
                {
                    first_text_seen = true;
                    tracing::info!(
                        turn_id = ?turn_id,
                        preparation_ms,
                        first_text_ms = started.elapsed().as_millis(),
                        "CORE_STREAM_FIRST_TEXT"
                    );
                }
                match item {
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
                }
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
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.complete(request.conversation).await {
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
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.speculate(request.conversation).await {
        Ok(status) => Json(serde_json::json!({"status":status})).into_response(),
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
