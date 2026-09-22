#![cfg(feature = "sandbox")]
/**
* Integration tests for sandbox rollback and transaction isolation.
*/
use chrono::{Duration, Utc};
use uuid::Uuid;
use vox_core::{
    agent_registry::{
        AgentRegistry, ModelConfigurationRequest, RegisterAgentDefinitionRequest,
        SelectAgentRequest,
    },
    approvals::{ApprovalService, CreateProposalRequest},
    capability_grants::{CapabilityGrantService, CreateGrantRequest},
    connections::{
        AuthorizationState, AuthorizeConnectionRequest, ConnectionService, CredentialCustody,
    },
    db::Db,
    durable_tasks::{DurableTaskService, StartTaskRequest},
    execution::{ExecutionCoordinator, StartExecutionRequest},
    execution_policy::{
        ExecutionIdentity, ExecutionPolicyError, ExecutionPolicyService, ExecutionRequest,
    },
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    integration_registry::{
        CapabilityDeclaration, CapabilityEffect, IntegrationProtocol, IntegrationRegistry,
        RegisterIntegrationRequest, SetIntegrationEnabledRequest,
    },
    sandbox::{Scenario, TransactionalSandbox},
};

async fn setup() -> Db {
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap();
    db.migrate().await.unwrap();
    db
}

