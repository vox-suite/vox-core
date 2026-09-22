/**
* Integration tests for agent registry and capability lookup.
*/
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    agent_registry::{
        AgentRegistry, AgentRegistryError, ModelConfigurationRequest,
        RegisterAgentDefinitionRequest, SelectAgentRequest,
    },
    db::Db,
    host_trust::{HostTrustService, RegisterHostAppRequest},
    http::{AppState, router},
};

async fn setup() -> Db {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    db
}

async fn deployment(db: &Db) -> String {
    let trust = HostTrustService::new(db.clone());
    trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("agent-registry-{}", Uuid::new_v4()),
            host_app_external_key: "reference-host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap()
        .deployment_external_key
}

fn definition(deployment_external_key: &str, external_key: &str) -> RegisterAgentDefinitionRequest {
    RegisterAgentDefinitionRequest {
        deployment_external_key: deployment_external_key.into(),
        external_key: external_key.into(),
        purpose: format!("{external_key} helps the user plan work"),
        requested_capability_categories: vec!["calendar.read".into(), "calendar.write".into()],
    }
}

fn selection(
    deployment_external_key: &str,
    agent_external_key: &str,
    model: &str,
) -> SelectAgentRequest {
    SelectAgentRequest {
        deployment_external_key: deployment_external_key.into(),
        agent_external_key: agent_external_key.into(),
        model_configuration: ModelConfigurationRequest {
            model_adapter: "remote-model".into(),
            model: model.into(),
            configuration: serde_json::json!({"temperature": 0.2}),
        },
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn definitions_and_model_replacement_leave_authority_out_of_the_registry() {
    let db = setup().await;
    let deployment = deployment(&db).await;
    let registry = AgentRegistry::new(db.clone());
    let authority_rows_before: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM identity_links) + (SELECT count(*) FROM login_identities)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    let planner = registry
        .register(definition(&deployment, "planner"))
        .await
        .unwrap();
    let researcher = registry
        .register(definition(&deployment, "researcher"))
        .await
        .unwrap();
    assert_ne!(planner.id, researcher.id);
    assert_eq!(
        planner.requested_capability_categories,
        vec!["calendar.read", "calendar.write"]
    );

    let first = registry
        .select(selection(&deployment, "planner", "model-a"))
        .await
        .unwrap();
    let second = registry
        .select(selection(&deployment, "researcher", "model-b"))
        .await
        .unwrap();
    assert_eq!(first.model_configuration.version, 1);
    assert_eq!(second.model_configuration.version, 1);
    let replacement = registry
        .select(selection(&deployment, "planner", "model-c"))
        .await
        .unwrap();
    assert_eq!(replacement.model_configuration.version, 2);
    assert_eq!(
        replacement.definition, planner,
        "model replacement must not alter the definition"
    );

    let selected = registry.selected_for_deployment(&deployment).await.unwrap();
    assert_eq!(selected.len(), 2, "a deployment can select multiple agents");
    assert_eq!(selected[0].definition.external_key, "planner");
    assert_eq!(selected[0].model_configuration.model, "model-c");
    let authority_rows_after: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM identity_links) + (SELECT count(*) FROM login_identities)",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        authority_rows_after, authority_rows_before,
        "installing or selecting agents must create no identity authority"
    );
    let configurations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent_model_configurations c \
         JOIN agent_definitions d ON d.id = c.agent_definition_id \
         JOIN platform_deployments p ON p.id = d.deployment_id \
         WHERE p.external_key = $1",
    )
    .bind(&deployment)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        configurations, 3,
        "model configuration history is versioned instead of mutating grants or policy"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn registry_rejects_sensitive_model_configuration() {
    let db = setup().await;
    let deployment = deployment(&db).await;
    let registry = AgentRegistry::new(db);
    registry
        .register(definition(&deployment, "planner"))
        .await
        .unwrap();
    let mut request = selection(&deployment, "planner", "model-a");
    request.model_configuration.configuration = serde_json::json!({"api_token": "must-not-store"});
    assert!(matches!(
        registry.select(request).await,
        Err(AgentRegistryError::SensitiveModelConfiguration)
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn operator_endpoints_register_select_and_list_agents() {
    let db = setup().await;
    let deployment = deployment(&db).await;
    let app = router(AppState::with_host_trust(
        db.clone(),
        "operator-token".into(),
    ));
    let definition = definition(&deployment, "planner");
    let denied = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agent-definitions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&definition).unwrap()))
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
                .uri("/v1/agent-definitions")
                .header("content-type", "application/json")
                .header("authorization", "Bearer operator-token")
                .body(Body::from(serde_json::to_vec(&definition).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(registered.status(), StatusCode::CREATED);
    let selected = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agent-selections")
                .header("content-type", "application/json")
                .header("authorization", "Bearer operator-token")
                .body(Body::from(
                    serde_json::to_vec(&selection(&deployment, "planner", "model-a")).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(selected.status(), StatusCode::CREATED);
    let listed = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/deployments/{deployment}/agents"))
                .header("authorization", "Bearer operator-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
}
