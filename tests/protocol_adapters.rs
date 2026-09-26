/**
 * Integration & Conformance Test Suite for MCP and Direct Protocol Adapters (E30).
 *
 * Verifies:
 * 1. Protocol Parity: MCP and Direct adapters pass the same read and consequential write conformance cases.
 * 2. Protocol Neutrality: Protocol choice provides no inherent trust or permission.
 * 3. Unsupported Guarantees: Provider guarantees are visible and never assumed.
 * 4. Context Minimization: Payloads are strictly filtered to declared access_needs; internal context is never leaked.
 * 5. Response Redaction: Leaked secrets, passwords, or tokens in provider responses are redacted.
 * 6. Dynamic Disablement: Disabling an adapter halts that protocol cleanly while preserving Core semantics and other adapters.
 * 7. Cryptographic Integrity: Outgoing requests carry valid tamper-evident assertions.
 * 8. Resilient Fuzzing: Defends against malformed JSON, protocol mismatches, oversized payloads, and network timeouts.
 */
use axum::{
    Router,
    extract::Json,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::net::TcpListener;
use uuid::Uuid;
use vox_core::{
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    remote_extensions::{
        AuthorizedEndpoint, ExtensionCapability, ExtensionEffect, ExtensionOperator,
        ExtensionProtocol, InstallExtensionRequest, RemoteExtensionService,
        adapters::{
            AdapterExecutionError, ExtensionInvocation, ProtocolRouter, ResponseStatus,
            direct::DirectProtocolAdapter,
            execution::ExtensionExecutionAdapter,
            integrity::{ExtensionIntegritySigner, IntegrityVerification},
            mcp::McpProtocolAdapter,
        },
    },
};

const TEST_SIGNING_SECRET: &[u8] = b"test-extension-integrity-key-32b";

async fn setup_db() -> Db {
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    db
}

async fn create_host_context(
    db: &Db,
    user: &str,
) -> (
    vox_core::identity::ResolvedUserContext,
    vox_core::host_trust::RegisteredHostApp,
) {
    let trust = HostTrustService::new(db.clone());
    let registered = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("ext-dep-{}", Uuid::new_v4()),
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
    (resolved, registered)
}

struct MockServerState {
    last_received_headers: tokio::sync::Mutex<HeaderMap>,
    last_received_body: tokio::sync::Mutex<Value>,
    call_count: AtomicUsize,
}

impl MockServerState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            last_received_headers: tokio::sync::Mutex::new(HeaderMap::new()),
            last_received_body: tokio::sync::Mutex::new(Value::Null),
            call_count: AtomicUsize::new(0),
        })
    }
}

async fn start_mock_mcp_server(state: Arc<MockServerState>) -> String {
    let app = Router::new().route(
        "/mcp",
        post({
            let state = Arc::clone(&state);
            move |headers: HeaderMap, Json(body): Json<Value>| {
                let state = Arc::clone(&state);
                async move {
                    *state.last_received_headers.lock().await = headers;
                    *state.last_received_body.lock().await = body.clone();
                    state.call_count.fetch_add(1, Ordering::SeqCst);

                    let req_id = body.get("id").cloned().unwrap_or(json!(1));
                    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
                    let params = body.get("params").cloned().unwrap_or(Value::Null);
                    let tool_name = params.get("name").and_then(Value::as_str).unwrap_or("");

                    if method != "tools/call" {
                        return (
                            StatusCode::OK,
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "error": { "code": -32601, "message": "Method not found" }
                            })),
                        );
                    }

                    match tool_name {
                        "weather.get_forecast" => (
                            StatusCode::OK,
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {
                                    "content": [
                                        { "type": "text", "text": "{\"temperature\": 24, \"conditions\": \"sunny\"}" }
                                    ],
                                    "isError": false
                                }
                            })),
                        ),
                        "calendar.create_event" => (
                            StatusCode::OK,
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {
                                    "content": [
                                        { "type": "text", "text": "{\"event_id\": \"evt-mcp-123\", \"status\": \"created\"}" }
                                    ],
                                    "_meta": {
                                        "provider_reference": "evt-mcp-123",
                                        "guarantees": { "idempotent": true }
                                    },
                                    "isError": false
                                }
                            })),
                        ),
                        "calendar.create_event.reconcile" => (
                            StatusCode::OK,
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {
                                    "content": [
                                        { "type": "text", "text": "{\"event_id\": \"evt-mcp-123\", \"status\": \"settled\"}" }
                                    ],
                                    "_meta": { "provider_reference": "evt-mcp-123" }
                                }
                            })),
                        ),
                        "leaked.secret" => (
                            StatusCode::OK,
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "result": {
                                    "content": [
                                        { "type": "text", "text": "{\"user\": \"alice\", \"api_key\": \"super-secret-mcp-key-123\", \"password\": \"secret123\"}" }
                                    ],
                                    "isError": false
                                }
                            })),
                        ),
                        "error.tool" => (
                            StatusCode::OK,
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "error": {
                                    "code": -32000,
                                    "message": "Custom MCP tool error"
                                }
                            })),
                        ),
                        _ => (
                            StatusCode::OK,
                            Json(json!({
                                "jsonrpc": "2.0",
                                "id": req_id,
                                "error": { "code": -32602, "message": "Unknown tool" }
                            })),
                        ),
                    }
                }
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/mcp")
}