async fn approved_execution(db: &Db) -> (vox_core::identity::ResolvedUserContext, Uuid) {
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("sandbox-{}", Uuid::new_v4()),
            host_app_external_key: "sandbox-host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let request = HostContextRequest {
        host_user_id: "synthetic-owner".into(),
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

    let agents = AgentRegistry::new(db.clone());
    agents
        .register(RegisterAgentDefinitionRequest {
            deployment_external_key: host.deployment_external_key.clone(),
            external_key: "sandbox-agent".into(),
            purpose: "sandbox acceptance".into(),
            requested_capability_categories: vec!["sandbox.write".into()],
        })
        .await
        .unwrap();
    agents
        .select(SelectAgentRequest {
            deployment_external_key: host.deployment_external_key.clone(),
            agent_external_key: "sandbox-agent".into(),
            model_configuration: ModelConfigurationRequest {
                model_adapter: "synthetic".into(),
                model: "sandbox-model".into(),
                configuration: serde_json::json!({}),
            },
        })
        .await
        .unwrap();

    let integrations = IntegrationRegistry::new(db.clone());
    integrations
        .register(RegisterIntegrationRequest {
            deployment_external_key: host.deployment_external_key.clone(),
            external_key: "sandbox".into(),
            protocol: IntegrationProtocol::Direct,
            display_name: "Transactional Sandbox (test only)".into(),
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
            deployment_external_key: host.deployment_external_key.clone(),
            external_key: "sandbox".into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connection = ConnectionService::new(db.clone())
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: "sandbox".into(),
                external_account_reference: "synthetic-account".into(),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: AuthorizationState::Authorized,
                authorized_capabilities: vec!["sandbox.write".into()],
                expires_at: Some(now + Duration::hours(1)),
                failure_code: None,
            },
        )
        .await
        .unwrap();
    CapabilityGrantService::new(db.clone())
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "sandbox-agent".into(),
                connection_id: connection.id,
                capability_external_key: "sandbox.write".into(),
            },
        )
        .await
        .unwrap();

    let task = DurableTaskService::new(db.clone())
        .start(
            &context,
            StartTaskRequest {
                title: "Synthetic execution".into(),
                instruction: "Exercise authority boundary".into(),
                agent_external_key: Some("sandbox-agent".into()),
            },
        )
        .await
        .unwrap();
    let identity = ExecutionIdentity {
        provider_external_key: "sandbox".into(),
        model_identifier: "sandbox-model".into(),
        account_reference: "synthetic-account".into(),
        connection_id: connection.id,
        price_amount_minor: 0,
        price_currency: "USD".into(),
    };
    let proposal = ApprovalService::new(db.clone())
        .propose(
            &context,
            CreateProposalRequest {
                task_id: task.id,
                task_run_id: task.run_id,
                agent_external_key: "sandbox-agent".into(),
                capability_external_key: "sandbox.write".into(),
                details: serde_json::json!({"execution": identity}),
                expires_at: now + Duration::minutes(5),
                replaces_proposal_id: None,
            },
            now,
        )
        .await
        .unwrap();
    let approval = ApprovalService::new(db.clone())
        .approve(&context, proposal.id, proposal.details, now)
        .await
        .unwrap();
    (context, approval.approval_id.unwrap())
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn sandbox_reconciliation_recovers_unknown_without_a_second_dispatch() {
    let db = setup().await;
    let (context, approval_id) = approved_execution(&db).await;
    let coordinator = ExecutionCoordinator::new(db);
    let sandbox = TransactionalSandbox::seeded("restart-key", Scenario::Unknown);
    let execution = coordinator
        .start(
            &context,
            StartExecutionRequest {
                approval_id,
                idempotency_key: "restart-key".into(),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    let duplicate = coordinator
        .start(
            &context,
            StartExecutionRequest {
                approval_id,
                idempotency_key: "restart-key".into(),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(duplicate.id, execution.id);
    assert!(
        coordinator
            .reconcile(&context, execution.id, &sandbox, Utc::now())
            .await
            .is_err()
    );
    assert_eq!(
        coordinator
            .dispatch(&context, execution.id, &sandbox, Utc::now())
            .await
            .unwrap()
            .state,
        "unknown"
    );
    assert_eq!(sandbox.dispatch_count("restart-key"), 1);

    let restarted = TransactionalSandbox::from_snapshot(sandbox.snapshot());
    restarted.script("restart-key", Scenario::ReconcileSuccess);
    let reconciled = coordinator
        .reconcile(&context, execution.id, &restarted, Utc::now())
        .await
        .unwrap();
    assert_eq!(reconciled.state, "succeeded");
    assert_eq!(
        reconciled.confirmation_evidence.unwrap()["kind"],
        "reconciled"
    );
    assert_eq!(restarted.dispatch_count("restart-key"), 1);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn sandbox_cancellation_keeps_confirmation_evidence() {
    let db = setup().await;
    let (context, approval_id) = approved_execution(&db).await;
    let coordinator = ExecutionCoordinator::new(db);
    let sandbox = TransactionalSandbox::seeded("cancel-key", Scenario::Cancellation);
    let execution = coordinator
        .start(
            &context,
            StartExecutionRequest {
                approval_id,
                idempotency_key: "cancel-key".into(),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    let outcome = coordinator
        .dispatch(&context, execution.id, &sandbox, Utc::now())
        .await
        .unwrap();
    assert_eq!(outcome.state, "cancelled");
    assert_eq!(
        outcome.confirmation_evidence.unwrap()["kind"],
        "cancellation"
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn changed_price_is_rejected_before_the_sandbox_is_dispatched() {
    let db = setup().await;
    let (context, approval_id) = approved_execution(&db).await;
    let details: serde_json::Value = sqlx::query_scalar(
        "SELECT p.details FROM action_approvals a JOIN action_proposals p ON p.id=a.proposal_id WHERE a.id=$1",
    )
    .bind(approval_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let execution: ExecutionIdentity =
        serde_json::from_value(details["execution"].clone()).unwrap();
    let changed = ExecutionIdentity {
        price_amount_minor: execution.price_amount_minor + 1,
        ..execution
    };
    assert!(matches!(
        ExecutionPolicyService::new(db)
            .evaluate(
                &context,
                ExecutionRequest {
                    approval_id,
                    attempt_id: Uuid::new_v4(),
                    execution: changed,
                },
                Utc::now(),
            )
            .await,
        Err(ExecutionPolicyError::FreshProposalRequired)
    ));
}
