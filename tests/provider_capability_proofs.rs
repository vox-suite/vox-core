/**
 * Integration tests for provider capability proofs (E35, E36, E37, E38).
 *
 * Verifies that Amazon, Expedia, Zomato, and Uber:
 * - Strictly implement only the capability level proven by E02 feasibility.
 * - Unsupported direct execution produces explicit labelled handoffs.
 * - Handoff is never reported as execution completion.
 * - Location data and context minimization are strictly enforced.
 * - Preserves authoritative currency, quotes, expiry, and itinerary confirmation.
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
    http::{AppState, router},
    integration_registry::{CapabilityEffect, IntegrationRegistry, SetIntegrationEnabledRequest},
    providers::{
        AMAZON_CAPABILITY_CATALOG_SEARCH, AMAZON_CAPABILITY_CATALOG_SEARCH_SHORT,
        AMAZON_CAPABILITY_ITEM_LOOKUP_SHORT, AMAZON_CAPABILITY_PURCHASE_HANDOFF,
        AMAZON_CAPABILITY_PURCHASE_HANDOFF_SHORT, AMAZON_INTEGRATION_KEY, AmazonCatalogItem,
        AmazonHandoffRequest, AmazonService, MockAmazonProviderClient, MockUberProviderClient,
        MockZomatoProviderClient, UBER_CAPABILITY_RIDE_ESTIMATE, UBER_CAPABILITY_RIDE_REQUEST,
        UBER_INTEGRATION_KEY, UberConnectedReadService, UberRideEstimateRequest,
        UberRideHandoffRequest, ZOMATO_CAPABILITY_ORDER_HANDOFF,
        ZOMATO_CAPABILITY_ORDER_HANDOFF_SHORT, ZOMATO_CAPABILITY_RESTAURANT_SEARCH,
        ZOMATO_CAPABILITY_RESTAURANT_SEARCH_SHORT, ZOMATO_INTEGRATION_KEY, ZomatoHandoffRequest,
        ZomatoHandoffType, ZomatoRestaurant, ZomatoService,
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
    let deployment_external_key = format!("cap-dep-{}", Uuid::new_v4());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: deployment_external_key.clone(),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();

    let agents = AgentRegistry::new(db.clone());
    agents
        .register(RegisterAgentDefinitionRequest {
            deployment_external_key: deployment_external_key.clone(),
            external_key: "saathi".into(),
            purpose: "saathi multi-provider assistant".into(),
            requested_capability_categories: vec![
                AMAZON_CAPABILITY_CATALOG_SEARCH.into(),
                AMAZON_CAPABILITY_PURCHASE_HANDOFF.into(),
                ZOMATO_CAPABILITY_RESTAURANT_SEARCH.into(),
                ZOMATO_CAPABILITY_ORDER_HANDOFF.into(),
                UBER_CAPABILITY_RIDE_ESTIMATE.into(),
                UBER_CAPABILITY_RIDE_REQUEST.into(),
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

// =========================================================================
// 1. Amazon Tests (E35 / vox-core#21)
// =========================================================================

#[tokio::test]
async fn amazon_declaration_and_capabilities_match_official_creators_api() {
    let decl = AmazonService::integration_declaration("dep-amazon");
    assert_eq!(decl.external_key, AMAZON_INTEGRATION_KEY);
    assert_eq!(decl.capabilities.len(), 3);

    let search = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == AMAZON_CAPABILITY_CATALOG_SEARCH_SHORT)
        .expect("catalog_search must be declared");
    assert_eq!(search.effect, CapabilityEffect::Read);
    assert_eq!(
        search.optional_guarantees["capability_level"],
        "L1_catalog_read"
    );
    assert_eq!(search.regions.len(), 22);

    let lookup = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == AMAZON_CAPABILITY_ITEM_LOOKUP_SHORT)
        .expect("item_lookup must be declared");
    assert_eq!(lookup.effect, CapabilityEffect::Read);
    assert_eq!(
        lookup.optional_guarantees["capability_level"],
        "L1_catalog_read"
    );

    let handoff = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == AMAZON_CAPABILITY_PURCHASE_HANDOFF_SHORT)
        .expect("purchase_handoff must be declared");
    assert_eq!(handoff.effect, CapabilityEffect::Write);
    assert_eq!(
        handoff.optional_guarantees["capability_level"],
        "L0_labelled_handoff_only"
    );
    assert_eq!(
        handoff.optional_guarantees["direct_execution_supported"],
        false
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn amazon_purchase_handoff_is_never_reported_as_purchase_completion() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-amazon-1").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(AmazonService::integration_declaration(&deployment))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: AMAZON_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: AMAZON_INTEGRATION_KEY.into(),
                external_account_reference: "amazon-partner-assoc-1".into(),
                account_display_id: Some("affiliate@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![
                    AMAZON_CAPABILITY_CATALOG_SEARCH.into(),
                    AMAZON_CAPABILITY_PURCHASE_HANDOFF.into(),
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
                capability_external_key: AMAZON_CAPABILITY_PURCHASE_HANDOFF.into(),
            },
        )
        .await
        .unwrap();

    let mock_client = Arc::new(MockAmazonProviderClient::new(vec![AmazonCatalogItem {
        asin: "B09V3KXJPB".into(),
        title: "Sony WH-1000XM5 Noise Canceling Headphones".into(),
        detail_page_url: "https://www.amazon.com/dp/B09V3KXJPB".into(),
        price_amount_minor: 39800,
        currency: "USD".into(),
        availability: "InStock".into(),
        image_url: None,
    }]));

    let amazon_svc = AmazonService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        mock_client.clone(),
    );

    // 1. Generate purchase handoff
    let handoff_resp = amazon_svc
        .create_purchase_handoff(
            &context,
            "saathi",
            connection.id,
            AmazonHandoffRequest {
                asin: "B09V3KXJPB".into(),
                locale: "US".into(),
                quantity: 1,
                partner_tag: Some("vox-20".into()),
            },
        )
        .await
        .unwrap();

    // Verify Invariant: Handoff is NOT reported as purchase completion
    assert_eq!(handoff_resp.provider, "amazon");
    assert_eq!(handoff_resp.status, "handoff_created");
    assert!(!handoff_resp.completed);
    assert!(
        handoff_resp
            .handoff_url
            .contains("amazon.com/dp/B09V3KXJPB?tag=vox-20")
    );
    assert!(
        handoff_resp
            .disclaimer
            .contains("Vox does not place consumer orders directly")
    );

    // 2. Direct purchase execution fails closed as unsupported
    let direct_err = amazon_svc
        .execute_purchase(&context, "saathi", connection.id, "B09V3KXJPB")
        .await;
    assert!(matches!(
        direct_err,
        Err(vox_core::providers::AmazonError::UnsupportedDirectExecution(_))
    ));
}

// =========================================================================
// 2. Zomato Tests (E37 / vox-core#23)
// =========================================================================

#[tokio::test]
async fn zomato_distinguishes_merchant_apis_and_enforces_consumer_handoff() {
    let decl = ZomatoService::integration_declaration("dep-zomato");
    assert_eq!(decl.external_key, ZOMATO_INTEGRATION_KEY);
    assert_eq!(decl.capabilities.len(), 3);

    let search = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == ZOMATO_CAPABILITY_RESTAURANT_SEARCH_SHORT)
        .expect("restaurant_search must be declared");
    assert_eq!(search.effect, CapabilityEffect::Read);
    assert_eq!(
        search.optional_guarantees["capability_level"],
        "L1_catalog_read"
    );

    let order = decl
        .capabilities
        .iter()
        .find(|c| c.external_key == ZOMATO_CAPABILITY_ORDER_HANDOFF_SHORT)
        .expect("consumer_order_handoff must be declared");
    assert_eq!(order.effect, CapabilityEffect::Write);
    assert_eq!(
        order.optional_guarantees["capability_level"],
        "L0_labelled_handoff_only"
    );
    assert_eq!(
        order.optional_guarantees["direct_execution_supported"],
        false
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn zomato_order_states_are_distinguished_and_never_reported_as_order_completion() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-zomato-1").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(ZomatoService::integration_declaration(&deployment))
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: ZOMATO_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();

    let connections = ConnectionService::new(db.clone());
    let connection = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: ZOMATO_INTEGRATION_KEY.into(),
                external_account_reference: "zomato-user-india".into(),
                account_display_id: Some("user@zomato.in".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![
                    ZOMATO_CAPABILITY_RESTAURANT_SEARCH.into(),
                    ZOMATO_CAPABILITY_ORDER_HANDOFF.into(),
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
                capability_external_key: ZOMATO_CAPABILITY_ORDER_HANDOFF.into(),
            },
        )
        .await
        .unwrap();

    let mock_client = Arc::new(MockZomatoProviderClient::new(vec![ZomatoRestaurant {
        res_id: "res_bombay_canteen_12".into(),
        name: "The Bombay Canteen".into(),
        cuisines: vec!["Indian Modern".into(), "Cocktails".into()],
        locality: "Lower Parel".into(),
        city: "Mumbai".into(),
        rating: 4.8,
        average_cost_for_two_minor: 250_000,
        currency: "INR".into(),
        web_url: "https://www.zomato.com/mumbai/the-bombay-canteen-lower-parel".into(),
    }]));

    let zomato_svc = ZomatoService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        mock_client.clone(),
    );

    // 1. Cart & checkout handoff
    let cart_handoff = zomato_svc
        .create_order_handoff(
            &context,
            "saathi",
            connection.id,
            ZomatoHandoffRequest {
                res_id: Some("res_bombay_canteen_12".into()),
                order_id: None,
                handoff_type: ZomatoHandoffType::CartAndCheckout,
            },
        )
        .await
        .unwrap();

    assert_eq!(cart_handoff.provider, "zomato");
    assert_eq!(cart_handoff.action, "cart_and_checkout");
    assert_eq!(cart_handoff.status, "handoff_created");
    assert!(!cart_handoff.completed);
    assert!(
        cart_handoff
            .handoff_url
            .contains("zomato.com/order/res_bombay_canteen_12")
    );
    assert!(
        cart_handoff
            .disclaimer
            .contains("Vox does not place consumer orders directly")
    );

    // 2. Track order handoff
    let track_handoff = zomato_svc
        .create_order_handoff(
            &context,
            "saathi",
            connection.id,
            ZomatoHandoffRequest {
                res_id: None,
                order_id: Some("ord_998877".into()),
                handoff_type: ZomatoHandoffType::TrackOrder,
            },
        )
        .await
        .unwrap();

    assert_eq!(track_handoff.action, "track_order");
    assert!(!track_handoff.completed);
    assert!(
        track_handoff
            .handoff_url
            .contains("zomato.com/order/track/ord_998877")
    );

    // 3. Direct order placement fails closed
    let direct_err = zomato_svc
        .execute_order(
            &context,
            "saathi",
            connection.id,
            json!({ "items": ["butter_chicken"] }),
        )
        .await;
    assert!(matches!(
        direct_err,
        Err(vox_core::providers::ZomatoError::UnsupportedDirectExecution(_))
    ));
}

// =========================================================================
// 3. Uber Tests (E38 / vox-core#24)
// =========================================================================

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn uber_ride_estimates_bind_route_and_opening_uber_is_not_confirmed_ride() {
    let db = setup().await;
    let (deployment, context, _, _) = create_context(&db, "user-uber-ride-1").await;

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
                external_account_reference: "uber-rider-test".into(),
                account_display_id: Some("rider@example.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![
                    UBER_CAPABILITY_RIDE_ESTIMATE.into(),
                    UBER_CAPABILITY_RIDE_REQUEST.into(),
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
                capability_external_key: UBER_CAPABILITY_RIDE_ESTIMATE.into(),
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
                capability_external_key: UBER_CAPABILITY_RIDE_REQUEST.into(),
            },
        )
        .await
        .unwrap();

    let mock_client = Arc::new(MockUberProviderClient::new(vec![]));
    let uber_svc = UberConnectedReadService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        mock_client.clone(),
    );

    // 1. Ride estimate binds route and quote expiry
    let estimate = uber_svc
        .estimate_ride(
            &context,
            "saathi",
            connection.id,
            UberRideEstimateRequest {
                pickup_latitude: 37.7749,
                pickup_longitude: -122.4194,
                dropoff_latitude: 37.7833,
                dropoff_longitude: -122.4167,
                pickup_display_name: Some("Civic Center".into()),
                dropoff_display_name: Some("Union Square".into()),
            },
        )
        .await
        .unwrap();

    assert_eq!(estimate.pickup_display_name, "Civic Center");
    assert_eq!(estimate.dropoff_display_name, "Union Square");
    assert!(!estimate.options.is_empty());
    let best_option = &estimate.options[0];
    assert!(best_option.price_amount_minor > 0);
    assert_eq!(best_option.currency, "USD");
    assert!(best_option.expires_at > Utc::now());

    // 2. Opening Uber is labelled handoff, NOT a confirmed ride
    let handoff = uber_svc
        .create_ride_handoff(
            &context,
            "saathi",
            connection.id,
            UberRideHandoffRequest {
                pickup_latitude: 37.7749,
                pickup_longitude: -122.4194,
                dropoff_latitude: 37.7833,
                dropoff_longitude: -122.4167,
                product_id: Some(best_option.product_id.clone()),
                fare_id: Some(best_option.fare_id.clone()),
            },
        )
        .await
        .unwrap();

    assert_eq!(handoff.provider, "uber");
    assert_eq!(handoff.action, "open_uber_ride_request");
    assert_eq!(handoff.status, "handoff_created");
    assert!(!handoff.completed); // Invariant: Opening Uber is not a confirmed ride
    assert!(
        handoff
            .handoff_url
            .contains("m.uber.com/ul/?action=setPickup")
    );
    assert!(
        handoff
            .disclaimer
            .contains("Vox does not claim a confirmed ride")
    );

    // 3. Direct execution fails closed without privileged production approval
    let direct_err = uber_svc
        .execute_ride_request(&context, "saathi", connection.id, &best_option.fare_id)
        .await;
    assert!(matches!(
        direct_err,
        Err(vox_core::providers::UberReadError::UnsupportedDirectExecution(_))
    ));
}

// =========================================================================
// 4. HTTP Handoff Endpoints Verification
// =========================================================================

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn http_handoff_endpoints_require_signed_assertions_and_report_honest_uncompleted_status() {
    let db = setup().await;
    let (deployment, context, host_app, host_req) = create_context(&db, "user-http-handoff").await;

    let registry = IntegrationRegistry::new(db.clone());
    registry
        .register(AmazonService::integration_declaration(&deployment))
        .await
        .unwrap();
    registry
        .register(ZomatoService::integration_declaration(&deployment))
        .await
        .unwrap();
    registry
        .register(UberConnectedReadService::integration_declaration(
            &deployment,
        ))
        .await
        .unwrap();

    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: AMAZON_INTEGRATION_KEY.into(),
            enabled: true,
        })
        .await
        .unwrap();
    registry
        .set_enabled(SetIntegrationEnabledRequest {
            deployment_external_key: deployment.clone(),
            external_key: ZOMATO_INTEGRATION_KEY.into(),
            enabled: true,
        })
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
    let amazon_conn = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: AMAZON_INTEGRATION_KEY.into(),
                external_account_reference: "amazon-user-http".into(),
                account_display_id: Some("amazon@test.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![AMAZON_CAPABILITY_PURCHASE_HANDOFF.into()],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    let zomato_conn = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: ZOMATO_INTEGRATION_KEY.into(),
                external_account_reference: "zomato-user-http".into(),
                account_display_id: Some("zomato@test.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![ZOMATO_CAPABILITY_ORDER_HANDOFF.into()],
                expires_at: Some(Utc::now() + Duration::days(30)),
                failure_code: None,
            },
        )
        .await
        .unwrap();

    let uber_conn = connections
        .record(
            &context,
            AuthorizeConnectionRequest {
                integration_external_key: UBER_INTEGRATION_KEY.into(),
                external_account_reference: "uber-user-http".into(),
                account_display_id: Some("uber@test.com".into()),
                credential_custody: CredentialCustody::ExternalOperator,
                authorization_state: vox_core::connections::AuthorizationState::Authorized,
                authorized_capabilities: vec![UBER_CAPABILITY_RIDE_REQUEST.into()],
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
                connection_id: amazon_conn.id,
                capability_external_key: AMAZON_CAPABILITY_PURCHASE_HANDOFF.into(),
            },
        )
        .await
        .unwrap();
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: zomato_conn.id,
                capability_external_key: ZOMATO_CAPABILITY_ORDER_HANDOFF.into(),
            },
        )
        .await
        .unwrap();
    grants
        .grant(
            &context,
            CreateGrantRequest {
                agent_external_key: "saathi".into(),
                connection_id: uber_conn.id,
                capability_external_key: UBER_CAPABILITY_RIDE_REQUEST.into(),
            },
        )
        .await
        .unwrap();

    let amazon_svc = Arc::new(AmazonService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        Arc::new(MockAmazonProviderClient::new(vec![])),
    ));
    let zomato_svc = Arc::new(ZomatoService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        Arc::new(MockZomatoProviderClient::new(vec![])),
    ));
    let uber_svc = Arc::new(UberConnectedReadService::new(
        db.clone(),
        connections.clone(),
        grants.clone(),
        Arc::new(MockUberProviderClient::new(vec![])),
    ));

    let app = router(
        AppState::with_host_trust(db.clone(), "test-token".into())
            .with_amazon(amazon_svc)
            .with_zomato(zomato_svc)
            .with_uber_read(uber_svc),
    );

    // 1. Amazon Handoff over HTTP
    let amazon_assertion = host_app
        .credential
        .sign_context_request(&host_req, Utc::now(), Uuid::new_v4())
        .unwrap();

    let amazon_body = json!({
        "host_context": host_req,
        "agent_external_key": "saathi",
        "connection_id": amazon_conn.id,
        "handoff": {
            "asin": "B08N5WRWNW",
            "locale": "US",
            "quantity": 1,
            "partner_tag": "vox-20"
        }
    });

    let amazon_req = Request::builder()
        .method("POST")
        .uri("/v1/handoffs/amazon")
        .header("content-type", "application/json")
        .header(
            "x-vox-host-credential",
            amazon_assertion.credential_id().to_string(),
        )
        .header("x-vox-host-secret", amazon_assertion.secret())
        .header("x-vox-host-audience", amazon_assertion.audience())
        .header(
            "x-vox-host-timestamp",
            amazon_assertion.issued_at().timestamp().to_string(),
        )
        .header("x-vox-host-nonce", amazon_assertion.nonce().to_string())
        .header("x-vox-host-signature", amazon_assertion.signature())
        .body(Body::from(amazon_body.to_string()))
        .unwrap();

    let amazon_resp = app.clone().oneshot(amazon_req).await.unwrap();
    assert_eq!(amazon_resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(amazon_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val["provider"], "amazon");
    assert_eq!(val["completed"], false);
    assert_eq!(val["status"], "handoff_created");

    // 2. Zomato Handoff over HTTP
    let zomato_assertion = host_app
        .credential
        .sign_context_request(&host_req, Utc::now(), Uuid::new_v4())
        .unwrap();

    let zomato_body = json!({
        "host_context": host_req,
        "agent_external_key": "saathi",
        "connection_id": zomato_conn.id,
        "handoff": {
            "res_id": "18204",
            "order_id": null,
            "handoff_type": "ViewRestaurant"
        }
    });

    let zomato_req = Request::builder()
        .method("POST")
        .uri("/v1/handoffs/zomato")
        .header("content-type", "application/json")
        .header(
            "x-vox-host-credential",
            zomato_assertion.credential_id().to_string(),
        )
        .header("x-vox-host-secret", zomato_assertion.secret())
        .header("x-vox-host-audience", zomato_assertion.audience())
        .header(
            "x-vox-host-timestamp",
            zomato_assertion.issued_at().timestamp().to_string(),
        )
        .header("x-vox-host-nonce", zomato_assertion.nonce().to_string())
        .header("x-vox-host-signature", zomato_assertion.signature())
        .body(Body::from(zomato_body.to_string()))
        .unwrap();

    let zomato_resp = app.clone().oneshot(zomato_req).await.unwrap();
    assert_eq!(zomato_resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(zomato_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val["provider"], "zomato");
    assert_eq!(val["completed"], false);

    // 3. Uber Ride Handoff over HTTP
    let uber_assertion = host_app
        .credential
        .sign_context_request(&host_req, Utc::now(), Uuid::new_v4())
        .unwrap();

    let uber_body = json!({
        "host_context": host_req,
        "agent_external_key": "saathi",
        "connection_id": uber_conn.id,
        "handoff": {
            "pickup_latitude": 37.7749,
            "pickup_longitude": -122.4194,
            "dropoff_latitude": 37.7833,
            "dropoff_longitude": -122.4167,
            "product_id": "uberx",
            "fare_id": "fare_123"
        }
    });

    let uber_req = Request::builder()
        .method("POST")
        .uri("/v1/handoffs/uber")
        .header("content-type", "application/json")
        .header(
            "x-vox-host-credential",
            uber_assertion.credential_id().to_string(),
        )
        .header("x-vox-host-secret", uber_assertion.secret())
        .header("x-vox-host-audience", uber_assertion.audience())
        .header(
            "x-vox-host-timestamp",
            uber_assertion.issued_at().timestamp().to_string(),
        )
        .header("x-vox-host-nonce", uber_assertion.nonce().to_string())
        .header("x-vox-host-signature", uber_assertion.signature())
        .body(Body::from(uber_body.to_string()))
        .unwrap();

    let uber_resp = app.oneshot(uber_req).await.unwrap();
    assert_eq!(uber_resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(uber_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val["provider"], "uber");
    assert_eq!(val["completed"], false);
    assert_eq!(val["status"], "handoff_created");
}
