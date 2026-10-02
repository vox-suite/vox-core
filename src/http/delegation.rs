//! Signed host consent is the only delegation-permission creation surface.
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    delegation::{DelegationService, PermissionRequest},
    durable_tasks::DurableTaskError,
    host_trust::HostContextRequest,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    host_context: HostContextRequest,
    permission: PermissionRequest,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    host_context: HostContextRequest,
}
async fn resolve(
    s: &AppState,
    h: &HeaderMap,
    r: HostContextRequest,
) -> Option<crate::identity::ResolvedUserContext> {
    let trust = s.host_trust.as_ref()?;
    let assertion = assertion_from_headers(h).ok()?;
    trust
        .resolve_authenticated_context(
            &assertion,
            &r,
            h.get("origin").and_then(|v| v.to_str().ok()),
            chrono::Utc::now(),
        )
        .await
        .ok()
}
fn error(e: DurableTaskError) -> Response {
    match e {
        DurableTaskError::Invalid => StatusCode::BAD_REQUEST,
        DurableTaskError::NotFound => StatusCode::NOT_FOUND,
        DurableTaskError::Conflict | DurableTaskError::BudgetExceeded => StatusCode::CONFLICT,
        DurableTaskError::Database(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
    .into_response()
}
pub async fn create(State(s): State<AppState>, h: HeaderMap, Json(r): Json<Create>) -> Response {
    let Some(c) = resolve(&s, &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(db) = s.db else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match DelegationService::new(db)
        .create_permission(&c, r.permission)
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(json!({"id":id}))).into_response(),
        Err(e) => error(e),
    }
}
pub async fn list(State(s): State<AppState>, h: HeaderMap, Json(r): Json<Context>) -> Response {
    let Some(c) = resolve(&s, &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(db) = s.db else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match DelegationService::new(db).list(&c).await {
        Ok(items) => Json(json!({"permissions":items})).into_response(),
        Err(e) => error(e),
    }
}
pub async fn revoke(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<Context>,
) -> Response {
    let Some(c) = resolve(&s, &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(db) = s.db else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match DelegationService::new(db).revoke(&c, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error(e),
    }
}
pub async fn stop_all(State(s): State<AppState>, h: HeaderMap, Json(r): Json<Context>) -> Response {
    let Some(c) = resolve(&s, &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = s.durable_tasks else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.stop_all(&c).await {
        Ok(count) => Json(json!({"cancelled":count,"undo":false})).into_response(),
        Err(e) => error(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scopes {
    host_context: HostContextRequest,
    requester_agent_key: String,
    specialist_agent_key: String,
}
pub async fn scopes(State(s): State<AppState>, h: HeaderMap, Json(r): Json<Scopes>) -> Response {
    let Some(c) = resolve(&s, &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(db) = s.db else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match DelegationService::new(db)
        .scopes(&c, &r.requester_agent_key, &r.specialist_agent_key)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> HostContextRequest {
        serde_json::from_value(json!({"host_user_id":"untrusted","organization_external_key":null}))
            .unwrap()
    }
    #[tokio::test]
    async fn delegation_surfaces_require_authenticated_host() {
        assert_eq!(
            scopes(
                State(AppState::new(true)),
                HeaderMap::new(),
                Json(Scopes {
                    host_context: context(),
                    requester_agent_key: "personal".into(),
                    specialist_agent_key: "specialist".into()
                })
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            list(
                State(AppState::new(true)),
                HeaderMap::new(),
                Json(Context {
                    host_context: context()
                })
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            stop_all(
                State(AppState::new(true)),
                HeaderMap::new(),
                Json(Context {
                    host_context: context()
                })
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            revoke(
                State(AppState::new(true)),
                HeaderMap::new(),
                Path(Uuid::new_v4()),
                Json(Context {
                    host_context: context()
                })
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
