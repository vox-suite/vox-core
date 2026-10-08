use super::{AppState, auth, remote_extensions::context};
use crate::host_trust::HostContextRequest;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;
use vox_connections::{
    packages::{PackageError, PackageRegistry, PublishPackage},
    remote_extensions::RemoteExtensionError,
};

#[derive(Deserialize)]
pub struct ListRequest {
    pub host_context: HostContextRequest,
}
#[derive(Deserialize)]
pub struct InstallRequest {
    pub host_context: HostContextRequest,
    pub external_key: String,
    pub version: i32,
    pub digest: String,
}
#[derive(Deserialize)]
pub struct WithdrawRequest {
    pub deployment_id: Uuid,
    pub external_key: String,
    pub version: i32,
}

pub async fn publish(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<PublishPackage>,
) -> Response {
    if !auth::authorized(&h, &s.service_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(db) = s.db.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    reply(PackageRegistry::new(db.pool().clone()).publish(r).await)
}
pub async fn list(State(s): State<AppState>, h: HeaderMap, Json(r): Json<ListRequest>) -> Response {
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(db) = s.db.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    reply(PackageRegistry::new(db.pool().clone()).list(&c).await)
}
pub async fn install(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<InstallRequest>,
) -> Response {
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(db) = s.db.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    reply(
        PackageRegistry::new(db.pool().clone())
            .install(&c, &r.external_key, r.version, &r.digest)
            .await,
    )
}
pub async fn withdraw(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<WithdrawRequest>,
) -> Response {
    if !auth::authorized(&h, &s.service_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(db) = s.db.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match PackageRegistry::new(db.pool().clone())
        .withdraw(r.deployment_id, &r.external_key, r.version)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error(e),
    }
}
fn reply<T: serde::Serialize>(result: Result<T, PackageError>) -> Response {
    match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => error(e),
    }
}
pub(super) fn error(e: PackageError) -> Response {
    let (status, code) = match e {
        PackageError::Invalid => (StatusCode::BAD_REQUEST, "invalid_package"),
        PackageError::Extension(RemoteExtensionError::Database(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "package_storage_unavailable",
        ),
        PackageError::Extension(RemoteExtensionError::Conflict) => {
            (StatusCode::CONFLICT, "package_conflict")
        }
        PackageError::Extension(RemoteExtensionError::NotFound) => {
            (StatusCode::NOT_FOUND, "package_unavailable")
        }
        PackageError::Extension(_) => (StatusCode::BAD_REQUEST, "invalid_extension"),
        PackageError::Unavailable => (StatusCode::NOT_FOUND, "package_unavailable"),
        PackageError::Conflict => (StatusCode::CONFLICT, "package_conflict"),
        PackageError::Database(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "package_storage_unavailable",
        ),
    };
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn headers(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("authorization", format!("Bearer {token}").parse().unwrap());
        h
    }

    #[tokio::test]
    async fn catalog_mutations_require_operator_auth_and_install_requires_host_trust() {
        let mut state = AppState::new(true);
        state.service_token = Arc::from("operator-secret");
        let publish_request = || PublishPackage {
            metadata: vox_connections::packages::PackageMetadata::oauth(),
            deployment_id: Uuid::new_v4(),
            version: 1,
            manifest: serde_json::from_value(serde_json::json!({
                "external_key":"fixture", "display_name":"Fixture", "protocol":"mcp",
                "endpoint_url":"https://example.com/mcp",
                "operator":{"operator_id":"fixture", "operator_name":"Fixture"},
                "capabilities":[]
            }))
            .unwrap(),
            review: serde_json::json!({"reviewer":"operator"}),
        };
        assert_eq!(
            publish(
                State(state.clone()),
                headers("host-secret"),
                Json(publish_request())
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            publish(
                State(state.clone()),
                headers("operator-secret"),
                Json(publish_request())
            )
            .await
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            withdraw(
                State(state.clone()),
                headers("host-secret"),
                Json(WithdrawRequest {
                    deployment_id: Uuid::new_v4(),
                    external_key: "fixture".into(),
                    version: 1,
                })
            )
            .await
            .status(),
            StatusCode::FORBIDDEN
        );
        let host_context = || {
            serde_json::from_value(
                serde_json::json!({"host_user_id":"untrusted", "organization_external_key":null}),
            )
            .unwrap()
        };
        assert_eq!(
            list(
                State(state.clone()),
                headers("operator-secret"),
                Json(ListRequest {
                    host_context: host_context()
                })
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            install(
                State(state),
                headers("operator-secret"),
                Json(InstallRequest {
                    host_context: host_context(),
                    external_key: "fixture".into(),
                    version: 1,
                    digest: "a".repeat(64),
                })
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
