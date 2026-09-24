/**
 * Integration tests for selected connected read integration (Uber rider trip history) (E33).
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
    capability_grants::{CapabilityGrantService, CreateGrantRequest},
    connections::{AuthorizeConnectionRequest, ConnectionService, CredentialCustody},
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    integration_registry::{IntegrationRegistry, SetIntegrationEnabledRequest},
    providers::{
        MockUberProviderClient, UBER_CAPABILITY_HISTORY, UBER_CAPABILITY_HISTORY_LITE,
        UBER_CAPABILITY_HISTORY_LITE_SHORT, UBER_CAPABILITY_HISTORY_SHORT,
        UBER_CAPABILITY_RIDE_ESTIMATE_SHORT, UBER_CAPABILITY_RIDE_REQUEST_SHORT,
        UBER_INTEGRATION_KEY, UberConnectedReadService, UberRawTrip, UberReadError,
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
    let deployment_external_key = format!("connected-read-{}", Uuid::new_v4());
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
            purpose: "saathi personal assistant".into(),
            requested_capability_categories: vec![
                UBER_CAPABILITY_HISTORY_LITE.into(),
                UBER_CAPABILITY_HISTORY.into(),
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

fn sample_raw_trips() -> Vec<UberRawTrip> {
    vec![
        UberRawTrip {
            trip_id: "trip-101".into(),
            request_time: Utc::now() - Duration::hours(2),
            status: "completed".into(),
            distance_miles: 4.8,
            start_city: Some("San Francisco".into()),
            pickup_latitude: Some(37.7749),
            pickup_longitude: Some(-122.4194),
            dropoff_latitude: Some(37.7891),
            dropoff_longitude: Some(-122.4014),
            payment_method_id: Some("card_pm_secret_123".into()),
            internal_rider_token: Some("rider_priv_token_xyz".into()),
        },
        UberRawTrip {
            trip_id: "trip-102".into(),
            request_time: Utc::now() - Duration::days(1),
            status: "completed".into(),
            distance_miles: 12.3,
            start_city: Some("Oakland".into()),
            pickup_latitude: Some(37.8044),
            pickup_longitude: Some(-122.2712),
            dropoff_latitude: Some(37.7749),
            dropoff_longitude: Some(-122.4194),
            payment_method_id: Some("card_pm_secret_123".into()),
            internal_rider_token: Some("rider_priv_token_xyz".into()),
        },
        UberRawTrip {
            trip_id: "trip-103".into(),
            request_time: Utc::now() - Duration::days(3),
            status: "driver_cancelled".into(),
            distance_miles: 0.0,
            start_city: Some("San Francisco".into()),
            pickup_latitude: Some(37.7600),
            pickup_longitude: Some(-122.4200),
            dropoff_latitude: None,
            dropoff_longitude: None,
            payment_method_id: Some("card_pm_secret_123".into()),
            internal_rider_token: Some("rider_priv_token_xyz".into()),
        },
    ]
}

#[tokio::test]
async fn integration_declaration_matches_approved_feasibility_finding() {
    let decl = UberConnectedReadService::integration_declaration("dep-test");
    assert_eq!(decl.external_key, UBER_INTEGRATION_KEY);
    assert_eq!(decl.capabilities.len(), 4);

    // 1. History lite: L2 connected read, data minimized
    let lite = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == UBER_CAPABILITY_HISTORY_LITE_SHORT)
        .expect("history_lite capability must be declared");
    assert_eq!(
        lite.effect,
        vox_core::integration_registry::CapabilityEffect::Read
    );
    assert_eq!(lite.access_needs, vec!["history_lite"]);
    assert_eq!(lite.data_recipients, vec!["api.uber.com"]);
    assert_eq!(
        lite.optional_guarantees["capability_level"],
        "L2_connected_read"
    );

    // 2. Full History: L2 connected read with city
    let history = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == UBER_CAPABILITY_HISTORY_SHORT)
        .expect("history capability must be declared");
    assert_eq!(
        history.effect,
        vox_core::integration_registry::CapabilityEffect::Read
    );
    assert_eq!(history.access_needs, vec!["history"]);
    assert_eq!(history.optional_guarantees["includes_city"], true);

    // 3. Ride estimate: L1 catalog read
    let estimate = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == UBER_CAPABILITY_RIDE_ESTIMATE_SHORT)
        .expect("ride_estimate capability must be declared");
    assert_eq!(
        estimate.effect,
        vox_core::integration_registry::CapabilityEffect::Read
    );
    assert_eq!(
        estimate.optional_guarantees["capability_level"],
        "L1_catalog_read"
    );

    // 4. Ride request: Consequential write, but strictly L0 labelled handoff only
    let request = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == UBER_CAPABILITY_RIDE_REQUEST_SHORT)
        .expect("ride_request capability must be declared");
    assert_eq!(
        request.effect,
        vox_core::integration_registry::CapabilityEffect::Write
    );
    assert_eq!(
        request.optional_guarantees["capability_level"],
        "L0_labelled_handoff_only"
    );
    assert_eq!(
        request.optional_guarantees["direct_execution_supported"],
        false
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn connected_read_enforces_context_minimization() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-minimized").await;

    // Register integration in registry and enable it
    let registry = IntegrationRegistry::new(db.clone());
    let decl = UberConnectedReadService::integration_declaration(&deployment);
    registry.register(decl).await.unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: UBER_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    // Authorize connection for integration 'uber'
    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: UBER_INTEGRATION_KEY.into(),
                external_account_reference: "rider_uber_123".into(),
                account_display_id: Some("rider@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![
                    UBER_CAPABILITY_HISTORY_LITE.into(),
                    UBER_CAPABILITY_HISTORY.into(),
                ],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    // Grant capability to agent 'saathi'
    let grants = CapabilityGrantService::new(db.clone());
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: UBER_CAPABILITY_HISTORY_LITE.into(),
            },
        )
        .await
        .unwrap();

    let mock_client = Arc::new(MockUberProviderClient::new(sample_raw_trips()));
    let read_service = UberConnectedReadService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        mock_client.clone(),
    );

    // 1. Read with uber.history_lite: city is None, coordinates & tokens stripped
    let lite_res = read_service
        .read_history(
            &context,
            "saathi",
            connection.id,
            UBER_CAPABILITY_HISTORY_LITE,
            0,
            10,
        )
        .await
        .unwrap();

    assert_eq!(lite_res.trips.len(), 3);
    assert_eq!(lite_res.trips[0].trip_id, "trip-101");
    assert_eq!(lite_res.trips[0].status, "completed");
    assert_eq!(lite_res.trips[0].city, None); // City omitted in history_lite

    // 2. Grant uber.history to saathi and read again: city is included, coordinates & tokens remain stripped
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: connection.id,
                capability_external_key: UBER_CAPABILITY_HISTORY.into(),
            },
        )
        .await
        .unwrap();

    let full_res = read_service
        .read_history(
            &context,
            "saathi",
            connection.id,
            UBER_CAPABILITY_HISTORY,
            0,
            10,
        )
        .await
        .unwrap();

    assert_eq!(full_res.trips.len(), 3);
    assert_eq!(full_res.trips[0].city, Some("San Francisco".into()));
    assert_eq!(full_res.trips[1].city, Some("Oakland".into()));

    // Verify context minimization: serialization of trips does NOT contain coordinates, payment IDs, or tokens
    let serialized = serde_json::to_string(&full_res.trips).unwrap();
    assert!(!serialized.contains("pickup_latitude"));
    assert!(!serialized.contains("payment_method_id"));
    assert!(!serialized.contains("internal_rider_token"));
    assert!(!serialized.contains("card_pm_secret_123"));
    assert!(!serialized.contains("rider_priv_token_xyz"));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn expired_or_revoked_access_pauses_work_and_reconnection_rechecks_facts() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-lifecycle").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(UberConnectedReadService::integration_declaration(
            &deployment,
        ))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: UBER_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let grants = CapabilityGrantService::new(db.clone());
    // Authorize connection initially, grant capability, then expire connection to test lifecycle
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: UBER_INTEGRATION_KEY.into(),
                external_account_reference: "rider_lifecycle".into(),
                account_display_id: Some("rider@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![UBER_CAPABILITY_HISTORY_LITE.into()],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
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
                capability_external_key: UBER_CAPABILITY_HISTORY_LITE.into(),
            },
        )
        .await
        .unwrap();

    // Now connection expires
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: UBER_INTEGRATION_KEY.into(),
                external_account_reference: "rider_lifecycle".into(),
                account_display_id: Some("rider@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Expired,
                authorized_capabilities: vec![UBER_CAPABILITY_HISTORY_LITE.into()],
                expires_at: None,
                failure_code: None,
            },
        )
        .await
        .unwrap();

    let mock_client = Arc::new(MockUberProviderClient::new(sample_raw_trips()));
    let read_service = UberConnectedReadService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        mock_client.clone(),
    );

    // 1. Reading with expired connection fails closed with ReconnectRequired
    let err = read_service
        .read_history(
            &context,
            "saathi",
            connection.id,
            UBER_CAPABILITY_HISTORY_LITE,
            0,
            10,
        )
        .await
        .unwrap_err();

    match err {
        UberReadError::ReconnectRequired => {}
        other => panic!("Expected ReconnectRequired, got: {other:?}"),
    }

    // 2. Reconnection event: User re-authorizes connection with fresh token & future expiration
    connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: UBER_INTEGRATION_KEY.into(),
                external_account_reference: "rider_lifecycle".into(),
                account_display_id: Some("rider@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![UBER_CAPABILITY_HISTORY_LITE.into()],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    // 3. Subsequent read succeeds and re-checks verified provider facts
    let fresh_res = read_service
        .read_history(
            &context,
            "saathi",
            connection.id,
            UBER_CAPABILITY_HISTORY_LITE,
            0,
            10,
        )
        .await
        .unwrap();

    assert_eq!(fresh_res.trips.len(), 3);
    assert_eq!(fresh_res.trips[0].trip_id, "trip-101");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cross_context_and_ungranted_capability_boundary_enforcement() {
    let db = setup().await;
    let (deployment_a, context_a, _, _) = create_context(&db, "user-alice").await;
    let (_, context_b, _, _) = create_context(&db, "user-bob").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(UberConnectedReadService::integration_declaration(
            &deployment_a,
        ))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment_a.clone(),
            external_key: UBER_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let connection_a = connections
        .record(
            &context_a,
            AuthorizeConnectionRequest {
                integration_external_key: UBER_INTEGRATION_KEY.into(),
                external_account_reference: "alice_rider".into(),
                account_display_id: Some("alice@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![UBER_CAPABILITY_HISTORY_LITE.into()],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    let grants = CapabilityGrantService::new(db.clone());
    let mock_client = Arc::new(MockUberProviderClient::new(sample_raw_trips()));
    let read_service = UberConnectedReadService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        mock_client.clone(),
    );

    // 1. Bob attempts to read Alice's connection -> Fails (ConnectionNotFound)
    let err_cross = read_service
        .read_history(
            &context_b,
            "saathi",
            connection_a.id,
            UBER_CAPABILITY_HISTORY_LITE,
            0,
            10,
        )
        .await
        .unwrap_err();

    match err_cross {
        UberReadError::ConnectionNotFound => {}
        other => panic!("Expected ConnectionNotFound, got: {other:?}"),
    }

    // 2. Alice attempts to read without an agent grant -> Fails (UnauthorizedCapability)
    let err_nogrant = read_service
        .read_history(
            &context_a,
            "saathi",
            connection_a.id,
            UBER_CAPABILITY_HISTORY_LITE,
            0,
            10,
        )
        .await
        .unwrap_err();

    match err_nogrant {
        UberReadError::UnauthorizedCapability(cap, _) => {
            assert_eq!(cap, UBER_CAPABILITY_HISTORY_LITE);
        }
        other => panic!("Expected UnauthorizedCapability, got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn rate_limiting_and_pagination() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-paged").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(UberConnectedReadService::integration_declaration(
            &deployment,
        ))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: UBER_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: UBER_INTEGRATION_KEY.into(),
                external_account_reference: "rider_paged".into(),
                account_display_id: Some("rider@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![UBER_CAPABILITY_HISTORY_LITE.into()],
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
                capability_external_key: UBER_CAPABILITY_HISTORY_LITE.into(),
            },
        )
        .await
        .unwrap();

    let mock_client = Arc::new(MockUberProviderClient::new(sample_raw_trips()));
    let read_service = UberConnectedReadService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        mock_client.clone(),
    );

    // 1. Pagination: offset=1, limit=1
    let paged = read_service
        .read_history(
            &context,
            "saathi",
            connection.id,
            UBER_CAPABILITY_HISTORY_LITE,
            1,
            1,
        )
        .await
        .unwrap();

    assert_eq!(paged.count, 3);
    assert_eq!(paged.trips.len(), 1);
    assert_eq!(paged.trips[0].trip_id, "trip-102");

    // 2. Provider rate limiting: returns RateLimited error with retry-after
    mock_client
        .fail_with_rate_limit
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let err = read_service
        .read_history(
            &context,
            "saathi",
            connection.id,
            UBER_CAPABILITY_HISTORY_LITE,
            0,
            10,
        )
        .await
        .unwrap_err();

    match err {
        UberReadError::RateLimited(retry_after) => {
            assert_eq!(retry_after, 30);
        }
        other => panic!("Expected RateLimited(30), got: {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn connected_reads_http_endpoint_requires_signed_host_assertion() {
    let db = setup().await;
    let (deployment, context, host_app, host_req) = create_context(&db, "user-http-read").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(UberConnectedReadService::integration_declaration(
            &deployment,
        ))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: UBER_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: UBER_INTEGRATION_KEY.into(),
                external_account_reference: "rider_http".into(),
                account_display_id: Some("rider@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![UBER_CAPABILITY_HISTORY_LITE.into()],
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
                capability_external_key: UBER_CAPABILITY_HISTORY_LITE.into(),
            },
        )
        .await
        .unwrap();

    let mock_client = Arc::new(MockUberProviderClient::new(sample_raw_trips()));
    let read_service = Arc::new(UberConnectedReadService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        mock_client.clone(),
    ));

    let app = vox_core::http::router(
        vox_core::http::AppState::with_host_trust(db.clone(), "test-token".into())
            .with_uber_read(read_service),
    );

    // 1. Unauthenticated request without signed headers -> 401 Unauthorized
    let unauth_req = Request::builder()
        .method("POST")
        .uri("/v1/connected-reads")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "host_context": host_req,
                "agent_external_key": "saathi",
                "connection_id": connection.id,
                "capability_external_key": UBER_CAPABILITY_HISTORY_LITE,
            })
            .to_string(),
        ))
        .unwrap();

    let unauth_resp = app.clone().oneshot(unauth_req).await.unwrap();
    assert_eq!(unauth_resp.status(), StatusCode::UNAUTHORIZED);

    // 2. Properly signed request with host assertions -> 200 OK
    let now = Utc::now();
    let assertion = host_app
        .credential
        .sign_context_request(&host_req, now, Uuid::new_v4())
        .unwrap();

    let auth_req = Request::builder()
        .method("POST")
        .uri("/v1/connected-reads")
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
        .body(Body::from(
            json!({
                "host_context": host_req,
                "agent_external_key": "saathi",
                "connection_id": connection.id,
                "capability_external_key": UBER_CAPABILITY_HISTORY_LITE,
                "offset": 0,
                "limit": 10,
            })
            .to_string(),
        ))
        .unwrap();

    let auth_resp = app.oneshot(auth_req).await.unwrap();
    assert_eq!(auth_resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(auth_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: vox_core::providers::UberHistoryResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body.trips.len(), 3);
    assert_eq!(body.trips[0].trip_id, "trip-101");
}
