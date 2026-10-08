use super::{AppState, remote_extensions::context};
use crate::host_trust::HostContextRequest;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use vox_connections::setup::{ConnectorSetup, SetupError, SetupRequest, SetupResult};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub host_context: HostContextRequest,
    pub setup: SetupRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteRequest {
    pub host_context: HostContextRequest,
    pub state: String,
    pub code: String,
    pub iss: Option<String>,
}

pub async fn start(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<StartRequest>,
) -> Response {
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (Some(db), Some(apps)) = (&s.db, &s.connected_apps) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    reply(
        ConnectorSetup::new(db.pool().clone())
            .start(&c, apps, r.setup)
            .await,
    )
}

pub async fn complete(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<CompleteRequest>,
) -> Response {
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let (Some(db), Some(apps)) = (&s.db, &s.connected_apps) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    reply(
        ConnectorSetup::new(db.pool().clone())
            .complete(&c, apps, &r.state, &r.code, r.iss.as_deref())
            .await,
    )
}

fn reply(outcome: Result<SetupResult, SetupError>) -> Response {
    let (status, code) = match outcome {
        Ok(result) => return (StatusCode::OK, Json(result)).into_response(),
        Err(SetupError::Package(error)) => return super::packages::error(error),
        Err(SetupError::Account(error)) => return super::connected_apps::error(error),
        Err(SetupError::Invalid) => (StatusCode::BAD_REQUEST, "invalid_setup"),
        Err(SetupError::Unavailable) => (StatusCode::NOT_FOUND, "setup_unavailable"),
        Err(SetupError::ReviewRequired) => (StatusCode::CONFLICT, "setup_needs_review"),
        Err(SetupError::Database(_)) => {
            (StatusCode::SERVICE_UNAVAILABLE, "setup_storage_unavailable")
        }
    };
    (status, Json(serde_json::json!({"error":code}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn setup_endpoints_require_authenticated_host_context() {
        let state = AppState::new(true);
        let context = || {
            serde_json::from_value(
                serde_json::json!({"host_user_id":"untrusted","organization_external_key":null}),
            )
            .unwrap()
        };
        let request = StartRequest {
            host_context: context(),
            setup: SetupRequest {
                external_key: "fixture".into(),
                version: 1,
                digest: "a".repeat(64),
                redirect_uri: "https://host.example/callback".into(),
                consent: None,
            },
        };
        assert_eq!(
            start(State(state.clone()), HeaderMap::new(), Json(request))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            complete(
                State(state),
                HeaderMap::new(),
                Json(CompleteRequest {
                    host_context: context(),
                    state: "state".into(),
                    code: "code".into(),
                    iss: None
                })
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
