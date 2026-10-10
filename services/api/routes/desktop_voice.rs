use crate::state::ApiState;
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;
use vox_core::{
    conversations::{CompleteConversationRequest, RespondRequest},
    desktop_voice::{DesktopVoiceBroker, VoiceBinding},
    domain::identity::Actor,
    http::auth::authorized,
    identity::ChannelIdentity,
};

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BootstrapInput {
    pub device_id: Option<Uuid>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedeemInput {
    pub ticket: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnInput {
    pub text: String,
    pub turn_id: Option<String>,
    pub revision: Option<u64>,
    pub tts_provider: Option<String>,
    pub initiation_context: Option<String>,
}
#[utoipa::path(post,path="/v1/me/desktop-voice/sessions",tag="desktop",request_body=BootstrapInput,responses((status=200,body=vox_core::desktop_voice::IssuedSession)))]
pub async fn bootstrap(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<BootstrapInput>,
) -> Response {
    match DesktopVoiceBroker::new(state.pool)
        .issue(actor.user_id, input.device_id)
        .await
    {
        Ok(issued) => Json(issued).into_response(),
        Err(sqlx::Error::RowNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
pub async fn redeem(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(input): Json<RedeemInput>,
) -> Response {
    if state.legacy.service_token().is_empty()
        || !authorized(&headers, state.legacy.service_token())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match DesktopVoiceBroker::new(state.pool)
        .redeem(&input.ticket)
        .await
    {
        Ok(Some(binding)) => Json(binding).into_response(),
        Ok(None) => StatusCode::UNAUTHORIZED.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
async fn binding(
    state: &ApiState,
    headers: &HeaderMap,
    id: Uuid,
) -> Result<VoiceBinding, StatusCode> {
    if state.legacy.service_token().is_empty() || !authorized(headers, state.legacy.service_token())
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    DesktopVoiceBroker::new(state.pool.clone())
        .active(id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::UNAUTHORIZED)
}
fn request(binding: &VoiceBinding, input: TurnInput) -> RespondRequest {
    RespondRequest {
        agent_external_key: "general".into(),
        identity: ChannelIdentity {
            channel: "voice".into(),
            external_id: binding.user_id.to_string(),
        },
        external_conversation_id: format!("desktop:{}", binding.session_id),
        text: input.text,
        initiation_context: input.initiation_context,
        turn_id: input.turn_id,
        revision: input.revision,
        tts_provider: input.tts_provider,
        filler: None,
    }
}
pub async fn respond_stream(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<TurnInput>,
) -> Response {
    if input.text.trim().is_empty() || input.text.len() > 32768 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let binding = match binding(&state, &headers, id).await {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let Some(service) = state.legacy.conversations() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let context = match service.resolve_context_for_user(binding.user_id).await {
        Ok(c) => c,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let stream = match service
        .respond_stream(context, request(&binding, input))
        .await
    {
        Ok(s) => s,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let events = stream
        .map(|item| {
            Ok::<_, std::convert::Infallible>(match item {
                Ok(delta) => format!("data: {}\n\n", json!({"delta":delta})),
                Err(_) => "event: error\ndata: {\"error\":\"agent error\"}\n\n".into(),
            })
        })
        .chain(futures_util::stream::once(async {
            Ok("data: [DONE]\n\n".to_owned())
        }));
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(axum::body::Body::from_stream(events))
        .unwrap()
}
pub async fn complete(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Response {
    let binding = match binding(&state, &headers, id).await {
        Ok(b) => b,
        Err(e) => return e.into_response(),
    };
    let broker = DesktopVoiceBroker::new(state.pool.clone());
    if broker.complete(id).await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if let Some(service) = state.legacy.conversations()
        && let Ok(context) = service.resolve_context_for_user(binding.user_id).await
    {
        let _ = service
            .complete(
                context,
                CompleteConversationRequest {
                    agent_external_key: "general".into(),
                    identity: ChannelIdentity {
                        channel: "voice".into(),
                        external_id: binding.user_id.to_string(),
                    },
                    external_conversation_id: format!("desktop:{id}"),
                },
            )
            .await;
    }
    StatusCode::NO_CONTENT.into_response()
}
