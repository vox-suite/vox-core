/**
* Integration tests for identity mapping and channel adapters.
*/
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    http::{AppState, router},
    identity_adapters::{
        AuthenticateIdentityRequest, FederatedProof, IdentityAdapterConfiguration,
        IdentityAdapterError, IdentityAdapterService, IdentityProof,
        RecordingRecoveryDelivery, RegisterIdentityAdapterRequest,
        StartPasswordlessRecoveryRequest,
    },
};

async fn setup() -> Db {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    db
}

async fn context(
    trust: &HostTrustService,
    host: &vox_core::host_trust::RegisteredHostApp,
    host_user_id: &str,
) -> vox_core::identity::ResolvedUserContext {
    let request = HostContextRequest {
        host_user_id: host_user_id.into(),
        organization_external_key: None,
    };
    let now = Utc::now();
    let assertion = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .expect("sign host context");
    trust
        .resolve_authenticated_context(&assertion, &request, None, now)
        .await
        .expect("resolve context")
}

fn federated_configuration(key: &SigningKey) -> IdentityAdapterConfiguration {
    IdentityAdapterConfiguration::FederatedEd25519 {
        issuer: "https://issuer.example.test".into(),
        audience: "vox-reference-host".into(),
        public_key: hex::encode(key.verifying_key().to_bytes()),
    }
}

fn federated_request(
    key: &SigningKey,
    external_key: &str,
    subject: &str,
) -> AuthenticateIdentityRequest {
    let now = Utc::now();
    AuthenticateIdentityRequest {
        adapter_external_key: external_key.into(),
        proof: IdentityProof::Federated(
            FederatedProof::sign(
                key,
                "https://issuer.example.test".into(),
                "vox-reference-host".into(),
                subject.into(),
                now,
                now + Duration::minutes(5),
                Uuid::new_v4(),
            )
            .expect("sign federated proof"),
        ),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn adapter_operations_are_unavailable_until_platform_storage_returns() {
    let db = setup().await;
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("identity-adapters-{}", Uuid::new_v4()),
            host_app_external_key: "reference-host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let first_context = context(&trust, &host, "first-host-user").await;
    let second_context = context(&trust, &host, "second-host-user").await;
    let delivery = Arc::new(RecordingRecoveryDelivery::default());
    let service = IdentityAdapterService::new(db.clone(), delivery);
    let signing_key = SigningKey::from_bytes(&[17; 32]);

    assert!(matches!(
        service
            .register_adapter(RegisterIdentityAdapterRequest {
                deployment_external_key: host.deployment_external_key.clone(),
                external_key: "federated-primary".into(),
                configuration: federated_configuration(&signing_key),
            })
            .await,
        Err(IdentityAdapterError::AdapterUnavailable)
    ));

    assert!(matches!(
        service
            .authenticate(
                &first_context,
                federated_request(&signing_key, "federated-primary", "same@example.test"),
                Utc::now(),
            )
            .await,
        Err(IdentityAdapterError::AdapterUnavailable)
    ));

    assert!(matches!(
        service
            .start_passwordless_recovery(
                &second_context,
                StartPasswordlessRecoveryRequest {
                    adapter_external_key: "recovery-email".into(),
                    recovery_handle: "same@example.test".into(),
                },
                Utc::now(),
            )
            .await,
        Err(IdentityAdapterError::AdapterUnavailable)
    ));

    assert!(matches!(
        service
            .link_identities("token-a", "token-b", Utc::now())
            .await,
        Err(IdentityAdapterError::AdapterUnavailable)
    ));
    assert!(matches!(
        service
            .unlink_identities("token-a", "token-b", Utc::now())
            .await,
        Err(IdentityAdapterError::AdapterUnavailable)
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn unavailable_adapter_service_rejects_all_authentication_paths() {
    let db = setup().await;
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("identity-replay-{}", Uuid::new_v4()),
            host_app_external_key: "reference-host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let user_context = context(&trust, &host, "host-user").await;
    let service = IdentityAdapterService::unavailable(db.clone());
    let old_key = SigningKey::from_bytes(&[23; 32]);
    assert!(matches!(
        service
            .register_adapter(RegisterIdentityAdapterRequest {
                deployment_external_key: host.deployment_external_key.clone(),
                external_key: "federated-v1".into(),
                configuration: federated_configuration(&old_key),
            })
            .await,
        Err(IdentityAdapterError::AdapterUnavailable)
    ));
    let replayed = federated_request(&old_key, "federated-v1", "person@example.test");
    assert!(matches!(
        service
            .authenticate(&user_context, replayed.clone(), Utc::now())
            .await,
        Err(IdentityAdapterError::AdapterUnavailable)
    ));
    assert!(matches!(
        service
            .authenticate(&user_context, replayed, Utc::now())
            .await,
        Err(IdentityAdapterError::AdapterUnavailable)
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn public_identity_endpoints_require_operator_or_fresh_host_proofs() {
    let db = setup().await;
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("identity-http-{}", Uuid::new_v4()),
            host_app_external_key: "reference-host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let signing_key = SigningKey::from_bytes(&[31; 32]);
    let app = router(
        AppState::with_host_trust(db.clone(), "operator-token".into())
            .with_identity_adapters(IdentityAdapterService::unavailable(db.clone())),
    );
    let registration = RegisterIdentityAdapterRequest {
        deployment_external_key: host.deployment_external_key.clone(),
        external_key: "federated-http".into(),
        configuration: federated_configuration(&signing_key),
    };
    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/identity-adapters")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&registration).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let registered = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/identity-adapters")
                .header("content-type", "application/json")
                .header("authorization", "Bearer operator-token")
                .body(Body::from(serde_json::to_vec(&registration).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(registered.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let host_context = HostContextRequest {
        host_user_id: "http-user".into(),
        organization_external_key: None,
    };
    let assertion = host
        .credential
        .sign_context_request(&host_context, Utc::now(), Uuid::new_v4())
        .unwrap();
    let payload = serde_json::json!({
        "host_context": host_context,
        "authentication": federated_request(&signing_key, "federated-http", "http@example.test"),
    });
    let authenticated = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/identity/authentications")
                .header("content-type", "application/json")
                .header(
                    "x-vox-host-credential",
                    assertion.credential_id().to_string(),
                )
                .header("x-vox-host-secret", assertion.secret())
                .header("x-vox-host-audience", assertion.audience())
                .header(
                    "x-vox-host-timestamp",
                    assertion.issued_at().timestamp().to_string(),
                )
                .header("x-vox-host-nonce", assertion.nonce().to_string())
                .header("x-vox-host-signature", assertion.signature())
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authenticated.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(authenticated.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body.is_empty() || body.starts_with(b"{"));
}
