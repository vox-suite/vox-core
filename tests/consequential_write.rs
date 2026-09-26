/**
 * Integration tests for selected consequential write integration (Expedia Rapid Lodging) (E34).
 */
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    agent_registry::{
        AgentRegistry, ModelConfigurationRequest, RegisterAgentDefinitionRequest,
        SelectAgentRequest,
    },
    approvals::ApprovalService,
    capability_grants::{CapabilityGrantService, CreateGrantRequest},
    connections::{AuthorizeConnectionRequest, ConnectionService, CredentialCustody},
    db::Db,
    durable_tasks::{DurableTaskService, StartTaskRequest},
    execution::ExecutionCoordinator,
    execution_policy::ExecutionIdentity,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    integration_registry::{IntegrationRegistry, SetIntegrationEnabledRequest},
    providers::{
        EXPEDIA_CAPABILITY_LODGING_BOOK, EXPEDIA_CAPABILITY_LODGING_BOOK_SHORT,
        EXPEDIA_CAPABILITY_LODGING_MANAGE, EXPEDIA_CAPABILITY_LODGING_MANAGE_SHORT,
        EXPEDIA_CAPABILITY_LODGING_SEARCH_SHORT, EXPEDIA_INTEGRATION_KEY, ExpediaBookingOutcome,
        ExpediaLodgingError, ExpediaLodgingProposalDetails, ExpediaLodgingService,
        MockExpediaProviderClient,
    },
};

async fn setup() -> Db {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    db
}