async fn start_mock_direct_server(state: Arc<MockServerState>) -> String {
    let app = Router::new().route(
        "/direct",
        post({
            let state = Arc::clone(&state);
            move |headers: HeaderMap, Json(body): Json<Value>| {
                let state = Arc::clone(&state);
                async move {
                    *state.last_received_headers.lock().await = headers;
                    *state.last_received_body.lock().await = body.clone();
                    state.call_count.fetch_add(1, Ordering::SeqCst);

                    let action = body.get("action").and_then(Value::as_str).unwrap_or("");

                    match action {
                        "weather.get_forecast" => (
                            StatusCode::OK,
                            Json(json!({
                                "temperature": 24,
                                "conditions": "sunny"
                            })),
                        ),
                        "calendar.create_event" => (
                            StatusCode::OK,
                            Json(json!({
                                "event_id": "evt-direct-123",
                                "status": "created",
                                "provider_reference": "evt-direct-123",
                                "guarantees": { "idempotent": true }
                            })),
                        ),
                        "calendar.create_event.reconcile" => (
                            StatusCode::OK,
                            Json(json!({
                                "event_id": "evt-direct-123",
                                "status": "settled",
                                "provider_reference": "evt-direct-123"
                            })),
                        ),
                        "leaked.secret" => (
                            StatusCode::OK,
                            Json(json!({
                                "user": "alice",
                                "api_key": "super-secret-direct-key-123",
                                "password": "secret123"
                            })),
                        ),
                        "error.tool" => (
                            StatusCode::BAD_REQUEST,
                            Json(json!({
                                "code": "INVALID_ARGUMENT",
                                "message": "Direct API validation error"
                            })),
                        ),
                        _ => (
                            StatusCode::NOT_FOUND,
                            Json(json!({ "code": "NOT_FOUND", "message": "Unknown action" })),
                        ),
                    }
                }
            }
        }),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}/direct")
}

fn create_router() -> ProtocolRouter {
    let mcp = Arc::new(
        McpProtocolAdapter::with_timeout(Duration::from_secs(5)).with_local_endpoints_for_testing(),
    );
    let direct = Arc::new(
        DirectProtocolAdapter::with_timeout(Duration::from_secs(5))
            .with_local_endpoints_for_testing(),
    );
    ProtocolRouter::new(mcp, direct, TEST_SIGNING_SECRET.to_vec())
}

