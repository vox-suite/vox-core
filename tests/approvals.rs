/**
* Integration tests for approval workflows and authorization.
*/
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    agent_registry::{
        AgentRegistry, ModelConfigurationRequest, RegisterAgentDefinitionRequest,
        SelectAgentRequest,
    },
    approvals::{ApprovalError, ApprovalService, CreateProposalRequest},
    capability_grants::{CapabilityGrantService, CreateGrantRequest},
    connections::{
        AuthorizationState, AuthorizeConnectionRequest, ConnectionService, CredentialCustody,
    },
    db::Db,
    durable_tasks::{DurableTaskService, StartTaskRequest},
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    http::{AppState, router},
    integration_registry::{
        CapabilityDeclaration, CapabilityEffect, IntegrationProtocol, IntegrationRegistry,
        RegisterIntegrationRequest, SetIntegrationEnabledRequest,
    },
};

async fn setup() -> Db {
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    db
}

async fn host(
    db: &Db,
    user: &str,
) -> (
    String,
    vox_core::identity::ResolvedUserContext,
    vox_core::host_trust::RegisteredHostApp,
    HostContextRequest,
) {
    let trust = HostTrustService::new(db.clone());
    let registered = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("approval-http-{}", Uuid::new_v4()),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let request = HostContextRequest {
        host_user_id: user.into(),
        organization_external_key: None,
    };
    let now = Utc::now();
    let assertion = registered
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    let resolved = trust
        .resolve_authenticated_context(&assertion, &request, None, now)
        .await
        .unwrap();
    (
        registered.deployment_external_key.clone(),
        resolved,
        registered,
        request,
    )
}

fn signed_request(
    uri: String,
    body: Vec<u8>,
    assertion: &vox_core::host_trust::HostContextAssertion,
) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header(
            "x-vox-host-credential",
            assertion.credential_id().to_string(),
        )
        .header("x-vox-host-audience", assertion.audience())
        .header(
            "x-vox-host-timestamp",
            assertion.issued_at().timestamp().to_string(),
        )
        .header("x-vox-host-nonce", assertion.nonce().to_string())
        .header("x-vox-host-signature", assertion.signature())
        .header("x-vox-host-secret", assertion.secret())
        .body(Body::from(body))
        .unwrap()
}

