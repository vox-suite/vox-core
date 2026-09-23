/**
* Integration tests for capability grant enforcement.
*/
use chrono::{Duration, Utc};
use uuid::Uuid;
use vox_core::{
    agent_registry::{
        AgentRegistry, ModelConfigurationRequest, RegisterAgentDefinitionRequest,
        SelectAgentRequest, SetAgentEnabledRequest,
    },
    capability_grants::{CapabilityGrantError, CapabilityGrantService, CreateGrantRequest},
    connections::{
        AuthorizationState, AuthorizeConnectionRequest, ConnectionService, CredentialCustody,
    },
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    integration_registry::{
        CapabilityDeclaration, CapabilityEffect, IntegrationProtocol, IntegrationRegistry,
        RegisterIntegrationRequest, SetIntegrationEnabledRequest,
    },
};

async fn setup() -> Db {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    db
}

async fn context(db: &Db) -> (String, vox_core::identity::ResolvedUserContext) {
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("grants-{}", Uuid::new_v4()),
            host_app_external_key: "reference-host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let request = HostContextRequest {
        host_user_id: "user".into(),
        organization_external_key: None,
    };
    let now = Utc::now();
    let assertion = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    let context = trust
        .resolve_authenticated_context(&assertion, &request, None, now)
        .await
        .unwrap();
    (host.deployment_external_key, context)
}

async fn agent(registry: &AgentRegistry, deployment: &str, key: &str) {
    registry
        .register(RegisterAgentDefinitionRequest {
            deployment_external_key: deployment.into(),
            external_key: key.into(),
            purpose: format!("{key} assists the user"),
            requested_capability_categories: vec!["calendar.read".into()],
        })
        .await
        .unwrap();
    registry
        .select(SelectAgentRequest {
            deployment_external_key: deployment.into(),
            agent_external_key: key.into(),
            model_configuration: ModelConfigurationRequest {
                model_adapter: "test".into(),
                model: "test-model".into(),
                configuration: serde_json::json!({}),
            },
        })
        .await
        .unwrap();
}

async fn connection(
    db: &Db,
    deployment: &str,
    context: &vox_core::identity::ResolvedUserContext,
) -> uuid::Uuid {
    let integrations = IntegrationRegistry::new(db.clone());
    integrations
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
    integrations
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.into(),
            external_key: "calendar".into(),
            enabled: true,
        })
        .await
        .unwrap();
    ConnectionService::new(db.clone())
        .record(
            context,
            AuthorizeConnectionRequest {
                integration_external_key: "calendar".into(),
                external_account_reference: "account@example.test".into(),
                account_display_id: Some("account@example.test".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: AuthorizationState::Authorized,
                authorized_capabilities: vec!["calendar.read".into()],
                expires_at: Some(Utc::now() + Duration::hours(1)),
                failure_code: None,
            },
        )
        .await
        .unwrap()
        .id
}

fn grant(agent: &str, connection_id: Uuid) -> CreateGrantRequest {
    CreateGrantRequest {
        agent_external_key: agent.into(),
        connection_id,
        capability_external_key: "calendar.read".into(),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn disconnect_revokes_grants_that_reconnection_must_not_restore() {
    let db = setup().await;
    let (deployment, context) = context(&db).await;
    agent(&AgentRegistry::new(db.clone()), &deployment, "planner").await;
    let connection_id = connection(&db, &deployment, &context).await;
    let grants = CapabilityGrantService::new(db.clone());
    grants
        .grant(&context, grant("planner", connection_id))
        .await
        .unwrap();
    assert_eq!(
        grants
            .effective_for_agent(&context, "planner")
            .await
            .unwrap()
            .len(),
        1
    );

    let connections = ConnectionService::new(db.clone());
    connections
        .disconnect(&context, connection_id)
        .await
        .unwrap();
    assert!(
        grants
            .effective_for_agent(&context, "planner")
            .await
            .unwrap()
            .is_empty()
    );

    let reconnected = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: "calendar".into(),
                external_account_reference: "account@example.test".into(),
                account_display_id: Some("account@example.test".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: AuthorizationState::Authorized,
                authorized_capabilities: vec!["calendar.read".into()],
                expires_at: Some(Utc::now() + Duration::hours(1)),
                failure_code: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(reconnected.id, connection_id);
    assert!(
        grants
            .effective_for_agent(&context, "planner")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn grants_are_explicit_context_bound_and_fail_closed_on_revocation() {
    let db = setup().await;
    let (deployment, context) = context(&db).await;
    let agents = AgentRegistry::new(db.clone());
    agent(&agents, &deployment, "planner").await;
    agent(&agents, &deployment, "researcher").await;
    let connection_id = connection(&db, &deployment, &context).await;
    let grants = CapabilityGrantService::new(db.clone());

    assert!(
        grants
            .effective_for_agent(&context, "planner")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        grants
            .effective_for_agent(&context, "researcher")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        grants
            .grant(
                &context,
                CreateGrantRequest {
                    capability_external_key: "calendar.write".into(),
                    ..grant("planner", connection_id)
                },
            )
            .await,
        Err(CapabilityGrantError::Unavailable)
    ));

    grants
        .grant(&context, grant("planner", connection_id))
        .await
        .unwrap();
    grants
        .grant(&context, grant("researcher", connection_id))
        .await
        .unwrap();
    assert_eq!(
        grants
            .effective_for_agent(&context, "planner")
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        grants
            .effective_for_agent(&context, "researcher")
            .await
            .unwrap()
            .len(),
        1
    );

    grants
        .revoke(&context, grant("planner", connection_id))
        .await
        .unwrap();
    assert!(
        grants
            .effective_for_agent(&context, "planner")
            .await
            .unwrap()
            .is_empty()
    );
    grants
        .grant(&context, grant("planner", connection_id))
        .await
        .unwrap();

    agents
        .set_enabled(SetAgentEnabledRequest {
            deployment_external_key: deployment.clone(),
            agent_external_key: "planner".into(),
            enabled: false,
        })
        .await
        .unwrap();
    assert!(
        grants
            .effective_for_agent(&context, "planner")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        grants
            .effective_for_agent(&context, "researcher")
            .await
            .unwrap()
            .len(),
        1,
        "disabling one agent must preserve unrelated grants"
    );

    ConnectionService::new(db.clone())
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: "calendar".into(),
                external_account_reference: "account@example.test".into(),
                account_display_id: Some("account@example.test".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: AuthorizationState::Revoked,
                authorized_capabilities: vec![],
                expires_at: None,
                failure_code: None,
            },
        )
        .await
        .unwrap();
    assert!(
        grants
            .effective_for_agent(&context, "researcher")
            .await
            .unwrap()
            .is_empty()
    );
}