async fn create_context(
    db: &Db,
    user_id: &str,
) -> (
    String,
    vox_core::identity::ResolvedUserContext,
    vox_core::host_trust::RegisteredHostApp,
    HostContextRequest,
) {
    let trust = HostTrustService::new(db.clone());
    let deployment_external_key = format!("expedia-dep-{}", Uuid::new_v4());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: deployment_external_key.clone(),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();

    // Register and select default test agent 'saathi' for this deployment
    let agents = AgentRegistry::new(db.clone());
    agents
        .register(RegisterAgentDefinitionRequest {
            deployment_external_key: deployment_external_key.clone(),
            external_key: "saathi".into(),
            purpose: "saathi travel assistant".into(),
            requested_capability_categories: vec![
                EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
                EXPEDIA_CAPABILITY_LODGING_MANAGE.into(),
            ],
        })
        .await
        .unwrap();
    agents
        .select(SelectAgentRequest {
            deployment_external_key: deployment_external_key.clone(),
            agent_external_key: "saathi".into(),
            model_configuration: ModelConfigurationRequest {
                model_adapter: "test".into(),
                model: "test-model".into(),
                configuration: json!({}),
            },
        })
        .await
        .unwrap();

    let req = HostContextRequest {
        host_user_id: user_id.into(),
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
    (deployment_external_key, context, host, req)
}

fn sample_proposal_details(connection_id: Uuid) -> ExpediaLodgingProposalDetails {
    ExpediaLodgingProposalDetails {
        property_id: "prop_grand_hotel_99".into(),
        room_type_id: "room_deluxe_king".into(),
        rate_plan_id: "rate_refundable_breakfast".into(),
        checkin_date: "2026-11-10".into(),
        checkout_date: "2026-11-15".into(),
        guest_count: 2,
        primary_guest_name: "Alice Traveler".into(),
        primary_guest_email: "alice@example.com".into(),
        total_price_amount_minor: 75_000, // $750.00
        price_currency: "USD".into(),
        execution: ExecutionIdentity {
            provider_external_key: EXPEDIA_INTEGRATION_KEY.into(),
            model_identifier: "rapid_v3".into(),
            account_reference: "expedia_partner_account_123".into(),
            connection_id,
            price_amount_minor: 75_000,
            price_currency: "USD".into(),
        },
    }
}

#[tokio::test]
async fn integration_declaration_matches_approved_feasibility_finding() {
    let decl = ExpediaLodgingService::integration_declaration("dep-test");
    assert_eq!(decl.external_key, EXPEDIA_INTEGRATION_KEY);
    assert_eq!(decl.capabilities.len(), 3);

    // 1. Search capability: L1 catalog read
    let search = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == EXPEDIA_CAPABILITY_LODGING_SEARCH_SHORT)
        .expect("lodging_search must be declared");
    assert_eq!(
        search.effect,
        vox_core::integration_registry::CapabilityEffect::Read
    );
    assert_eq!(
        search.optional_guarantees["capability_level"],
        "L1_catalog_read"
    );

    // 2. Book capability: L3 consequential write
    let book = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == EXPEDIA_CAPABILITY_LODGING_BOOK_SHORT)
        .expect("lodging_book must be declared");
    assert_eq!(
        book.effect,
        vox_core::integration_registry::CapabilityEffect::Write
    );
    assert_eq!(
        book.optional_guarantees["capability_level"],
        "L3_consequential_write"
    );
    assert_eq!(book.optional_guarantees["idempotency_supported"], true);
    assert_eq!(book.optional_guarantees["reconciliation_supported"], true);

    // 3. Manage capability: L3 consequential write
    let manage = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == EXPEDIA_CAPABILITY_LODGING_MANAGE_SHORT)
        .expect("lodging_manage must be declared");
    assert_eq!(
        manage.effect,
        vox_core::integration_registry::CapabilityEffect::Write
    );
    assert_eq!(
        manage.optional_guarantees["capability_level"],
        "L3_consequential_write"
    );
    assert_eq!(manage.optional_guarantees["cancellation_supported"], true);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn exact_proposal_and_single_use_approval_lifecycle() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-lifecycle").await;

    // Register & enable Expedia integration
    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(ExpediaLodgingService::integration_declaration(&deployment))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: EXPEDIA_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    // Record user connection
    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: EXPEDIA_INTEGRATION_KEY.into(),
                external_account_reference: "partner_user_expedia".into(),
                account_display_id: Some("alice@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![
                    EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
                    EXPEDIA_CAPABILITY_LODGING_MANAGE.into(),
                ],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    // Grant capability to agent
    let grants = CapabilityGrantService::new(db.clone());
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
            },
        )
        .await
        .unwrap();

    let approvals = ApprovalService::new(db.clone());
    let execution = ExecutionCoordinator::new(db.clone());
    let mock_client = Arc::new(MockExpediaProviderClient::new());
    let write_service = ExpediaLodgingService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        approvals.clone(),
        execution.clone(),
        mock_client.clone(),
    );

    // Start durable task
    let tasks = DurableTaskService::new(db.clone());
    let task = tasks
        .start(
            &context,
            StartTaskRequest {
                title: "Book vacation hotel".into(),
                instruction: "Book 5 nights at Grand Hotel".into(),
                agent_external_key: Some("saathi".into()),
            },
        )
        .await
        .unwrap();

    let details = sample_proposal_details(connection.id);
    let now = Utc::now();
    let expires_at = now + Duration::hours(2);

    // 1. Propose booking
    let proposal = write_service
        .propose_booking(
            &context,
            task.id,
            task.run_id,
            "saathi",
            connection.id,
            details.clone(),
            expires_at,
        )
        .await
        .unwrap();

    // 2. Attempting to approve modified details fails closed
    let mut tampered_details = proposal.details.clone();
    tampered_details["total_price_amount_minor"] = json!(70_000); // Attempting to alter price
    let tamper_err = approvals
        .approve(&context, proposal.id, tampered_details, now)
        .await
        .unwrap_err();
    match tamper_err {
        vox_core::approvals::ApprovalError::NotApprovable => {}
        other => panic!("Expected NotApprovable on tampered details, got {other:?}"),
    }

    // 3. Approving exact details succeeds and generates an approval_id
    let approved_proposal = approvals
        .approve(&context, proposal.id, proposal.details.clone(), now)
        .await
        .unwrap();
    let approval_id = approved_proposal
        .approval_id
        .expect("approval_id must exist");

    // 4. Executing booking with valid approval succeeds
    let idempotency_key = format!("affil-ref-{}", Uuid::new_v4());
    let outcome = write_service
        .execute_booking(&context, approval_id, &idempotency_key, now)
        .await
        .unwrap();

    match outcome {
        ExpediaBookingOutcome::Succeeded {
            itinerary_id,
            booking_status,
            total_price_amount_minor,
            ..
        } => {
            assert!(!itinerary_id.is_empty());
            assert_eq!(booking_status, "booked");
            assert_eq!(total_price_amount_minor, 75_000);
        }
        other => panic!("Expected Succeeded outcome, got {other:?}"),
    }

    // 4b. Idempotent duplicate delivery with identical idempotency key returns original Succeeded outcome
    let duplicate_outcome = write_service
        .execute_booking(&context, approval_id, &idempotency_key, now)
        .await
        .unwrap();
    match duplicate_outcome {
        ExpediaBookingOutcome::Succeeded {
            itinerary_id,
            booking_status,
            total_price_amount_minor,
            ..
        } => {
            assert!(!itinerary_id.is_empty());
            assert_eq!(booking_status, "booked");
            assert_eq!(total_price_amount_minor, 75_000);
        }
        other => panic!("Expected Succeeded outcome on duplicate delivery, got {other:?}"),
    }

    // 5. Attempting to re-execute with the same consumed approval fails closed
    let second_key = format!("affil-ref-{}", Uuid::new_v4());
    let replay_err = write_service
        .execute_booking(&context, approval_id, &second_key, now)
        .await
        .unwrap_err();

    match replay_err {
        ExpediaLodgingError::NotApproved => {}
        other => panic!("Expected NotApproved on consumed approval replay, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn distinct_provider_authentication_flow() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-3ds").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(ExpediaLodgingService::integration_declaration(&deployment))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: EXPEDIA_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: EXPEDIA_INTEGRATION_KEY.into(),
                external_account_reference: "partner_user_3ds".into(),
                account_display_id: Some("alice@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![EXPEDIA_CAPABILITY_LODGING_BOOK.into()],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    let grants = CapabilityGrantService::new(db.clone());
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
            },
        )
        .await
        .unwrap();

    let approvals = ApprovalService::new(db.clone());
    let execution = ExecutionCoordinator::new(db.clone());
    let mock_client = Arc::new(MockExpediaProviderClient::new());
    // Configure provider to trigger 3DS / SCA challenge
    mock_client
        .require_3ds
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let write_service = ExpediaLodgingService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        approvals.clone(),
        execution.clone(),
        mock_client.clone(),
    );

    let tasks = DurableTaskService::new(db.clone());
    let task = tasks
        .start(
            &context,
            StartTaskRequest {
                title: "Book resort requiring 3DS".into(),
                instruction: "Book lodging".into(),
                agent_external_key: Some("saathi".into()),
            },
        )
        .await
        .unwrap();

    let details = sample_proposal_details(connection.id);
    let now = Utc::now();
    let proposal = write_service
        .propose_booking(
            &context,
            task.id,
            task.run_id,
            "saathi",
            connection.id,
            details,
            now + Duration::hours(1),
        )
        .await
        .unwrap();

    let approved = approvals
        .approve(&context, proposal.id, proposal.details, now)
        .await
        .unwrap();

    let idempotency_key = format!("affil-3ds-{}", Uuid::new_v4());
    let outcome = write_service
        .execute_booking(
            &context,
            approved.approval_id.unwrap(),
            &idempotency_key,
            now,
        )
        .await
        .unwrap();

    // Verifies provider authentication is cleanly distinguished from platform approval
    match outcome {
        ExpediaBookingOutcome::AwaitingProviderAuthentication {
            challenge_url,
            challenge_token,
            affiliate_reference_id,
        } => {
            assert!(challenge_url.contains("3ds-challenge"));
            assert!(!challenge_token.is_empty());
            assert_eq!(affiliate_reference_id, idempotency_key);
        }
        other => panic!("Expected AwaitingProviderAuthentication, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn authoritative_cancellation_and_refund_accounting() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-cancel").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(ExpediaLodgingService::integration_declaration(&deployment))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: EXPEDIA_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: EXPEDIA_INTEGRATION_KEY.into(),
                external_account_reference: "partner_user_cancel".into(),
                account_display_id: Some("alice@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![
                    EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
                    EXPEDIA_CAPABILITY_LODGING_MANAGE.into(),
                ],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    let grants = CapabilityGrantService::new(db.clone());
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
            },
        )
        .await
        .unwrap();
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: EXPEDIA_CAPABILITY_LODGING_MANAGE.into(),
            },
        )
        .await
        .unwrap();

    let approvals = ApprovalService::new(db.clone());
    let execution = ExecutionCoordinator::new(db.clone());
    let mock_client = Arc::new(MockExpediaProviderClient::new());
    let write_service = ExpediaLodgingService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        approvals.clone(),
        execution.clone(),
        mock_client.clone(),
    );

    let tasks = DurableTaskService::new(db.clone());
    let task = tasks
        .start(
            &context,
            StartTaskRequest {
                title: "Book hotel for cancellation test".into(),
                instruction: "Book lodging".into(),
                agent_external_key: Some("saathi".into()),
            },
        )
        .await
        .unwrap();

    let details = sample_proposal_details(connection.id);
    let now = Utc::now();
    let proposal = write_service
        .propose_booking(
            &context,
            task.id,
            task.run_id,
            "saathi",
            connection.id,
            details,
            now + Duration::hours(1),
        )
        .await
        .unwrap();

    let approved = approvals
        .approve(&context, proposal.id, proposal.details, now)
        .await
        .unwrap();

    let idempotency_key = format!("affil-cancel-{}", Uuid::new_v4());
    let booking = write_service
        .execute_booking(
            &context,
            approved.approval_id.unwrap(),
            &idempotency_key,
            now,
        )
        .await
        .unwrap();

    let itinerary_id = match booking {
        ExpediaBookingOutcome::Succeeded { itinerary_id, .. } => itinerary_id,
        other => panic!("Expected Succeeded, got {other:?}"),
    };

    // A manage grant alone cannot target a booking not recorded for this
    // user and connection.
    let unrelated = write_service
        .cancel_booking(
            &context,
            "saathi",
            connection.id,
            "another-users-itinerary",
            "Unauthorized cancellation attempt",
        )
        .await;
    assert!(matches!(
        unrelated,
        Err(ExpediaLodgingError::ProposalNotFound)
    ));

    // 1. Authoritative cancellation
    let cancel_res = write_service
        .cancel_booking(
            &context,
            "saathi",
            connection.id,
            &itinerary_id,
            "Traveler requested cancellation",
        )
        .await
        .unwrap();

    assert_eq!(cancel_res.itinerary_id, itinerary_id);
    assert_eq!(cancel_res.penalty_amount_minor, 5_000); // $50 penalty
    assert_eq!(cancel_res.refund_amount_minor, 70_000); // $700 refund
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn timeout_and_unknown_outcome_reconciliation() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-timeout").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(ExpediaLodgingService::integration_declaration(&deployment))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: EXPEDIA_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: EXPEDIA_INTEGRATION_KEY.into(),
                external_account_reference: "partner_user_timeout".into(),
                account_display_id: Some("alice@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![EXPEDIA_CAPABILITY_LODGING_BOOK.into()],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    let grants = CapabilityGrantService::new(db.clone());
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
            },
        )
        .await
        .unwrap();

    let approvals = ApprovalService::new(db.clone());
    let execution = ExecutionCoordinator::new(db.clone());
    let mock_client = Arc::new(MockExpediaProviderClient::new());
    // Simulate network timeout on dispatch
    mock_client
        .fail_with_timeout
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let write_service = ExpediaLodgingService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        approvals.clone(),
        execution.clone(),
        mock_client.clone(),
    );

    let tasks = DurableTaskService::new(db.clone());
    let task = tasks
        .start(
            &context,
            StartTaskRequest {
                title: "Book lodging timeout test".into(),
                instruction: "Book hotel".into(),
                agent_external_key: Some("saathi".into()),
            },
        )
        .await
        .unwrap();

    let details = sample_proposal_details(connection.id);
    let now = Utc::now();
    let proposal = write_service
        .propose_booking(
            &context,
            task.id,
            task.run_id,
            "saathi",
            connection.id,
            details,
            now + Duration::hours(1),
        )
        .await
        .unwrap();

    let approved = approvals
        .approve(&context, proposal.id, proposal.details, now)
        .await
        .unwrap();

    let idempotency_key = format!("affil-timeout-{}", Uuid::new_v4());

    // 1. Dispatch encounters timeout -> Halts cleanly into Reconciling without failing or retrying blindly
    let outcome = write_service
        .execute_booking(
            &context,
            approved.approval_id.unwrap(),
            &idempotency_key,
            now,
        )
        .await
        .unwrap();

    match outcome {
        ExpediaBookingOutcome::Reconciling {
            affiliate_reference_id,
            ..
        } => {
            assert_eq!(affiliate_reference_id, idempotency_key);
        }
        other => panic!("Expected Reconciling outcome on timeout, got {other:?}"),
    }

    // 2. The provider actually processed the request out of band
    mock_client
        .fail_with_timeout
        .store(false, std::sync::atomic::Ordering::SeqCst);
    mock_client.bookings.lock().unwrap().insert(
        idempotency_key.clone(),
        vox_core::providers::ExpediaRawBookingResponse {
            itinerary_id: "itin-reconciled-789".into(),
            confirmation_reference: "CONF-RECONCILED".into(),
            status: "booked".into(),
            challenge_url: None,
            challenge_token: None,
            affiliate_reference_id: idempotency_key.clone(),
            total_price_amount_minor: 75_000,
            price_currency: "USD".into(),
            cancellation_penalty_minor: None,
            refund_amount_minor: None,
        },
    );

    // 3. Subsequent reconciliation queries by affiliate_reference_id and transitions to Succeeded
    let reconciled = write_service
        .reconcile_booking(&context, &idempotency_key, now)
        .await
        .unwrap();

    match reconciled {
        ExpediaBookingOutcome::Succeeded {
            itinerary_id,
            confirmation_reference,
            booking_status,
            ..
        } => {
            assert_eq!(itinerary_id, "itin-reconciled-789");
            assert_eq!(confirmation_reference, "CONF-RECONCILED");
            assert_eq!(booking_status, "booked");
        }
        other => panic!("Expected Succeeded on reconciliation, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn context_minimization_and_zero_raw_card_handling() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-zero-card").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(ExpediaLodgingService::integration_declaration(&deployment))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: EXPEDIA_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: EXPEDIA_INTEGRATION_KEY.into(),
                external_account_reference: "partner_user_zero_card".into(),
                account_display_id: Some("alice@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![EXPEDIA_CAPABILITY_LODGING_BOOK.into()],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    let grants = CapabilityGrantService::new(db.clone());
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
            },
        )
        .await
        .unwrap();

    let approvals = ApprovalService::new(db.clone());
    let execution = ExecutionCoordinator::new(db.clone());
    let mock_client = Arc::new(MockExpediaProviderClient::new());
    let write_service = ExpediaLodgingService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        approvals.clone(),
        execution.clone(),
        mock_client.clone(),
    );

    let tasks = DurableTaskService::new(db.clone());
    let task = tasks
        .start(
            &context,
            StartTaskRequest {
                title: "Book lodging zero card check".into(),
                instruction: "Book hotel".into(),
                agent_external_key: Some("saathi".into()),
            },
        )
        .await
        .unwrap();

    let details = sample_proposal_details(connection.id);
    let proposal = write_service
        .propose_booking(
            &context,
            task.id,
            task.run_id,
            "saathi",
            connection.id,
            details,
            Utc::now() + Duration::hours(1),
        )
        .await
        .unwrap();

    let serialized = serde_json::to_string(&proposal.details).unwrap();
    // Invariant: Zero raw card data entering Vox Core or agent context
    assert!(!serialized.contains("card_number"));
    assert!(!serialized.contains("cvv"));
    assert!(!serialized.contains("pan"));
    assert!(!serialized.contains("credit_card"));
    assert!(!serialized.contains("expiration_date"));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn consequential_writes_http_endpoints_require_signed_host_assertion() {
    let db = setup().await;
    let (deployment, context, host_app, host_req) = create_context(&db, "user-http-write").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(ExpediaLodgingService::integration_declaration(&deployment))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: EXPEDIA_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: EXPEDIA_INTEGRATION_KEY.into(),
                external_account_reference: "partner_user_http".into(),
                account_display_id: Some("alice@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![EXPEDIA_CAPABILITY_LODGING_BOOK.into()],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    let grants = CapabilityGrantService::new(db.clone());
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
            },
        )
        .await
        .unwrap();

    let approvals = ApprovalService::new(db.clone());
    let execution = ExecutionCoordinator::new(db.clone());
    let mock_client = Arc::new(MockExpediaProviderClient::new());
    let write_service = Arc::new(ExpediaLodgingService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        approvals.clone(),
        execution.clone(),
        mock_client.clone(),
    ));

    let app = vox_core::http::router(
        vox_core::http::AppState::with_host_trust(db.clone(), "test-token".into())
            .with_expedia_write(write_service),
    );

    let tasks = DurableTaskService::new(db.clone());
    let task = tasks
        .start(
            &context,
            StartTaskRequest {
                title: "Book hotel over HTTP".into(),
                instruction: "Book hotel".into(),
                agent_external_key: Some("saathi".into()),
            },
        )
        .await
        .unwrap();

    let details = sample_proposal_details(connection.id);
    let now = Utc::now();
    let body_json = json!({
        "host_context": host_req,
        "span_id": task.id,
        "task_run_id": task.run_id,
        "agent_external_key": "saathi",
        "connection_id": connection.id,
        "details": details,
        "expires_at": now + Duration::hours(1),
    });

    // 1. Unauthenticated request without signed headers -> 401 Unauthorized
    let unauth_req = Request::builder()
        .method("POST")
        .uri("/v1/consequential-writes/propose")
        .header("content-type", "application/json")
        .body(Body::from(body_json.to_string()))
        .unwrap();

    let unauth_resp = app.clone().oneshot(unauth_req).await.unwrap();
    assert_eq!(unauth_resp.status(), StatusCode::UNAUTHORIZED);

    // 2. Properly signed request with host assertions -> 201 Created
    let assertion = host_app
        .credential
        .sign_context_request(&host_req, now, Uuid::new_v4())
        .unwrap();

    let auth_req = Request::builder()
        .method("POST")
        .uri("/v1/consequential-writes/propose")
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
        .body(Body::from(body_json.to_string()))
        .unwrap();

    let auth_resp = app.oneshot(auth_req).await.unwrap();
    assert_eq!(auth_resp.status(), StatusCode::CREATED);

    let bytes = axum::body::to_bytes(auth_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let resp_val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        resp_val["capability_external_key"],
        EXPEDIA_CAPABILITY_LODGING_BOOK
    );
}
