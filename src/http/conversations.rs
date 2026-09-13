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

pub async fn respond(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RespondRequest>,
) -> Response {
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match service.respond(request).await {
        Ok(response) => (StatusCode::OK, Json::<RespondResponse>(response)).into_response(),
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ConversationError::Agent(_)) => StatusCode::BAD_GATEWAY.into_response(),
        Err(
            ConversationError::Database(_)
            | ConversationError::IdentityConflict
            | ConversationError::NotFound,
        ) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CompleteConversationRequest>,
) -> Response {
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match service.complete(request).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
