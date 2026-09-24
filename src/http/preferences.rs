use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    host_trust::{HostContextRequest, HostTrustService},
    preferences::{PreferenceError, SetPreferenceRequest},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct SetPreferencePayload {
    pub host_context: HostContextRequest,
    pub preference: SetPreferenceRequest,
}

#[derive(Deserialize)]
pub struct ListPreferencesPayload {
    pub host_context: HostContextRequest,
}

#[derive(Deserialize)]
pub struct EffectivePreferencesPayload {
    pub host_context: HostContextRequest,
    pub categories: Vec<String>,
}

#[derive(Deserialize)]
pub struct DeletePreferencePayload {
    pub host_context: HostContextRequest,
}

pub async fn set(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<SetPreferencePayload>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.preferences.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service
        .set_preference(&context, r.preference, Utc::now())
        .await
    {
        Ok(pref) => (StatusCode::OK, Json(pref)).into_response(),
        Err(PreferenceError::Invalid(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(PreferenceError::ConfirmationRequired) => {
            StatusCode::PRECONDITION_REQUIRED.into_response()
        }
        Err(PreferenceError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn list(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<ListPreferencesPayload>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.preferences.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service.list_preferences(&context).await {
        Ok(prefs) => (StatusCode::OK, Json(prefs)).into_response(),
        Err(PreferenceError::Invalid(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(PreferenceError::ConfirmationRequired) => {
            StatusCode::PRECONDITION_REQUIRED.into_response()
        }
        Err(PreferenceError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn effective(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(agent_key): Path<String>,
    Json(r): Json<EffectivePreferencesPayload>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.preferences.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service
        .effective_for_agent(&context, &agent_key, &r.categories)
        .await
    {
        Ok(prefs) => (StatusCode::OK, Json(prefs)).into_response(),
        Err(PreferenceError::Invalid(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(PreferenceError::ConfirmationRequired) => {
            StatusCode::PRECONDITION_REQUIRED.into_response()
        }
        Err(PreferenceError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn delete_key(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(key): Path<String>,
    Json(r): Json<DeletePreferencePayload>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.preferences.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service.delete_preference(&context, &key).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(PreferenceError::Invalid(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(PreferenceError::ConfirmationRequired) => {
            StatusCode::PRECONDITION_REQUIRED.into_response()
        }
        Err(PreferenceError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
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