#[tokio::test]
async fn test_protocol_parity_read_and_consequential_scenarios() {
    let mcp_state = MockServerState::new();
    let mcp_url = start_mock_mcp_server(mcp_state.clone()).await;

    let direct_state = MockServerState::new();
    let direct_url = start_mock_direct_server(direct_state.clone()).await;

    let router = create_router();

    // 1. READ CONFORMANCE PARITY: weather.get_forecast
    let mcp_read_endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: mcp_url.clone(),
        protocol: ExtensionProtocol::Mcp,
        capability: ExtensionCapability {
            external_key: "weather.get_forecast".into(),
            display_name: "Get Forecast".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec!["city".into()],
            optional_guarantees: json!({}),
        },
    };

    let direct_read_endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: direct_url.clone(),
        protocol: ExtensionProtocol::Direct,
        capability: ExtensionCapability {
            external_key: "weather.get_forecast".into(),
            display_name: "Get Forecast".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec!["city".into()],
            optional_guarantees: json!({}),
        },
    };

    let read_invocation = ExtensionInvocation {
        capability_key: "weather.get_forecast".into(),
        parameters: json!({ "city": "Bengaluru" }),
        access_context: Value::Null,
        idempotency_key: None,
        execution_id: None,
        required_guarantees: vec![],
    };

    let mcp_read_res = router
        .execute(&mcp_read_endpoint, &read_invocation)
        .await
        .unwrap();
    let direct_read_res = router
        .execute(&direct_read_endpoint, &read_invocation)
        .await
        .unwrap();

    assert_eq!(mcp_read_res.status, ResponseStatus::Success);
    assert_eq!(direct_read_res.status, ResponseStatus::Success);
    assert_eq!(mcp_read_res.data["temperature"], 24);
    assert_eq!(direct_read_res.data["temperature"], 24);
    assert_eq!(mcp_read_res.data["conditions"], "sunny");
    assert_eq!(direct_read_res.data["conditions"], "sunny");

    // 2. CONSEQUENTIAL WRITE PARITY: calendar.create_event
    let mcp_write_endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: mcp_url.clone(),
        protocol: ExtensionProtocol::Mcp,
        capability: ExtensionCapability {
            external_key: "calendar.create_event".into(),
            display_name: "Create Event".into(),
            effect: ExtensionEffect::Write,
            consequential: true,
            data_recipients: vec!["Calendar API".into()],
            access_needs: vec!["summary".into()],
            optional_guarantees: json!({ "idempotent": true }),
        },
    };

    let direct_write_endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: direct_url.clone(),
        protocol: ExtensionProtocol::Direct,
        capability: ExtensionCapability {
            external_key: "calendar.create_event".into(),
            display_name: "Create Event".into(),
            effect: ExtensionEffect::Write,
            consequential: true,
            data_recipients: vec!["Calendar API".into()],
            access_needs: vec!["summary".into()],
            optional_guarantees: json!({ "idempotent": true }),
        },
    };

    let write_invocation = ExtensionInvocation {
        capability_key: "calendar.create_event".into(),
        parameters: json!({ "summary": "Quarterly Planning" }),
        access_context: Value::Null,
        idempotency_key: Some("idemp-par-123".into()),
        execution_id: Some(Uuid::new_v4()),
        required_guarantees: vec!["idempotent".into()],
    };

    let mcp_write_res = router
        .execute(&mcp_write_endpoint, &write_invocation)
        .await
        .unwrap();
    let direct_write_res = router
        .execute(&direct_write_endpoint, &write_invocation)
        .await
        .unwrap();

    assert_eq!(mcp_write_res.status, ResponseStatus::Success);
    assert_eq!(direct_write_res.status, ResponseStatus::Success);
    assert!(mcp_write_res.provider_reference.is_some());
    assert!(direct_write_res.provider_reference.is_some());
    assert_eq!(
        mcp_write_res.guarantees_reported.as_ref().unwrap()["idempotent"],
        true
    );
    assert_eq!(
        direct_write_res.guarantees_reported.as_ref().unwrap()["idempotent"],
        true
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn test_protocol_choice_provides_no_inherent_trust_or_permission() {
    let db = setup_db().await;
    let (context, _) = create_host_context(&db, "user-parity-trust").await;
    let service =
        Arc::new(RemoteExtensionService::new(db.clone()).with_local_endpoints_for_testing());
    let router = Arc::new(create_router());

    // 1. Install MCP extension with consequential capability
    let mcp_ext = service
        .install(
            &context,
            InstallExtensionRequest {
                external_key: "mcp-consequential".into(),
                display_name: "MCP Consequential Ext".into(),
                protocol: ExtensionProtocol::Mcp,
                endpoint_url: "https://mcp.test.example.com".into(),
                operator: ExtensionOperator {
                    operator_id: "op-mcp".into(),
                    operator_name: "Operator MCP".into(),
                    support_email: None,
                    terms_url: None,
                },
                capabilities: vec![ExtensionCapability {
                    external_key: "mcp.mutate".into(),
                    display_name: "Mutate Action".into(),
                    effect: ExtensionEffect::Write,
                    consequential: true,
                    data_recipients: vec![],
                    access_needs: vec![],
                    optional_guarantees: json!({}),
                }],
            },
        )
        .await
        .unwrap();

    // 2. Install Direct extension with consequential capability
    let direct_ext = service
        .install(
            &context,
            InstallExtensionRequest {
                external_key: "direct-consequential".into(),
                display_name: "Direct Consequential Ext".into(),
                protocol: ExtensionProtocol::Direct,
                endpoint_url: "https://direct.test.example.com".into(),
                operator: ExtensionOperator {
                    operator_id: "op-direct".into(),
                    operator_name: "Operator Direct".into(),
                    support_email: None,
                    terms_url: None,
                },
                capabilities: vec![ExtensionCapability {
                    external_key: "direct.mutate".into(),
                    display_name: "Mutate Action".into(),
                    effect: ExtensionEffect::Write,
                    consequential: true,
                    data_recipients: vec![],
                    access_needs: vec![],
                    optional_guarantees: json!({}),
                }],
            },
        )
        .await
        .unwrap();

    // Both are installed, neither has passed conformance or received operator enablement.
    // Testing dispatch via ExtensionExecutionAdapter for MCP:
    let mcp_adapter = ExtensionExecutionAdapter::new(
        service.clone(),
        router.clone(),
        mcp_ext.id,
        context.clone(),
        json!({}),
    );
    use vox_core::execution::{
        AdapterOutcome, AdapterRequest, ExecutionAdapter, ExecutionIdentity,
    };
    let mcp_outcome = mcp_adapter
        .dispatch(AdapterRequest {
            execution_id: Uuid::new_v4(),
            idempotency_key: "idemp-mcp".into(),
            identity: ExecutionIdentity {
                provider_external_key: "mcp-provider".into(),
                model_identifier: "gpt-4o".into(),
                account_reference: "acc".into(),
                connection_id: Uuid::new_v4(),
                price_amount_minor: 0,
                price_currency: "USD".into(),
            },
            capability_external_key: "mcp.mutate".into(),
        })
        .await;

    // Must fail closed with authorization denied
    match mcp_outcome {
        AdapterOutcome::Failed { code } => {
            assert!(code.contains("AUTHORIZATION_DENIED"));
        }
        other => panic!("Expected failed authorization for unapproved MCP, got: {other:?}"),
    }

    // Testing dispatch via ExtensionExecutionAdapter for Direct:
    let direct_adapter = ExtensionExecutionAdapter::new(
        service.clone(),
        router.clone(),
        direct_ext.id,
        context.clone(),
        json!({}),
    );
    let direct_outcome = direct_adapter
        .dispatch(AdapterRequest {
            execution_id: Uuid::new_v4(),
            idempotency_key: "idemp-direct".into(),
            identity: ExecutionIdentity {
                provider_external_key: "direct-provider".into(),
                model_identifier: "gpt-4o".into(),
                account_reference: "acc".into(),
                connection_id: Uuid::new_v4(),
                price_amount_minor: 0,
                price_currency: "USD".into(),
            },
            capability_external_key: "direct.mutate".into(),
        })
        .await;

    // Direct must ALSO fail closed with authorization denied
    match direct_outcome {
        AdapterOutcome::Failed { code } => {
            assert!(code.contains("AUTHORIZATION_DENIED"));
        }
        other => panic!("Expected failed authorization for unapproved Direct, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_unsupported_provider_guarantees_are_visible_and_never_assumed() {
    let router = create_router();

    // Endpoint does NOT declare atomic_refund or synchronous_settlement
    let endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: "http://127.0.0.1:9".into(), // Will not be called
        protocol: ExtensionProtocol::Mcp,
        capability: ExtensionCapability {
            external_key: "payments.charge".into(),
            display_name: "Charge Card".into(),
            effect: ExtensionEffect::Write,
            consequential: true,
            data_recipients: vec![],
            access_needs: vec![],
            optional_guarantees: json!({ "idempotent": true }), // Only idempotent is guaranteed
        },
    };

    // Invocation requiring unsupported guarantee "atomic_refund"
    let invocation = ExtensionInvocation {
        capability_key: "payments.charge".into(),
        parameters: json!({ "amount": 100 }),
        access_context: Value::Null,
        idempotency_key: Some("key-1".into()),
        execution_id: Some(Uuid::new_v4()),
        required_guarantees: vec!["atomic_refund".into()],
    };

    let err = router.execute(&endpoint, &invocation).await.unwrap_err();
    match err {
        AdapterExecutionError::UnsupportedGuarantee(g) => {
            assert!(g.contains("atomic_refund"));
        }
        other => panic!("Expected UnsupportedGuarantee error, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_context_minimization_and_internal_credential_stripping() {
    let mcp_state = MockServerState::new();
    let mcp_url = start_mock_mcp_server(mcp_state.clone()).await;

    let router = create_router();

    let endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: mcp_url,
        protocol: ExtensionProtocol::Mcp,
        capability: ExtensionCapability {
            external_key: "weather.get_forecast".into(),
            display_name: "Get Forecast".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec!["city".into()], // ONLY "city" is granted
            optional_guarantees: json!({}),
        },
    };

    let dirty_parameters = json!({
        "city": "Paris",
        "system_prompt": "You are a helpful AI assistant...",
        "internal_token": "vox_sec_99999999",
        "session_token": "sess_88888888",
        "unrequested_user_email": "user@example.com"
    });

    let invocation = ExtensionInvocation {
        capability_key: "weather.get_forecast".into(),
        parameters: dirty_parameters,
        access_context: json!({ "private_db_url": "postgres://secret" }),
        idempotency_key: None,
        execution_id: None,
        required_guarantees: vec![],
    };

    let res = router.execute(&endpoint, &invocation).await.unwrap();
    assert_eq!(res.status, ResponseStatus::Success);

    // Inspect what was received by the mock server
    let received_body = mcp_state.last_received_body.lock().await;
    let received_args = received_body["params"]["arguments"].as_object().unwrap();

    // 1. "city" was preserved
    assert_eq!(received_args.get("city").unwrap(), "Paris");

    // 2. Sensitive internal tokens and unrequested keys were strictly stripped
    assert!(!received_args.contains_key("system_prompt"));
    assert!(!received_args.contains_key("internal_token"));
    assert!(!received_args.contains_key("session_token"));
    assert!(!received_args.contains_key("unrequested_user_email"));
    assert!(!received_args.contains_key("private_db_url"));
}

#[tokio::test]
async fn test_response_redaction_protects_against_accidental_leaks() {
    let mcp_state = MockServerState::new();
    let mcp_url = start_mock_mcp_server(mcp_state.clone()).await;

    let direct_state = MockServerState::new();
    let direct_url = start_mock_direct_server(direct_state.clone()).await;

    let router = create_router();

    let mcp_endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: mcp_url,
        protocol: ExtensionProtocol::Mcp,
        capability: ExtensionCapability {
            external_key: "leaked.secret".into(),
            display_name: "Leaked".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec![],
            optional_guarantees: json!({}),
        },
    };

    let direct_endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: direct_url,
        protocol: ExtensionProtocol::Direct,
        capability: ExtensionCapability {
            external_key: "leaked.secret".into(),
            display_name: "Leaked".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec![],
            optional_guarantees: json!({}),
        },
    };

    let invocation = ExtensionInvocation {
        capability_key: "leaked.secret".into(),
        parameters: json!({}),
        access_context: Value::Null,
        idempotency_key: None,
        execution_id: None,
        required_guarantees: vec![],
    };

    let mcp_res = router.execute(&mcp_endpoint, &invocation).await.unwrap();
    assert_eq!(mcp_res.data["user"], "alice");
    assert_eq!(mcp_res.data["api_key"], "[REDACTED]");
    assert_eq!(mcp_res.data["password"], "[REDACTED]");

    let direct_res = router.execute(&direct_endpoint, &invocation).await.unwrap();
    assert_eq!(direct_res.data["user"], "alice");
    assert_eq!(direct_res.data["api_key"], "[REDACTED]");
    assert_eq!(direct_res.data["password"], "[REDACTED]");
}

#[tokio::test]
async fn test_adapter_dynamic_disablement_without_semantic_corruption() {
    let direct_state = MockServerState::new();
    let direct_url = start_mock_direct_server(direct_state.clone()).await;

    let mcp_state = MockServerState::new();
    let mcp_url = start_mock_mcp_server(mcp_state.clone()).await;

    let router = create_router();

    let mcp_endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: mcp_url,
        protocol: ExtensionProtocol::Mcp,
        capability: ExtensionCapability {
            external_key: "weather.get_forecast".into(),
            display_name: "Forecast".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec![],
            optional_guarantees: json!({}),
        },
    };

    let direct_endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: direct_url,
        protocol: ExtensionProtocol::Direct,
        capability: ExtensionCapability {
            external_key: "weather.get_forecast".into(),
            display_name: "Forecast".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec![],
            optional_guarantees: json!({}),
        },
    };

    let invocation = ExtensionInvocation {
        capability_key: "weather.get_forecast".into(),
        parameters: json!({}),
        access_context: Value::Null,
        idempotency_key: None,
        execution_id: None,
        required_guarantees: vec![],
    };

    // Both work initially
    assert!(router.execute(&mcp_endpoint, &invocation).await.is_ok());
    assert!(router.execute(&direct_endpoint, &invocation).await.is_ok());

    // Disable MCP adapter
    router.set_adapter_enabled(ExtensionProtocol::Mcp, false);

    // MCP call fails with AdapterDisabled
    let mcp_err = router
        .execute(&mcp_endpoint, &invocation)
        .await
        .unwrap_err();
    assert!(matches!(
        mcp_err,
        AdapterExecutionError::AdapterDisabled(ExtensionProtocol::Mcp)
    ));

    // Direct adapter STILL WORKS perfectly
    let direct_res = router.execute(&direct_endpoint, &invocation).await.unwrap();
    assert_eq!(direct_res.status, ResponseStatus::Success);

    // Re-enable MCP adapter
    router.set_adapter_enabled(ExtensionProtocol::Mcp, true);
    assert!(router.execute(&mcp_endpoint, &invocation).await.is_ok());
}

