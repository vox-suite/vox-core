/**
* HTTP endpoints for conversation turns, audio streams, and history.
*/
use super::{AppState, context::context};
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
    pub host_context: crate::host_trust::HostContextRequest,
    #[serde(flatten)]
    pub conversation: RespondRequest,
}

#[derive(Deserialize)]
pub struct AuthenticatedCompleteRequest {
    pub host_context: crate::host_trust::HostContextRequest,
    #[serde(flatten)]
    pub conversation: CompleteConversationRequest,
}

pub async fn respond(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedRespondRequest>,
) -> Response {
    let Some(context) = context(state.host_trust.as_deref(), &headers, request.host_context).await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.respond(context, request.conversation).await {
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
    let started = std::time::Instant::now();
    let Some(context) = context(state.host_trust.as_deref(), &headers, request.host_context).await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let context_ms = started.elapsed().as_millis() as u64;
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let turn_id = request.conversation.turn_id.clone();
    let conversation_id = request.conversation.external_conversation_id.clone();
    let channel = request.conversation.identity.channel.clone();
    let prompt_chars = request.conversation.text.len();
    let service_started = std::time::Instant::now();
    match service.respond_stream(context, request.conversation).await {
        Ok(stream) => {
            let service_ms = service_started.elapsed().as_millis() as u64;
            let preparation_ms = started.elapsed().as_millis() as u64;
            let mut first_text_ms: Option<u64> = None;
            let deltas = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let chars = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let (deltas_done, chars_done) = (deltas.clone(), chars.clone());
            let first_turn_id = turn_id.clone();
            let first_conversation_id = conversation_id.clone();
            let sse_stream = stream.map(move |item| {
                if let Ok(delta) = &item
                    && !delta.is_empty()
                {
                    deltas.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    chars.fetch_add(delta.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    if first_text_ms.is_none() {
                        let first = started.elapsed().as_millis() as u64;
                        first_text_ms = Some(first);
                        tracing::info!(
                            turn_id = ?first_turn_id,
                            conversation_id = %first_conversation_id,
                            context_ms,
                            service_prepare_ms = service_ms,
                            preparation_ms,
                            first_text_ms = first,
                            "CORE_STREAM_FIRST_TEXT"
                        );
                    }
                }
                match item {
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
                tracing::info!(
                    turn_id = ?turn_id,
                    conversation_id = %conversation_id,
                    channel = %channel,
                    prompt_chars,
                    reply_deltas = deltas_done.load(std::sync::atomic::Ordering::Relaxed),
                    reply_chars = chars_done.load(std::sync::atomic::Ordering::Relaxed),
                    context_ms,
                    service_prepare_ms = service_ms,
                    stream_total_ms = started.elapsed().as_millis() as u64,
                    "CORE_STREAM_DONE"
                );
                Ok::<_, std::convert::Infallible>("data: [DONE]\n\n".to_string())
            });
            let full_stream = sse_stream.chain(done_stream);

            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .header("connection", "keep-alive")
                .header("x-vox-prepare-ms", preparation_ms.to_string())
                .header("x-vox-context-ms", context_ms.to_string())
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
    let Some(context) = context(state.host_trust.as_deref(), &headers, request.host_context).await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.complete(context, request.conversation).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
