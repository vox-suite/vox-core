use uuid::Uuid;
use vox_core::{
    db::Db,
    host_trust::{HostTrustService, RegisterHostAppRequest},
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
