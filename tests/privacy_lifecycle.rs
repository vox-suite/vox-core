/**
 * Integration and lifecycle tests for E43 (vox-core#29):
 * - Retention policy declaration
 * - Task history deletion with canonical disclosure and audit preservation
 * - Portable export excluding credentials, active approvals, and reusable authority
 * - Fresh authorization requirement on imported connections (FR-PRT-006)
 * - Historical action evidence remaining interpretable after integration removal
 * - Cross-context privacy isolation
 */
use chrono::Utc;
use uuid::Uuid;
use vox_core::{
    db::Db,
    durable_tasks::{DurableTaskService, StartTaskRequest},
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    preferences::{PreferenceService, SetPreferenceRequest},
    privacy::{
        CANONICAL_DELETION_DISCLOSURE, ExportedConfig, ExportedConnectionDeclaration,
        ExportedPreferences, ExportedUserPreference, PortableExportBundle, PrivacyService,
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
            deployment_external_key: format!("privacy-test-{}", Uuid::new_v4()),
            host_app_external_key: "privacy-host".into(),
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

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn task_history_deletion_purges_tasks_and_returns_canonical_disclosure() {
    let db = setup().await;
    let (_, owner, _, _) = host(&db, "privacy-user-1").await;
    let task_service = DurableTaskService::new(db.clone());

    // 1. Create durable tasks
    task_service
        .start(
            &owner,
            StartTaskRequest {
                title: "Book hotel in Paris".into(),
                instruction: "Expedia lodging booking".into(),
                agent_external_key: None,
            },
        )
        .await
        .unwrap();

    let privacy_service = PrivacyService::new(db.clone(), None);

    // 2. Execute deletion
    let delete_result = privacy_service
        .delete_task_history(&owner, true)
        .await
        .unwrap();

    assert!(delete_result.deleted_spans_count >= 1);
    assert_eq!(delete_result.disclosure, CANONICAL_DELETION_DISCLOSURE);
    assert!(delete_result.disclosure.contains("365 days"));
    assert!(delete_result.disclosure.contains("30 days"));
    assert!(
        delete_result
            .disclosure
            .contains("does not cancel or refund")
    );

    // 3. Deletion is idempotent
    let idempotent_result = privacy_service
        .delete_task_history(&owner, true)
        .await
        .unwrap();
    assert_eq!(idempotent_result.deleted_spans_count, 0);
    assert_eq!(idempotent_result.disclosure, CANONICAL_DELETION_DISCLOSURE);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn portable_export_excludes_secrets_and_active_approvals() {
    let db = setup().await;
    let (_, owner, _, _) = host(&db, "privacy-user-2").await;

    // Set a preference
    let pref_service = PreferenceService::new(db.clone());
    pref_service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "travel".into(),
                preference_key: "seat".into(),
                value: serde_json::json!("window"),
                is_sensitive: Some(false),
                confirmed: None,
            },
            Utc::now(),
        )
        .await
        .unwrap();

    let privacy_service = PrivacyService::new(db.clone(), None);

    // Request export
    let export_bundle = privacy_service
        .generate_portable_export(
            &owner,
            &["config".into(), "preferences".into(), "spans".into()],
            Utc::now(),
        )
        .await
        .unwrap();

    assert_eq!(export_bundle.schema_version, "1.0");
    assert!(export_bundle.preferences.is_some());
    let prefs = export_bundle.preferences.as_ref().unwrap();
    assert_eq!(prefs.preferences.len(), 1);
    assert_eq!(prefs.preferences[0].preference_key, "seat");

    // Canary scan confirms zero prohibited secrets or active authority
    let bundle_value = serde_json::to_value(&export_bundle).unwrap();
    assert!(PrivacyService::scan_for_prohibited_content(&bundle_value).is_ok());

    // Fetch export via get_export
    let retrieved = privacy_service
        .get_export(&owner, export_bundle.export_id)
        .await
        .unwrap();
    assert_eq!(retrieved.export_id, export_bundle.export_id);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn imported_connections_require_fresh_authorization() {
    let db = setup().await;
    let (_, owner, _, _) = host(&db, "privacy-user-3").await;
    let privacy_service = PrivacyService::new(db.clone(), None);

    let bundle = PortableExportBundle {
        export_id: Uuid::new_v4(),
        schema_version: "1.0".into(),
        generated_at: Utc::now(),
        categories: vec!["config".into(), "preferences".into()],
        disclosure: CANONICAL_DELETION_DISCLOSURE.into(),
        config: Some(ExportedConfig {
            agents: vec![],
            integrations: vec![],
            connections: vec![ExportedConnectionDeclaration {
                integration_key: "uber".into(),
                account_display_id: Some("rider@example.com".into()),
                credential_custody: "platform_held".into(),
                requires_renewed_authorization: true,
            }],
        }),
        preferences: Some(ExportedPreferences {
            preferences: vec![ExportedUserPreference {
                category: "units".into(),
                preference_key: "distance".into(),
                value: serde_json::json!("kilometers"),
                is_sensitive: false,
                confirmed_at: None,
                authority_disclaimer: "Advisory context only".into(),
            }],
            capability_grants: vec![],
            disclaimer: "Advisory context only".into(),
        }),
        spans: None,
    };

    let import_result = privacy_service
        .import_portable_data(&owner, &bundle)
        .await
        .unwrap();

    assert_eq!(import_result.imported_preferences_count, 1);
    assert!(
        import_result
            .status
            .contains("require fresh user authorization")
    );

    // Verify preference was saved
    let pref_service = PreferenceService::new(db.clone());
    let prefs = pref_service.list_preferences(&owner).await.unwrap();
    assert!(prefs.iter().any(|p| p.preference_key == "distance"));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cross_context_isolation_prevents_unauthorized_deletion_and_export() {
    let db = setup().await;
    let (_, owner, _, _) = host(&db, "privacy-owner").await;
    let (_, stranger, _, _) = host(&db, "privacy-stranger").await;

    let task_service = DurableTaskService::new(db.clone());
    task_service
        .start(
            &owner,
            StartTaskRequest {
                title: "Owner task".into(),
                instruction: "Do work".into(),
                agent_external_key: None,
            },
        )
        .await
        .unwrap();

    let privacy_service = PrivacyService::new(db.clone(), None);

    // Stranger deletes their task history - does not affect owner's tasks
    let stranger_delete = privacy_service
        .delete_task_history(&stranger, true)
        .await
        .unwrap();
    assert_eq!(stranger_delete.deleted_spans_count, 0);

    // Owner creates an export
    let owner_export = privacy_service
        .generate_portable_export(&owner, &["spans".into()], Utc::now())
        .await
        .unwrap();

    // Stranger attempts to download owner's export
    let stranger_fetch = privacy_service
        .get_export(&stranger, owner_export.export_id)
        .await;
    assert!(stranger_fetch.is_err());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn historical_action_evidence_remains_interpretable_after_integration_removed() {
    let db = setup().await;
    let (deployment_key, owner, _, _) = host(&db, "hist-user").await;
    let now = Utc::now();

    let registry = vox_core::integration_registry::IntegrationRegistry::new(db.clone());
    registry
        .register(vox_core::integration_registry::RegisterIntegrationRequest {
            deployment_external_key: deployment_key.clone(),
            external_key: "ephemeral_svc".into(),
            protocol: vox_core::integration_registry::IntegrationProtocol::Direct,
            display_name: "Ephemeral Service".into(),
            declaration_version: 1,
            capabilities: vec![vox_core::integration_registry::CapabilityDeclaration {
                external_key: "action".into(),
                effect: vox_core::integration_registry::CapabilityEffect::Write,
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
        .set_enabled(
            vox_core::integration_registry::SetIntegrationEnabledRequest {
                deployment_external_key: deployment_key.clone(),
                external_key: "ephemeral_svc".into(),
                enabled: true,
            },
        )
        .await
        .unwrap();

    let agents = vox_core::agent_registry::AgentRegistry::new(db.clone());
    agents
        .register(vox_core::agent_registry::RegisterAgentDefinitionRequest {
            deployment_external_key: deployment_key.clone(),
            external_key: "bot".into(),
            purpose: "actions".into(),
            requested_capability_categories: vec!["ephemeral_svc.action".into()],
        })
        .await
        .unwrap();
    agents
        .select(vox_core::agent_registry::SelectAgentRequest {
            deployment_external_key: deployment_key.clone(),
            agent_external_key: "bot".into(),
            model_configuration: vox_core::agent_registry::ModelConfigurationRequest {
                model_adapter: "test".into(),
                model: "model-e".into(),
                configuration: serde_json::json!({}),
            },
        })
        .await
        .unwrap();

    let connections = vox_core::connections::ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &owner,
            vox_core::connections::AuthorizeConnectionRequest {
                integration_external_key: "ephemeral_svc".into(),
                external_account_reference: "acct_eph_1".into(),
                account_display_id: None,
                credential_custody: vox_core::connections::CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec!["ephemeral_svc.action".into()],
                expires_at: Some(now + chrono::Duration::hours(1)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    vox_core::capability_grants::CapabilityGrantService::new(db.clone())
        .grant(
            &owner,
            vox_core::capability_grants::CreateGrantRequest {
                agent_external_key: "bot".into(),
                connection_id: connection.id,
                capability_external_key: "ephemeral_svc.action".into(),
            },
        )
        .await
        .unwrap();

    let task = DurableTaskService::new(db.clone())
        .start(
            &owner,
            StartTaskRequest {
                title: "Run ephemeral action".into(),
                instruction: "Execute".into(),
                agent_external_key: Some("bot".into()),
            },
        )
        .await
        .unwrap();

    let proposals = vox_core::approvals::ApprovalService::new(db.clone());
    let proposal = proposals
        .propose(
            &owner,
            vox_core::approvals::CreateProposalRequest {
                span_id: task.id,
                task_run_id: task.run_id,
                agent_external_key: "bot".into(),
                capability_external_key: "ephemeral_svc.action".into(),
                details: serde_json::json!({
                    "execution": vox_core::execution_policy::ExecutionIdentity {
                        provider_external_key: "ephemeral_svc".into(),
                        model_identifier: "model-e".into(),
                        account_reference: "acct_eph_1".into(),
                        connection_id: connection.id,
                        price_amount_minor: 100,
                        price_currency: "USD".into(),
                    }
                }),
                expires_at: now + chrono::Duration::minutes(10),
                replaces_proposal_id: None,
            },
            now,
        )
        .await
        .unwrap();

    let approval = proposals
        .approve(&owner, proposal.id, proposal.details, now)
        .await
        .unwrap();

    let coordinator = vox_core::execution::ExecutionCoordinator::new(db.clone());
    let execution = coordinator
        .start(
            &owner,
            vox_core::execution::StartExecutionRequest {
                approval_id: approval.approval_id.unwrap(),
                idempotency_key: format!("hist-exec-{}", Uuid::new_v4()),
            },
            now,
        )
        .await
        .unwrap();

    coordinator
        .record_outcome(
            &owner,
            execution.id,
            vox_core::execution::AdapterOutcome::Succeeded {
                provider_reference: "prov-ref-hist-1".into(),
                evidence: serde_json::json!({
                    "receipt_number": "rcpt-hist-123",
                    "status": "confirmed"
                }),
            },
            now,
        )
        .await
        .unwrap();

    registry
        .set_enabled(
            vox_core::integration_registry::SetIntegrationEnabledRequest {
                deployment_external_key: deployment_key,
                external_key: "ephemeral_svc".into(),
                enabled: false,
            },
        )
        .await
        .unwrap();

    let privacy = PrivacyService::new(db.clone(), None);
    let export = privacy
        .generate_portable_export(&owner, &["spans".into()], now)
        .await
        .unwrap();

    let tasks_export = export.spans.expect("spans must be included");
    let recorded_exec = tasks_export
        .executions
        .iter()
        .find(|e| e.id == execution.id)
        .expect("historical execution must be found in export");

    assert_eq!(recorded_exec.state, "succeeded");
    assert_eq!(
        recorded_exec.provider_reference.as_deref(),
        Some("prov-ref-hist-1")
    );
    assert_eq!(
        recorded_exec.confirmation_evidence.as_ref().unwrap()["receipt_number"],
        "rcpt-hist-123"
    );
}
