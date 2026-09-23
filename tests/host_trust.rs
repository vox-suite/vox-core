/**
* Integration tests for host trust validation and device attestation.
*/
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    db::Db,
    host_trust::{
        HostContextAssertion, HostContextRequest, HostTrustError, RegisterHostAppRequest,
        RegisteredHostApp,
    },
    http::{AppState, router},
};

async fn setup() -> Db {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    db
}

fn registration(suffix: Uuid) -> RegisterHostAppRequest {
    RegisterHostAppRequest {
        deployment_external_key: format!("test-deployment-{suffix}"),
        host_app_external_key: "reference-host".into(),
        allowed_origins: vec!["https://host.example.test".into()],
    }
}

fn context_request() -> HostContextRequest {
    HostContextRequest {
        host_user_id: "person-42".into(),
        organization_external_key: None,
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn host_trust_rejects_forgery_expiry_replay_wrong_scope_and_revocation() {
    let db = setup().await;
    let trust = vox_core::host_trust::HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(registration(Uuid::new_v4()))
        .await
        .unwrap();
    let request = context_request();
    let now = Utc::now();

    let first = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    let (first_attempt, replay_attempt) = tokio::join!(
        trust.resolve_authenticated_context(
            &first,
            &request,
            Some("https://host.example.test"),
            now
        ),
        trust.resolve_authenticated_context(
            &first,
            &request,
            Some("https://host.example.test"),
            now
        )
    );
    let (resolved, replay_error) = match (first_attempt, replay_attempt) {
        (Ok(context), Err(error)) | (Err(error), Ok(context)) => (context, error),
        outcome => panic!("one concurrent assertion must succeed and one must fail: {outcome:?}"),
    };
    assert!(matches!(replay_error, HostTrustError::AssertionReplayed));

    let expired = host
        .credential
        .sign_context_request(&request, now - Duration::seconds(301), Uuid::new_v4())
        .unwrap();
    assert!(matches!(
        trust
            .resolve_authenticated_context(&expired, &request, None, now)
            .await,
        Err(HostTrustError::AssertionExpired)
    ));

    let valid = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    let wrong_audience = HostContextAssertion::from_parts(
        valid.credential_id(),
        "vox-host:another-deployment:another-host".into(),
        valid.issued_at(),
        valid.nonce(),
        valid.signature().into(),
        valid.secret().into(),
    );
    assert!(matches!(
        trust
            .resolve_authenticated_context(&wrong_audience, &request, None, now)
            .await,
        Err(HostTrustError::AuthenticationDenied)
    ));
    let tampered_user = HostContextRequest {
        host_user_id: "attacker-selected-user".into(),
        organization_external_key: None,
    };
    assert!(matches!(
        trust
            .resolve_authenticated_context(&valid, &tampered_user, None, now)
            .await,
        Err(HostTrustError::AuthenticationDenied)
    ));
    let forged_secret = HostContextAssertion::from_parts(
        valid.credential_id(),
        valid.audience().into(),
        valid.issued_at(),
        valid.nonce(),
        valid.signature().into(),
        "forged-secret".into(),
    );
    assert!(matches!(
        trust
            .resolve_authenticated_context(&forged_secret, &request, None, now)
            .await,
        Err(HostTrustError::AuthenticationDenied)
    ));

    let wrong_origin = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    assert!(matches!(
        trust
            .resolve_authenticated_context(
                &wrong_origin,
                &request,
                Some("https://forged.example"),
                now
            )
            .await,
        Err(HostTrustError::OriginDenied)
    ));

    let other_host = trust
        .register_host_app(registration(Uuid::new_v4()))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO host_organizations (deployment_id, host_app_id, external_key) \
         VALUES ($1, $2, 'other-host-org')",
    )
    .bind(other_host.deployment_id.0)
    .bind(other_host.host_app_id.0)
    .execute(db.pool())
    .await
    .unwrap();
    let wrong_scope = HostContextRequest {
        host_user_id: request.host_user_id.clone(),
        organization_external_key: Some("other-host-org".into()),
    };
    let wrong_scope_assertion = host
        .credential
        .sign_context_request(&wrong_scope, now, Uuid::new_v4())
        .unwrap();
    assert!(matches!(
        trust
            .resolve_authenticated_context(&wrong_scope_assertion, &wrong_scope, None, now)
            .await,
        Err(HostTrustError::AuthenticationDenied)
    ));

    let replacement = trust.rotate_credential(host.host_app_id).await.unwrap();
    let overlapping_old = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    assert_eq!(
        trust
            .resolve_authenticated_context(&overlapping_old, &request, None, now)
            .await
            .unwrap()
            .id,
        resolved.id,
        "the old credential remains valid during a deliberate rotation overlap"
    );
    let replacement_assertion = replacement
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    let rotated = trust
        .resolve_authenticated_context(&replacement_assertion, &request, None, now)
        .await
        .unwrap();
    assert_eq!(resolved.id, rotated.id, "rotation must not change identity");

    trust
        .revoke_credential(host.credential.credential_id)
        .await
        .unwrap();
    let revoked = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    assert!(matches!(
        trust
            .resolve_authenticated_context(&revoked, &request, None, now)
            .await,
        Err(HostTrustError::AuthenticationDenied)
    ));

    let stored_hash: Vec<u8> =
        sqlx::query_scalar("SELECT secret_hash FROM host_app_credentials WHERE id = $1")
            .bind(replacement.credential_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(stored_hash.len(), 32);
    assert_ne!(stored_hash, replacement.secret.as_bytes());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn host_trust_state_survives_a_new_service_instance() {
    let db = setup().await;
    let first = vox_core::host_trust::HostTrustService::new(db.clone());
    let host = first
        .register_host_app(registration(Uuid::new_v4()))
        .await
        .unwrap();
    let second = vox_core::host_trust::HostTrustService::new(db.clone());
    let request = context_request();
    let now = Utc::now();
    let assertion = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    let resolved = second
        .resolve_authenticated_context(&assertion, &request, None, now)
        .await
        .unwrap();
    assert_eq!(resolved.subject.deployment_id, host.deployment_id);
    assert_eq!(resolved.subject.host_app_id, host.host_app_id);
    assert!(matches!(
        first
            .resolve_authenticated_context(&assertion, &request, None, now)
            .await,
        Err(HostTrustError::AssertionReplayed)
    ));
    first
        .revoke_credential(host.credential.credential_id)
        .await
        .unwrap();
    let after_revocation = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    assert!(matches!(
        second
            .resolve_authenticated_context(&after_revocation, &request, None, now)
            .await,
        Err(HostTrustError::AuthenticationDenied)
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn host_registration_and_context_endpoint_accept_only_signed_host_assertions() {
    let db = setup().await;
    let app = router(AppState::with_host_trust(db, "operator-token".into()));
    let register_body = serde_json::to_vec(&registration(Uuid::new_v4())).unwrap();
    let unauthorized_registration = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/host-apps")
                .header("content-type", "application/json")
                .body(Body::from(register_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized_registration.status(), StatusCode::UNAUTHORIZED);
    let register_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/host-apps")
                .header("authorization", "Bearer operator-token")
                .header("content-type", "application/json")
                .body(Body::from(register_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(register_response.status(), StatusCode::CREATED);
    let registered: RegisteredHostApp = serde_json::from_slice(
        &to_bytes(register_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();

    let request = context_request();
    let assertion = registered
        .credential
        .sign_context_request(&request, Utc::now(), Uuid::new_v4())
        .unwrap();
    let response = app
        .clone()
        .oneshot(signed_http_request(&request, &assertion))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let forged = HostContextAssertion::from_parts(
        assertion.credential_id(),
        assertion.audience().into(),
        assertion.issued_at(),
        Uuid::new_v4(),
        assertion.signature().into(),
        "forged-secret".into(),
    );
    let forged_response = app
        .oneshot(signed_http_request(&request, &forged))
        .await
        .unwrap();
    assert_eq!(forged_response.status(), StatusCode::UNAUTHORIZED);
}

fn signed_http_request(
    request: &HostContextRequest,
    assertion: &HostContextAssertion,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/host/context")
        .header("content-type", "application/json")
        .header("origin", "https://host.example.test")
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
        .body(Body::from(serde_json::to_vec(request).unwrap()))
        .unwrap()
}
