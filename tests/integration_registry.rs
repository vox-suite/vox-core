use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::Utc;
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    agent_registry::{
        AgentRegistry, ModelConfigurationRequest, RegisterAgentDefinitionRequest,
        SelectAgentRequest,
    },
    capability_grants::{CapabilityGrantError, CapabilityGrantService, CreateGrantRequest},
    connections::{
        AuthorizationState, AuthorizeConnectionRequest, ConnectionService, CredentialCustody,
    },
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    http::{AppState, router},
    integration_registry::{
        CapabilityDeclaration, CapabilityEffect, IntegrationProtocol, IntegrationRegistry,
        IntegrationRegistryError, RegisterIntegrationRequest, SetIntegrationEnabledRequest,
    },
};
async fn setup() -> Db {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    db
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn signed_context_discovery_is_deployment_scoped_and_agent_filtered() {
    let db = setup().await;
    let trust = HostTrustService::new(db.clone());
    let host_a = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("discovery-a-{}", Uuid::new_v4()),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let host_b = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("discovery-b-{}", Uuid::new_v4()),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let user = HostContextRequest {
        host_user_id: "user-1".into(),
        organization_external_key: None,
    };
    let signed = host_a
        .credential
        .sign_context_request(&user, Utc::now(), Uuid::new_v4())
        .unwrap();
    let context_a = trust
        .resolve_authenticated_context(&signed, &user, None, Utc::now())
        .await
        .unwrap();
    let signed_b = host_b
        .credential
        .sign_context_request(&user, Utc::now(), Uuid::new_v4())
        .unwrap();
    let context_b = trust
        .resolve_authenticated_context(&signed_b, &user, None, Utc::now())
        .await
        .unwrap();

    let registry = IntegrationRegistry::new(db.clone());
    for (deployment, key) in [
        (&host_a.deployment_external_key, "weather"),
        (&host_a.deployment_external_key, "calendar"),
        (&host_b.deployment_external_key, "private"),
    ] {
        let mut declaration = registration(deployment, key, IntegrationProtocol::Direct);
        if key == "calendar" {
            declaration.capabilities[0].regions = vec!["IN".into()];
        }
        registry.register(declaration).await.unwrap();
        registry
            .set_enabled(SetIntegrationEnabledRequest {
                deployment_external_key: deployment.clone(),
                external_key: key.into(),
                enabled: true,
            })
            .await
            .unwrap();
    }
    let agents = AgentRegistry::new(db.clone());
    agents
        .register(RegisterAgentDefinitionRequest {
            deployment_external_key: host_a.deployment_external_key.clone(),
            external_key: "saathi".into(),
            purpose: "Voice assistant".into(),
            requested_capability_categories: vec!["weather.search".into()],
        })
        .await
        .unwrap();
    agents
        .select(SelectAgentRequest {
            deployment_external_key: host_a.deployment_external_key.clone(),
            agent_external_key: "saathi".into(),
            model_configuration: ModelConfigurationRequest {
                model_adapter: "test-model".into(),
                model: "test".into(),
                configuration: serde_json::json!({}),
            },
        })
        .await
        .unwrap();
    let catalog_a = registry
        .discover_for_context(&context_a, None, None)
        .await
        .unwrap();
    assert_eq!(catalog_a.len(), 1);
    assert!(catalog_a.iter().all(|c| c.declaration_is_claim));
    assert_eq!(
        registry
            .discover_for_context(&context_a, None, Some("in"))
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        registry
            .discover_for_context(&context_a, None, Some("US"))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        registry
            .discover_for_context(&context_a, None, Some("anywhere"))
            .await,
        Err(IntegrationRegistryError::Invalid)
    ));
    let catalog_b = registry
        .discover_for_context(&context_b, None, None)
        .await
        .unwrap();
    assert_eq!(catalog_b.len(), 1);
    assert_eq!(catalog_b[0].integration_external_key, "private");
    let filtered = registry
        .discover_for_context(&context_a, Some("saathi"), None)
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].integration_external_key, "weather");
    assert!(matches!(
        registry
            .discover_for_context(&context_b, Some("saathi"), None)
            .await,
        Err(IntegrationRegistryError::NotFound)
    ));

    let app = router(AppState::with_host_trust(db, "operator-token".into()));
    let body = serde_json::json!({
        "host_context": user,
        "agent_external_key": "saathi"
    });
    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/capabilities/discover")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let assertion = host_a
        .credential
        .sign_context_request(&user, Utc::now(), Uuid::new_v4())
        .unwrap();
    let make_request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/capabilities/discover")
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
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let accepted = app.clone().oneshot(make_request()).await.unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);
    let bytes = to_bytes(accepted.into_body(), usize::MAX).await.unwrap();
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result.as_array().unwrap().len(), 1);
    assert_eq!(result[0]["integration_external_key"], "weather");
    let replay = app.oneshot(make_request()).await.unwrap();
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
}

