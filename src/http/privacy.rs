use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    host_trust::{HostContextRequest, HostTrustService},
    privacy::{PortableExportBundle, PortableExportResponse, PrivacyError},
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

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub struct DeleteHistoryPayload {
    pub host_context: HostContextRequest,
    #[serde(default = "default_true")]
    pub delete_conversations: bool,
}

#[derive(Deserialize)]
pub struct PortableExportPayload {
    pub host_context: HostContextRequest,
    pub categories: Vec<String>,
}

#[derive(Deserialize)]
pub struct PortableImportPayload {
    pub host_context: HostContextRequest,
    pub bundle: PortableExportBundle,
}

#[derive(Deserialize)]
pub struct AuthenticatedRequestPayload {
    pub host_context: HostContextRequest,
}

/// POST /v1/privacy/delete-history
pub async fn delete_history(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<DeleteHistoryPayload>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.privacy.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    match service
        .delete_task_history(&context, r.delete_conversations)
        .await
    {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(PrivacyError::Invalid(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(PrivacyError::Unauthorized) => StatusCode::UNAUTHORIZED.into_response(),
        Err(PrivacyError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// POST /v1/privacy/portable-export
pub async fn portable_export(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<PortableExportPayload>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.privacy.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    let now = Utc::now();
    match service
        .generate_portable_export(&context, &r.categories, now)
        .await
    {
        Ok(bundle) => {
            let resp = PortableExportResponse {
                export_id: bundle.export_id,
                download_url: format!("/v1/privacy/exports/{}/download", bundle.export_id),
                categories: bundle.categories,
                generated_at: bundle.generated_at,
                disclosure: bundle.disclosure,
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(PrivacyError::Invalid(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(PrivacyError::ProhibitedData(_)) => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
        Err(PrivacyError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// POST /v1/privacy/exports/{id}/download
pub async fn download_export(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(export_id): Path<Uuid>,
    Json(r): Json<AuthenticatedRequestPayload>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.privacy.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    match service.get_export(&context, export_id).await {
        Ok(bundle) => (StatusCode::OK, Json(bundle)).into_response(),
        Err(PrivacyError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(PrivacyError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// POST /v1/privacy/portable-import
pub async fn portable_import(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<PortableImportPayload>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.privacy.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    match service.import_portable_data(&context, &r.bundle).await {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(PrivacyError::Invalid(_)) => StatusCode::BAD_REQUEST.into_response(),
        Err(PrivacyError::ProhibitedData(_)) => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
        Err(PrivacyError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// GET /v1/privacy/retention-policy
pub async fn get_retention_policy(State(s): State<AppState>) -> Response {
    let Some(service) = s.privacy.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    (StatusCode::OK, Json(service.retention_policy().clone())).into_response()
}

/// POST /v1/privacy/retention/prune
pub async fn prune_retention(State(s): State<AppState>, headers: HeaderMap) -> Response {
    if !crate::http::auth::authorized(&headers, &s.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(service) = s.privacy.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.apply_retention_prune(Utc::now()).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(PrivacyError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// POST /v1/privacy/executions/{id}/evidence
pub async fn get_action_evidence(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(execution_id): Path<Uuid>,
    Json(r): Json<AuthenticatedRequestPayload>,
) -> Response {
    let (Some(service), Some(context)) = (
        s.privacy.as_ref(),
        context(s.host_trust.as_deref(), &h, r.host_context).await,
    ) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };

    match service
        .get_historical_action_evidence(&context, execution_id)
        .await
    {
        Ok(evidence) => (StatusCode::OK, Json(evidence)).into_response(),
        Err(PrivacyError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(PrivacyError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
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