async fn context(db: &Db, user: &str) -> (String, vox_core::identity::ResolvedUserContext) {
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("approval-{}", Uuid::new_v4()),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let request = HostContextRequest {
        host_user_id: user.into(),
        organization_external_key: None,
    };
    let now = Utc::now();
    let assertion = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    (
        host.deployment_external_key,
        trust
            .resolve_authenticated_context(&assertion, &request, None, now)
            .await
            .unwrap(),
    )
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn approval_endpoint_rejects_agent_assertion_forgery_and_replay() {
    let db = setup().await;
    let (deployment, owner, registered, host_context) = host(&db, "owner").await;
    let connection_id = prepare(&db, &deployment, &owner).await;
    let now = Utc::now();
    let task = DurableTaskService::new(db.clone())
        .start(
            &owner,
            StartTaskRequest {
                title: "Reserve".into(),
                instruction: "Prepare reservation".into(),
                agent_external_key: Some("planner".into()),
            },
        )
        .await
        .unwrap();
    let proposal = ApprovalService::new(db.clone())
        .propose(
            &owner,
            proposal(
                task.id,
                task.run_id,
                connection_id,
                now + Duration::minutes(10),
            ),
            now,
        )
        .await
        .unwrap();
    let app = router(AppState::with_host_trust(db, "operator-token".into()));
    let body = serde_json::to_vec(
        &serde_json::json!({"host_context":host_context,"details":proposal.details}),
    )
    .unwrap();
    let uri = format!("/v1/action-proposals/{}/approve", proposal.id);
    let agent_asserted = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("authorization", "Bearer operator-token")
        .header("content-type", "application/json")
        .body(Body::from(body.clone()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(agent_asserted).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let assertion = registered
        .credential
        .sign_context_request(&host_context, now, Uuid::new_v4())
        .unwrap();
    let mut forged = signed_request(uri.clone(), body.clone(), &assertion);
    forged
        .headers_mut()
        .insert("x-vox-host-secret", "forged".parse().unwrap());
    assert_eq!(
        app.clone().oneshot(forged).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let valid = registered
        .credential
        .sign_context_request(&host_context, now, Uuid::new_v4())
        .unwrap();
    assert_eq!(
        app.clone()
            .oneshot(signed_request(uri.clone(), body.clone(), &valid))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.oneshot(signed_request(uri, body, &valid))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

async fn prepare(
    db: &Db,
    deployment: &str,
    context: &vox_core::identity::ResolvedUserContext,
) -> Uuid {
    let agents = AgentRegistry::new(db.clone());
    agents
        .register(RegisterAgentDefinitionRequest {
            deployment_external_key: deployment.into(),
            external_key: "planner".into(),
            purpose: "plans writes".into(),
            requested_capability_categories: vec!["calendar.write".into()],
        })
        .await
        .unwrap();
    agents
        .select(SelectAgentRequest {
            deployment_external_key: deployment.into(),
            agent_external_key: "planner".into(),
            model_configuration: ModelConfigurationRequest {
                model_adapter: "test".into(),
                model: "test".into(),
                configuration: serde_json::json!({}),
            },
        })
        .await
        .unwrap();
    let integrations = IntegrationRegistry::new(db.clone());
    integrations
        .register(RegisterIntegrationRequest {
            deployment_external_key: deployment.into(),
            external_key: "calendar".into(),
            protocol: IntegrationProtocol::Direct,
            display_name: "Calendar".into(),
            declaration_version: 1,
            capabilities: vec![CapabilityDeclaration {
                external_key: "write".into(),
                effect: CapabilityEffect::Write,
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
    let connection = ConnectionService::new(db.clone())
        .record(
            context,
            AuthorizeConnectionRequest {
                integration_external_key: "calendar".into(),
                external_account_reference: "account@test".into(),
                account_display_id: None,
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: AuthorizationState::Authorized,
                authorized_capabilities: vec!["calendar.write".into()],
                expires_at: Some(Utc::now() + Duration::hours(1)),
                failure_code: None,
            },
        )
        .await
        .unwrap();
    CapabilityGrantService::new(db.clone())
        .grant(
            context,
            CreateGrantRequest {
                agent_external_key: "planner".into(),
                connection_id: connection.id,
                capability_external_key: "calendar.write".into(),
            },
        )
        .await
        .unwrap();
    connection.id
}

fn proposal(
    span_id: Uuid,
    task_run_id: Uuid,
    connection_id: Uuid,
    expires_at: chrono::DateTime<Utc>,
) -> CreateProposalRequest {
    CreateProposalRequest {
        span_id,
        task_run_id,
        agent_external_key: "planner".into(),
        capability_external_key: "calendar.write".into(),
        details: serde_json::json!({"event":"Dinner","at":"2026-10-01T19:00:00Z","execution":{"connection_id":connection_id}}),
        expires_at,
        replaces_proposal_id: None,
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn proposal_requires_an_effective_grant_for_its_connection() {
    let db = setup().await;
    let (deployment, owner) = context(&db, "proposal-grant-owner").await;
    let connection_id = prepare(&db, &deployment, &owner).await;
    let task = DurableTaskService::new(db.clone())
        .start(
            &owner,
            StartTaskRequest {
                title: "Grant-bound proposal".into(),
                instruction: "Propose a calendar write".into(),
                agent_external_key: Some("planner".into()),
            },
        )
        .await
        .unwrap();
    CapabilityGrantService::new(db.clone())
        .revoke(
            &owner,
            CreateGrantRequest {
                agent_external_key: "planner".into(),
                connection_id,
                capability_external_key: "calendar.write".into(),
            },
        )
        .await
        .unwrap();
    let result = ApprovalService::new(db)
        .propose(
            &owner,
            proposal(
                task.id,
                task.run_id,
                connection_id,
                Utc::now() + Duration::minutes(10),
            ),
            Utc::now(),
        )
        .await;
    assert!(matches!(result, Err(ApprovalError::UnauthorizedCapability)));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn exact_approval_is_context_bound_single_use_and_invalidated_by_change_or_expiry() {
    let db = setup().await;
    let (deployment, owner) = context(&db, "owner").await;
    let connection_id = prepare(&db, &deployment, &owner).await;
    let approvals = ApprovalService::new(db.clone());
    let now = Utc::now();
    let task = DurableTaskService::new(db.clone())
        .start(
            &owner,
            StartTaskRequest {
                title: "Book dinner".into(),
                instruction: "Prepare a dinner reservation".into(),
                agent_external_key: Some("planner".into()),
            },
        )
        .await
        .unwrap();
    let first = approvals
        .propose(
            &owner,
            proposal(
                task.id,
                task.run_id,
                connection_id,
                now + Duration::minutes(10),
            ),
            now,
        )
        .await
        .unwrap();
    let (_, other) = context(&db, "other").await;
    assert!(matches!(
        approvals
            .approve(&other, first.id, first.details.clone(), now)
            .await,
        Err(ApprovalError::NotFound)
    ));
    assert!(matches!(
        approvals
            .approve(
                &owner,
                first.id,
                serde_json::json!({"event":"Dinner","at":"2026-10-01T20:00:00Z"}),
                now
            )
            .await,
        Err(ApprovalError::NotApprovable)
    ));
    let approved = approvals
        .approve(&owner, first.id, first.details.clone(), now)
        .await
        .unwrap();
    let approval_id = approved.approval_id.unwrap();
    let changed = approvals
        .propose(
            &owner,
            CreateProposalRequest {
                replaces_proposal_id: Some(first.id),
                details: serde_json::json!({"event":"Dinner","at":"2026-10-01T20:00:00Z","execution":{"connection_id":connection_id}}),
                ..proposal(task.id, task.run_id, connection_id, now + Duration::minutes(10))
            },
            now,
        )
        .await
        .unwrap();
    assert!(matches!(
        approvals
            .consume(&owner, approval_id, Uuid::new_v4(), now)
            .await,
        Err(ApprovalError::NotApprovable)
    ));
    let changed_approval = approvals
        .approve(&owner, changed.id, changed.details.clone(), now)
        .await
        .unwrap();
    let changed_approval_id = changed_approval.approval_id.unwrap();
    let execution_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO executions (user_id, proposal_id, approval_id, idempotency_key) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(owner.user_id.0)
    .bind(changed.id)
    .bind(changed_approval_id)
    .bind(format!("idem-{}", Uuid::new_v4()))
    .fetch_one(db.pool())
    .await
    .unwrap();
    approvals
        .consume(&owner, changed_approval_id, execution_id, now)
        .await
        .unwrap();
    assert!(matches!(
        approvals
            .consume(&owner, changed_approval_id, execution_id, now)
            .await,
        Err(ApprovalError::Consumed)
    ));
    let expired_task = DurableTaskService::new(db.clone())
        .start(
            &owner,
            StartTaskRequest {
                title: "Book later".into(),
                instruction: "Prepare another reservation".into(),
                agent_external_key: Some("planner".into()),
            },
        )
        .await
        .unwrap();
    let expired = approvals
        .propose(
            &owner,
            proposal(
                expired_task.id,
                expired_task.run_id,
                connection_id,
                now + Duration::minutes(1),
            ),
            now,
        )
        .await
        .unwrap();
    assert!(matches!(
        approvals
            .approve(
                &owner,
                expired.id,
                expired.details.clone(),
                now + Duration::minutes(2)
            )
            .await,
        Err(ApprovalError::Expired)
    ));
}