#[tokio::test]
async fn test_cryptographic_integrity_and_tamper_evidence() {
    let direct_state = MockServerState::new();
    let direct_url = start_mock_direct_server(direct_state.clone()).await;

    let router = create_router();
    let extension_id = Uuid::new_v4();

    let endpoint = AuthorizedEndpoint {
        extension_id,
        endpoint_url: direct_url,
        protocol: ExtensionProtocol::Direct,
        capability: ExtensionCapability {
            external_key: "weather.get_forecast".into(),
            display_name: "Forecast".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec![],
            optional_guarantees: json!({}),
        },
    };

    let invocation = ExtensionInvocation {
        capability_key: "weather.get_forecast".into(),
        parameters: json!({}),
        access_context: Value::Null,
        idempotency_key: None,
        execution_id: None,
        required_guarantees: vec![],
    };

    router.execute(&endpoint, &invocation).await.unwrap();

    let headers = direct_state.last_received_headers.lock().await;
    let signature = headers
        .get("x-vox-extension-assertion")
        .unwrap()
        .to_str()
        .unwrap();
    let timestamp_str = headers
        .get("x-vox-extension-timestamp")
        .unwrap()
        .to_str()
        .unwrap();
    let nonce_str = headers
        .get("x-vox-extension-nonce")
        .unwrap()
        .to_str()
        .unwrap();

    let timestamp_secs: i64 = timestamp_str.parse().unwrap();
    let timestamp = chrono::DateTime::from_timestamp(timestamp_secs, 0).unwrap();
    let nonce = Uuid::parse_str(nonce_str).unwrap();

    let body = direct_state.last_received_body.lock().await;
    let body_bytes = serde_json::to_vec(&*body).unwrap();

    // 1. Verify valid integrity assertion
    let verification = IntegrityVerification {
        secret: TEST_SIGNING_SECRET,
        extension_id,
        capability_key: "weather.get_forecast",
        payload: &body_bytes,
        timestamp,
        nonce,
        signature,
    };
    let verify_res = ExtensionIntegritySigner::verify(&verification, Utc::now(), None);
    assert!(verify_res.is_ok());

    // 2. Tampered payload fails verification
    let tampered_bytes = b"{\"action\":\"hacked\"}";
    let tampered_verification = IntegrityVerification {
        secret: TEST_SIGNING_SECRET,
        extension_id,
        capability_key: "weather.get_forecast",
        payload: tampered_bytes,
        timestamp,
        nonce,
        signature,
    };
    assert!(ExtensionIntegritySigner::verify(&tampered_verification, Utc::now(), None).is_err());
}