async fn deployment(db: &Db) -> String {
    HostTrustService::new(db.clone())
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("integration-{}", Uuid::new_v4()),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap()
        .deployment_external_key
}

fn registration(
    deployment_external_key: &str,
    key: &str,
    protocol: IntegrationProtocol,
) -> RegisterIntegrationRequest {
    RegisterIntegrationRequest {
        deployment_external_key: deployment_external_key.into(),
        external_key: key.into(),
        protocol,
        display_name: key.into(),
        declaration_version: 1,
        capabilities: vec![CapabilityDeclaration {
            external_key: "search".into(),
            effect: CapabilityEffect::Read,
            access_needs: vec!["oauth".into()],
            data_recipients: vec!["provider".into()],
            regions: vec!["global".into()],
            failure_modes: vec!["expired_access".into()],
            optional_guarantees: serde_json::json!({"freshness":"claimed"}),
        }],
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn discovery_only_returns_enabled_protocol_neutral_claims() {
    let db = setup().await;
    let deployment = deployment(&db).await;
    let registry = IntegrationRegistry::new(db);
    registry
        .register(registration(
            &deployment,
            "mcp-calendar",
            IntegrationProtocol::Mcp,
        ))
        .await
        .unwrap();
    registry
        .register(registration(
            &deployment,
            "direct-weather",
            IntegrationProtocol::Direct,
        ))
        .await
        .unwrap();
    assert!(registry.discover(&deployment).await.unwrap().is_empty());
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: "mcp-calendar".into(),
            enabled: true,
        })
        .await
        .unwrap();
    let discovered = registry.discover(&deployment).await.unwrap();
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].protocol, IntegrationProtocol::Mcp);
    assert!(discovered[0].declaration_is_claim);
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: "direct-weather".into(),
            enabled: true,
        })
        .await
        .unwrap();
    let discovered = registry.discover(&deployment).await.unwrap();
    assert_eq!(discovered.len(), 2);
    assert_eq!(discovered[1].protocol, IntegrationProtocol::Mcp);
    assert_eq!(discovered[0].protocol, IntegrationProtocol::Direct);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn revisions_preserve_history_and_require_explicit_reenablement() {
    let db = setup().await;
    let deployment = deployment(&db).await;
    let registry = IntegrationRegistry::new(db);
    let first = registration(&deployment, "weather", IntegrationProtocol::Direct);
    registry.register(first.clone()).await.unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: "weather".into(),
            enabled: true,
        })
        .await
        .unwrap();
    let mut second = first.clone();
    second.declaration_version = 2;
    second.capabilities[0].data_recipients = vec!["new-operator".into()];
    registry.register(second.clone()).await.unwrap();
    assert!(registry.discover(&deployment).await.unwrap().is_empty());
    let versions = registry.versions(&deployment, "weather").await.unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].version, 1);
    assert_eq!(
        versions[0].declaration["capabilities"][0]["data_recipients"][0],
        "provider"
    );
    assert_eq!(versions[1].version, 2);
    assert_eq!(
        versions[1].declaration["capabilities"][0]["data_recipients"][0],
        "new-operator"
    );
    assert!(matches!(
        registry.register(first).await,
        Err(IntegrationRegistryError::Invalid)
    ));
    registry.register(second.clone()).await.unwrap();
    second.capabilities[0].effect = CapabilityEffect::Write;
    assert!(matches!(
        registry.register(second).await,
        Err(IntegrationRegistryError::Invalid)
    ));
    let mut secret_claim = registration(&deployment, "secret-test", IntegrationProtocol::Direct);
    secret_claim.capabilities[0].optional_guarantees =
        serde_json::json!({"nested": {"client_secret": "must-not-store"}});
    assert!(matches!(
        registry.register(secret_claim).await,
        Err(IntegrationRegistryError::Invalid)
    ));
    assert!(registry.discover(&deployment).await.unwrap().is_empty());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn new_declaration_revokes_old_connection_authority_and_grants() {
    let db = setup().await;
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("revision-{}", Uuid::new_v4()),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let user = HostContextRequest {
        host_user_id: "user-1".into(),
        organization_external_key: None,
    };
    let assertion = host
        .credential
        .sign_context_request(&user, Utc::now(), Uuid::new_v4())
        .unwrap();
    let context = trust
        .resolve_authenticated_context(&assertion, &user, None, Utc::now())
        .await
        .unwrap();
    let registry = IntegrationRegistry::new(db.clone());
    let first = registration(
        &host.deployment_external_key,
        "weather",
        IntegrationProtocol::Direct,
    );
    registry.register(first.clone()).await.unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: host.deployment_external_key.clone(),
            external_key: "weather".into(),
            enabled: true,
        })
        .await
        .unwrap();
    let agents = AgentRegistry::new(db.clone());
    agents
        .register(RegisterAgentDefinitionRequest {
            deployment_external_key: host.deployment_external_key.clone(),
            external_key: "saathi".into(),
            purpose: "Voice assistant".into(),
            requested_capability_categories: vec!["weather.search".into()],
        })
        .await
        .unwrap();
    agents
        .select(SelectAgentRequest {
            deployment_external_key: host.deployment_external_key.clone(),
            agent_external_key: "saathi".into(),
            model_configuration: ModelConfigurationRequest {
                model_adapter: "test-model".into(),
                model: "test".into(),
                configuration: serde_json::json!({}),
            },
        })
        .await
        .unwrap();
    let connection = ConnectionService::new(db.clone())
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: "weather".into(),
                external_account_reference: "provider-user-1".into(),
                account_display_id: None,
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: AuthorizationState::Authorized,
                authorized_capabilities: vec!["weather.search".into()],
                expires_at: None,
                failure_code: None,
            },
        )
        .await
        .unwrap();
    CapabilityGrantService::new(db.clone())
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: "weather.search".into(),
            },
        )
        .await
        .unwrap();
    let mut second = first;
    second.declaration_version = 2;
    second.capabilities[0].data_recipients = vec!["new-operator".into()];
    registry.register(second).await.unwrap();
    let state: String =
        sqlx::query_scalar("SELECT authorization_state FROM external_connections WHERE id=$1")
            .bind(connection.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(state, "expired");
    let grant_state: String =
        sqlx::query_scalar("SELECT state FROM agent_capability_grants WHERE connection_id=$1")
            .bind(connection.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(grant_state, "revoked");
    let denied = CapabilityGrantService::new(db.clone())
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: "weather.search".into(),
            },
        )
        .await;
    assert!(matches!(denied, Err(CapabilityGrantError::Unavailable)));
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: host.deployment_external_key,
            external_key: "weather".into(),
            enabled: true,
        })
        .await
        .unwrap();
    assert!(
        CapabilityGrantService::new(db)
            .effective_for_agent(&context, "saathi")
            .await
            .unwrap()
            .is_empty(),
        "operator re-enablement must not restore old agent authority"
    );
}
