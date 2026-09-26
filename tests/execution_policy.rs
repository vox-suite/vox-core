/**
* Integration tests for execution policy rules and matching.
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
        AuthorizationState, AuthorizeConnectionRequest, Connection, ConnectionService,
        CredentialCustody,
    },
    db::Db,
    durable_tasks::{DurableTaskService, StartTaskRequest},
    execution_policy::{
        ExecutionIdentity, ExecutionPolicyError, ExecutionPolicyService, ExecutionRequest,
        OperationalQuotaRequest, SpendingPolicyRequest,
    },
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
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

async fn context(db: &Db) -> (String, vox_core::identity::ResolvedUserContext) {
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("policy-{}", Uuid::new_v4()),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let request = HostContextRequest {
        host_user_id: "owner".into(),
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

async fn prepare(
    db: &Db,
    deployment: &str,
    context: &vox_core::identity::ResolvedUserContext,
) -> Connection {
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
                model: "model-a".into(),
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
                external_account_reference: "account-a".into(),
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
    connection
}

async fn approved_request(
    db: &Db,
    context: &vox_core::identity::ResolvedUserContext,
    connection: Connection,
    now: chrono::DateTime<Utc>,
) -> (Uuid, ExecutionIdentity) {
    let task = DurableTaskService::new(db.clone())
        .start(
            context,
            StartTaskRequest {
                title: "Reserve".into(),
                instruction: "Prepare reservation".into(),
                agent_external_key: Some("planner".into()),
            },
        )
        .await
        .unwrap();
    let execution = ExecutionIdentity {
        provider_external_key: "calendar".into(),
        model_identifier: "model-a".into(),
        account_reference: "account-a".into(),
        connection_id: connection.id,
        price_amount_minor: 1_000,
        price_currency: "USD".into(),
    };
    let proposal = ApprovalService::new(db.clone())
        .propose(
            context,
            CreateProposalRequest {
                span_id: task.id,
                task_run_id: task.run_id,
                agent_external_key: "planner".into(),
                capability_external_key: "calendar.write".into(),
                details: serde_json::json!({"reservation":"Dinner","execution":execution}),
                expires_at: now + Duration::minutes(10),
                replaces_proposal_id: None,
            },
            now,
        )
        .await
        .unwrap();
    let approval = ApprovalService::new(db.clone())
        .approve(context, proposal.id, proposal.details, now)
        .await
        .unwrap();
    (approval.approval_id.unwrap(), execution)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn approved_work_still_requires_exact_price_identity_and_available_quota() {
    let db = setup().await;
    let (deployment, owner) = context(&db).await;
    let connection = prepare(&db, &deployment, &owner).await;
    let now = Utc::now();
    let policies = ExecutionPolicyService::new(db.clone());
    policies
        .set_spending_policy(
            &owner,
            SpendingPolicyRequest {
                capability_external_key: "calendar.write".into(),
                provider_external_key: Some("calendar".into()),
                currency: "USD".into(),
                max_amount_minor: 1_000,
            },
        )
        .await
        .unwrap();
    policies
        .set_operational_quota(
            &owner,
            OperationalQuotaRequest {
                provider_external_key: "calendar".into(),
                model_identifier: "model-a".into(),
                account_reference: "account-a".into(),
                connection_id: connection.id,
                max_attempts: 1,
            },
        )
        .await
        .unwrap();
    let (approval_id, execution) = approved_request(&db, &owner, connection.clone(), now).await;
    for changed_execution in [
        ExecutionIdentity {
            price_amount_minor: 1_001,
            ..execution.clone()
        },
        ExecutionIdentity {
            price_amount_minor: 999,
            ..execution.clone()
        },
        ExecutionIdentity {
            provider_external_key: "other-provider".into(),
            ..execution.clone()
        },
        ExecutionIdentity {
            model_identifier: "model-b".into(),
            ..execution.clone()
        },
        ExecutionIdentity {
            account_reference: "account-b".into(),
            ..execution.clone()
        },
        ExecutionIdentity {
            connection_id: Uuid::new_v4(),
            ..execution.clone()
        },
    ] {
        assert!(matches!(
            policies
                .evaluate(
                    &owner,
                    ExecutionRequest {
                        approval_id,
                        attempt_id: Uuid::new_v4(),
                        execution: changed_execution
                    },
                    now
                )
                .await,
            Err(ExecutionPolicyError::FreshProposalRequired)
        ));
    }
    let decision = policies
        .evaluate(
            &owner,
            ExecutionRequest {
                approval_id,
                attempt_id: Uuid::new_v4(),
                execution: execution.clone(),
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(decision.policy_snapshot["spending"][0]["version"], 1);
    // Policy evaluation reserves the exact quota slot. An arbitrary execution ID
    // cannot consume an approval because the database requires a real execution.
    let (second_approval, second_execution) = approved_request(&db, &owner, connection, now).await;
    assert!(matches!(
        policies
            .evaluate(
                &owner,
                ExecutionRequest {
                    approval_id: second_approval,
                    attempt_id: Uuid::new_v4(),
                    execution: second_execution
                },
                now
            )
            .await,
        Err(ExecutionPolicyError::QuotaExhausted)
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn concurrent_exact_attempts_reserve_only_one_quota_slot() {
    let db = setup().await;
    let (deployment, owner) = context(&db).await;
    let connection = prepare(&db, &deployment, &owner).await;
    let now = Utc::now();
    let policies = ExecutionPolicyService::new(db.clone());
    policies
        .set_operational_quota(
            &owner,
            OperationalQuotaRequest {
                provider_external_key: "calendar".into(),
                model_identifier: "model-a".into(),
                account_reference: "account-a".into(),
                connection_id: connection.id,
                max_attempts: 1,
            },
        )
        .await
        .unwrap();
    let (first_approval, first_execution) =
        approved_request(&db, &owner, connection.clone(), now).await;
    let (second_approval, second_execution) = approved_request(&db, &owner, connection, now).await;
    let first = policies.evaluate(
        &owner,
        ExecutionRequest {
            approval_id: first_approval,
            attempt_id: Uuid::new_v4(),
            execution: first_execution,
        },
        now,
    );
    let second = policies.evaluate(
        &owner,
        ExecutionRequest {
            approval_id: second_approval,
            attempt_id: Uuid::new_v4(),
            execution: second_execution,
        },
        now,
    );
    let (first, second) = tokio::join!(first, second);
    assert_eq!(
        [first.is_ok(), second.is_ok()]
            .into_iter()
            .filter(|result| *result)
            .count(),
        1,
    );
    assert!(matches!(
        first.err().or_else(|| second.err()),
        Some(ExecutionPolicyError::QuotaExhausted)
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn spending_limit_blocks_an_approved_exact_execution_without_becoming_authority() {
    let db = setup().await;
    let (deployment, owner) = context(&db).await;
    let connection = prepare(&db, &deployment, &owner).await;
    let now = Utc::now();
    let policies = ExecutionPolicyService::new(db.clone());
    policies
        .set_spending_policy(
            &owner,
            SpendingPolicyRequest {
                capability_external_key: "calendar.write".into(),
                provider_external_key: None,
                currency: "USD".into(),
                max_amount_minor: 999,
            },
        )
        .await
        .unwrap();
    let (approval_id, execution) = approved_request(&db, &owner, connection, now).await;
    assert!(matches!(
        policies
            .evaluate(
                &owner,
                ExecutionRequest {
                    approval_id,
                    attempt_id: Uuid::new_v4(),
                    execution: execution.clone()
                },
                now
            )
            .await,
        Err(ExecutionPolicyError::SpendingPolicyExceeded)
    ));
    assert!(matches!(
        policies
            .evaluate(
                &owner,
                ExecutionRequest {
                    approval_id: Uuid::new_v4(),
                    attempt_id: Uuid::new_v4(),
                    execution
                },
                now
            )
            .await,
        Err(ExecutionPolicyError::ApprovalRequired)
    ));
    assert!(
        ApprovalService::new(db)
            .consume(&owner, approval_id, Uuid::new_v4(), now)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn transactional_rollback_releases_quota_reservation() {
    let db = setup().await;
    let (deployment, owner) = context(&db).await;
    let connection = prepare(&db, &deployment, &owner).await;
    let now = Utc::now();
    let policies = ExecutionPolicyService::new(db.clone());
    policies
        .set_operational_quota(
            &owner,
            OperationalQuotaRequest {
                provider_external_key: "calendar".into(),
                model_identifier: "model-a".into(),
                account_reference: "account-a".into(),
                connection_id: connection.id,
                max_attempts: 1,
            },
        )
        .await
        .unwrap();
    let (approval_id, execution) = approved_request(&db, &owner, connection, now).await;

    let mut tx = db.pool().begin().await.unwrap();
    let decision = policies
        .evaluate_in_transaction(
            &owner,
            ExecutionRequest {
                approval_id,
                attempt_id: Uuid::new_v4(),
                execution: execution.clone(),
            },
            now,
            &mut tx,
        )
        .await;
    assert!(decision.is_ok());
    tx.rollback().await.unwrap();

    let retry_decision = policies
        .evaluate(
            &owner,
            ExecutionRequest {
                approval_id,
                attempt_id: Uuid::new_v4(),
                execution,
            },
            now,
        )
        .await;
    assert!(retry_decision.is_ok());
}