#[tokio::test]
async fn test_fuzzing_and_malformed_protocol_resilience() {
    let router = create_router();

    // 1. Invalid JSON response from server
    let malformed_app = Router::new().route(
        "/bad",
        post(|| async { (StatusCode::OK, "this is not json at all <html />") }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, malformed_app).await.unwrap();
    });

    let bad_endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: format!("http://{addr}/bad"),
        protocol: ExtensionProtocol::Mcp,
        capability: ExtensionCapability {
            external_key: "fuzz".into(),
            display_name: "Fuzz".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec![],
            optional_guarantees: json!({}),
        },
    };

    let invocation = ExtensionInvocation {
        capability_key: "fuzz".into(),
        parameters: json!({}),
        access_context: Value::Null,
        idempotency_key: None,
        execution_id: None,
        required_guarantees: vec![],
    };

    let res = router.execute(&bad_endpoint, &invocation).await;
    assert!(
        res.is_err(),
        "Must return ProtocolError on unparseable payload"
    );

    // 2. HTTP 502 Bad Gateway / Upstream failure
    let error_app = Router::new().route(
        "/502",
        post(|| async { (StatusCode::BAD_GATEWAY, "Bad Gateway from upstream proxy") }),
    );
    let listener502 = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr502 = listener502.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener502, error_app).await.unwrap();
    });

    let bad_gateway_endpoint = AuthorizedEndpoint {
        extension_id: Uuid::new_v4(),
        endpoint_url: format!("http://{addr502}/502"),
        protocol: ExtensionProtocol::Direct,
        capability: ExtensionCapability {
            external_key: "fuzz".into(),
            display_name: "Fuzz".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec!["huge".into()],
            optional_guarantees: json!({}),
        },
    };

    let resp = router
        .execute(&bad_gateway_endpoint, &invocation)
        .await
        .unwrap();
    assert_eq!(resp.status, ResponseStatus::ProviderError);
    assert_eq!(resp.error_code.unwrap(), "HTTP_502");

    // 3. Oversized payload rejection (> 2MB limit)
    let huge_data = "a".repeat(3 * 1024 * 1024);
    let huge_invocation = ExtensionInvocation {
        capability_key: "fuzz".into(),
        parameters: json!({ "huge": huge_data }),
        access_context: Value::Null,
        idempotency_key: None,
        execution_id: None,
        required_guarantees: vec![],
    };
    let huge_res = router
        .execute(&bad_gateway_endpoint, &huge_invocation)
        .await;
    assert!(matches!(
        huge_res,
        Err(AdapterExecutionError::InvalidPayload(_))
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn test_consequential_dispatch_succeeds_after_conformance_and_operator_enablement() {
    let db = setup_db().await;
    let (context, _) = create_host_context(&db, "user-consequential-success").await;
    let service =
        Arc::new(RemoteExtensionService::new(db.clone()).with_local_endpoints_for_testing());

    let mcp_state = MockServerState::new();
    let mcp_url = start_mock_mcp_server(mcp_state.clone()).await;

    let direct_state = MockServerState::new();
    let direct_url = start_mock_direct_server(direct_state.clone()).await;

    let router = Arc::new(create_router());

    // 1. Install MCP extension
    let mcp_ext = service
        .install(
            &context,
            InstallExtensionRequest {
                external_key: "mcp-calendar-ext".into(),
                display_name: "MCP Calendar".into(),
                protocol: ExtensionProtocol::Mcp,
                endpoint_url: mcp_url,
                operator: ExtensionOperator {
                    operator_id: "op-cal-mcp".into(),
                    operator_name: "Calendar Inc".into(),
                    support_email: None,
                    terms_url: None,
                },
                capabilities: vec![ExtensionCapability {
                    external_key: "calendar.create_event".into(),
                    display_name: "Create Event".into(),
                    effect: ExtensionEffect::Write,
                    consequential: true,
                    data_recipients: vec![],
                    access_needs: vec!["summary".into()],
                    optional_guarantees: json!({ "idempotent": true }),
                }],
            },
        )
        .await
        .unwrap();

    // Enable operator and record conformance passed
    service
        .set_operator_enabled(&context, mcp_ext.id, true)
        .await
        .unwrap();
    service
        .record_conformance(&context, mcp_ext.id, 1, true, json!({"passed": true}))
        .await
        .unwrap();

    // Dispatch via ExtensionExecutionAdapter
    let mcp_adapter = ExtensionExecutionAdapter::new(
        service.clone(),
        router.clone(),
        mcp_ext.id,
        context.clone(),
        json!({ "summary": "Team Sync" }),
    );
    use vox_core::execution::{
        AdapterOutcome, AdapterRequest, ExecutionAdapter, ExecutionIdentity,
    };
    let mcp_outcome = mcp_adapter
        .dispatch(AdapterRequest {
            execution_id: Uuid::new_v4(),
            idempotency_key: "idemp-mcp-ok".into(),
            identity: ExecutionIdentity {
                provider_external_key: "mcp-provider".into(),
                model_identifier: "gpt-4o".into(),
                account_reference: "acc".into(),
                connection_id: Uuid::new_v4(),
                price_amount_minor: 0,
                price_currency: "USD".into(),
            },
            capability_external_key: "calendar.create_event".into(),
        })
        .await;

    match mcp_outcome {
        AdapterOutcome::Succeeded {
            provider_reference,
            evidence,
        } => {
            assert_eq!(provider_reference, "evt-mcp-123");
            assert_eq!(evidence["status"], "created");
        }
        other => panic!("Expected Succeeded outcome for conformant MCP, got: {other:?}"),
    }

    // 2. Install Direct extension
    let direct_ext = service
        .install(
            &context,
            InstallExtensionRequest {
                external_key: "direct-calendar-ext".into(),
                display_name: "Direct Calendar".into(),
                protocol: ExtensionProtocol::Direct,
                endpoint_url: direct_url,
                operator: ExtensionOperator {
                    operator_id: "op-cal-direct".into(),
                    operator_name: "Calendar Inc".into(),
                    support_email: None,
                    terms_url: None,
                },
                capabilities: vec![ExtensionCapability {
                    external_key: "calendar.create_event".into(),
                    display_name: "Create Event".into(),
                    effect: ExtensionEffect::Write,
                    consequential: true,
                    data_recipients: vec![],
                    access_needs: vec!["summary".into()],
                    optional_guarantees: json!({ "idempotent": true }),
                }],
            },
        )
        .await
        .unwrap();

    // Enable operator and record conformance passed
    service
        .set_operator_enabled(&context, direct_ext.id, true)
        .await
        .unwrap();
    service
        .record_conformance(&context, direct_ext.id, 1, true, json!({"passed": true}))
        .await
        .unwrap();

    // Dispatch via ExtensionExecutionAdapter
    let direct_adapter = ExtensionExecutionAdapter::new(
        service.clone(),
        router.clone(),
        direct_ext.id,
        context.clone(),
        json!({ "summary": "Team Sync" }),
    );
    let direct_outcome = direct_adapter
        .dispatch(AdapterRequest {
            execution_id: Uuid::new_v4(),
            idempotency_key: "idemp-direct-ok".into(),
            identity: ExecutionIdentity {
                provider_external_key: "direct-provider".into(),
                model_identifier: "gpt-4o".into(),
                account_reference: "acc".into(),
                connection_id: Uuid::new_v4(),
                price_amount_minor: 0,
                price_currency: "USD".into(),
            },
            capability_external_key: "calendar.create_event".into(),
        })
        .await;

    match direct_outcome {
        AdapterOutcome::Succeeded {
            provider_reference,
            evidence,
        } => {
            assert_eq!(provider_reference, "evt-direct-123");
            assert_eq!(evidence["status"], "created");
        }
        other => panic!("Expected Succeeded outcome for conformant Direct, got: {other:?}"),
    }

    // 3. Test reconciliation on both
    let mcp_reconciled = mcp_adapter
        .reconcile(
            AdapterRequest {
                execution_id: Uuid::new_v4(),
                idempotency_key: "idemp-mcp-ok".into(),
                identity: ExecutionIdentity {
                    provider_external_key: "mcp-provider".into(),
                    model_identifier: "gpt-4o".into(),
                    account_reference: "acc".into(),
                    connection_id: Uuid::new_v4(),
                    price_amount_minor: 0,
                    price_currency: "USD".into(),
                },
                capability_external_key: "calendar.create_event".into(),
            },
            Some("evt-mcp-123"),
        )
        .await;
    match mcp_reconciled {
        AdapterOutcome::Succeeded {
            provider_reference,
            evidence,
        } => {
            assert_eq!(provider_reference, "evt-mcp-123");
            assert_eq!(evidence["status"], "settled");
        }
        other => panic!("Expected Succeeded outcome for MCP reconcile, got: {other:?}"),
    }

    let direct_reconciled = direct_adapter
        .reconcile(
            AdapterRequest {
                execution_id: Uuid::new_v4(),
                idempotency_key: "idemp-direct-ok".into(),
                identity: ExecutionIdentity {
                    provider_external_key: "direct-provider".into(),
                    model_identifier: "gpt-4o".into(),
                    account_reference: "acc".into(),
                    connection_id: Uuid::new_v4(),
                    price_amount_minor: 0,
                    price_currency: "USD".into(),
                },
                capability_external_key: "calendar.create_event".into(),
            },
            Some("evt-direct-123"),
        )
        .await;
    match direct_reconciled {
        AdapterOutcome::Succeeded {
            provider_reference,
            evidence,
        } => {
            assert_eq!(provider_reference, "evt-direct-123");
            assert_eq!(evidence["status"], "settled");
        }
        other => panic!("Expected Succeeded outcome for Direct reconcile, got: {other:?}"),
    }
}
