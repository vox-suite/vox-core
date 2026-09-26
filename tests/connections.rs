/**
* Integration tests for external connection persistence.
*/
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    connections::{
        AuthorizationState, AuthorizeConnectionRequest, ConnectionError, ConnectionService,
        CredentialCustody, InitiateConnectionRequest, VerifyConnectionCallbackRequest,
    },
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    http::{AppState, router},
    integration_registry::{
        CapabilityDeclaration, CapabilityEffect, IntegrationProtocol, IntegrationRegistry,
        RegisterIntegrationRequest, SetIntegrationEnabledRequest,
    },
};

#[tokio::test]
async fn host_cannot_claim_provider_authorization_or_platform_credential_custody() {
    let app = router(AppState::new(false));
    for (state, custody) in [
        ("authorized", "external_operator"),
        ("pending", "platform_held"),
    ] {
        let body = serde_json::json!({
            "host_context": {"host_user_id": "user"},
            "authorization": {
                "integration_external_key": "calendar",
                "external_account_reference": "example",
                "credential_custody": custody,
                "authorization_state": state,
                "authorized_capabilities": ["read"],
                "expires_at": null,
                "failure_code": null
            }
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/connections/authorize")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

async fn setup() -> Db {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    db
}

async fn create_context(db: &Db) -> (String, vox_core::identity::ResolvedUserContext) {
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("connections-{}", Uuid::new_v4()),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let req = HostContextRequest {
        host_user_id: "user".into(),
        organization_external_key: None,
    };
    let now = Utc::now();
    let assertion = host
        .credential
        .sign_context_request(&req, now, Uuid::new_v4())
        .unwrap();
    let context = trust
        .resolve_authenticated_context(&assertion, &req, None, now)
        .await
        .unwrap();
    (host.deployment_external_key, context)
}

async fn enable(db: &Db, deployment: &str) {
    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(RegisterIntegrationRequest {
            deployment_external_key: deployment.into(),
            external_key: "calendar".into(),
            protocol: IntegrationProtocol::Direct,
            display_name: "Calendar".into(),
            declaration_version: 1,
            capabilities: vec![CapabilityDeclaration {
                external_key: "read".into(),
                effect: CapabilityEffect::Read,
                access_needs: vec![],
                data_recipients: vec![],
                regions: vec![],
                failure_modes: vec![],
                optional_guarantees: serde_json::json!({}),
            }],
        })
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.into(),
            external_key: "calendar".into(),
            enabled: true,
        })
        .await
        .unwrap()
}

fn request(state: AuthorizationState) -> AuthorizeConnectionRequest {
    AuthorizeConnectionRequest {
        integration_external_key: "calendar".into(),
        external_account_reference: "account@example.test".into(),
        account_display_id: Some("account@example.test".into()),
        credential_custody: CredentialCustody::ExternalOperator,
        authorization_state: state,
        authorized_capabilities: vec!["read".into()],
        expires_at: None,
        failure_code: None,
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn connection_is_context_bound_and_reports_truthful_lifecycle() {
    let db = setup().await;
    let (deployment, context) = create_context(&db).await;
    enable(&db, &deployment).await;
    let service = ConnectionService::new(db.clone());
    let mut authorized = request(AuthorizationState::Authorized);
    authorized.expires_at = Some(Utc::now() + Duration::hours(1));
    let created = service.record(&context, authorized).await.unwrap();
    assert_eq!(created.user_context_id, context.id);
    assert_eq!(
        created.credential_custody,
        CredentialCustody::ExternalOperator
    );
    assert_eq!(
        created.account_display_id,
        Some("account@example.test".into())
    );
    let mut expired = request(AuthorizationState::Expired);
    expired.authorized_capabilities = vec![];
    let updated = service.record(&context, expired).await.unwrap();
    assert_eq!(updated.id, created.id);
    assert_eq!(updated.authorization_state, AuthorizationState::Expired);
    let hash_length: i32 = sqlx::query_scalar(
        "SELECT octet_length(external_account_hash) FROM external_connections WHERE id=$1",
    )
    .bind(created.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(hash_length, 32, "only the account hash is persisted");
    let listed = service.list(&context).await.unwrap();
    assert!(listed.iter().any(|connection| connection.id == created.id));

    let (_, other_context) = create_context(&db).await;
    assert!(service.list(&other_context).await.unwrap().is_empty());
    assert!(matches!(
        service.disconnect(&other_context, created.id).await,
        Err(ConnectionError::NotFound)
    ));

    let revoked = service.disconnect(&context, created.id).await.unwrap();
    assert_eq!(revoked.authorization_state, AuthorizationState::Revoked);
    assert!(revoked.authorized_capabilities.is_empty());
    assert_eq!(
        service
            .disconnect(&context, created.id)
            .await
            .unwrap()
            .authorization_state,
        AuthorizationState::Revoked
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn unverified_authorization_cannot_create_a_connection() {
    let db = setup().await;
    let (deployment, context) = create_context(&db).await;
    enable(&db, &deployment).await;
    let service = ConnectionService::new(db.clone());

    let initiation = service
        .initiate(
            &context,
            InitiateConnectionRequest {
                integration_external_key: "calendar".into(),
                credential_custody: CredentialCustody::ExternalOperator,
                requested_capabilities: vec!["read".into()],
                redirect_uri: Some("https://app.voxagent.in/auth/callback".into()),
            },
        )
        .await;
    assert!(matches!(
        initiation,
        Err(ConnectionError::AuthorizationUnavailable)
    ));

    let forged_callback = service
        .verify_callback(
            &context,
            VerifyConnectionCallbackRequest {
                session_id: Uuid::new_v4(),
                state_token: "host-supplied-state".into(),
                provider_code: "unverified-code".into(),
                external_account_reference: "victim-account".into(),
                account_display_id: "victim@example.com".into(),
            },
        )
        .await;
    assert!(matches!(
        forged_callback,
        Err(ConnectionError::AuthorizationUnavailable)
    ));
    assert!(service.list(&context).await.unwrap().is_empty());
}
