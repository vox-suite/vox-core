/**
* Integration tests for remote extension governance, conformance, sandboxing, and renewed consent (E29).
*/
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    remote_extensions::{
        ConformanceStatus, ConsentStatus, ExtensionCapability, ExtensionEffect, ExtensionOperator,
        ExtensionProtocol, InstallExtensionRequest, LifecycleState, RemoteExtensionError,
        RemoteExtensionService, UpdateExtensionRequest,
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
    (
        registered.deployment_external_key.clone(),
        resolved,
        registered,
        request,
    )
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn installation_grants_no_authority_or_connections() {
    let db = setup().await;
    let (_, context, _, _) = host(&db, "user-install").await;
    let service = RemoteExtensionService::new(db.clone());

    let request = InstallExtensionRequest {
        external_key: "weather-service".into(),
        display_name: "Weather Updates".into(),
        protocol: ExtensionProtocol::Mcp,
        endpoint_url: "https://mcp.weather.example.com/sse".into(),
        operator: ExtensionOperator {
            operator_id: "weather-corp".into(),
            operator_name: "Weather Corp Inc.".into(),
            support_email: Some("support@weather.example.com".into()),
            terms_url: Some("https://weather.example.com/terms".into()),
        },
        capabilities: vec![ExtensionCapability {
            external_key: "weather.get_forecast".into(),
            display_name: "Get Forecast".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec!["Weather Corp Cloud".into()],
            access_needs: vec!["location".into()],
            optional_guarantees: json!({}),
        }],
    };

    let extension = service.install(&context, request).await.unwrap();

    // 1. Initial state is 'installed', operator_enabled is false, conformance is pending
    assert_eq!(extension.lifecycle_state, LifecycleState::Installed);
    assert!(!extension.operator_enabled);
    assert_eq!(extension.conformance_status, ConformanceStatus::Pending);
    assert_eq!(extension.consent_status, ConsentStatus::Consented);

    // 2. Authorize call fails because installation grants no capability authority
    let err = service
        .authorize_call(&context, extension.id, "weather.get_forecast")
        .await
        .unwrap_err();
    match err {
        RemoteExtensionError::NotActive(state) => {
            assert_eq!(state, LifecycleState::Installed);
        }
        other => panic!("Expected NotActive(Installed), got: {other:?}"),
    }

    // 3. No external connection is granted or created
    let connection_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM external_connections WHERE user_context_id = $1",
    )
    .bind(context.id.0)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(connection_count, 0);

    // 4. No capability grant is created
    let grant_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM agent_capability_grants WHERE user_context_id = $1",
    )
    .bind(context.id.0)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(grant_count, 0);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn consequential_capabilities_require_conformance_and_operator_enablement() {
    let db = setup().await;
    let (_, context, _, _) = host(&db, "user-consequential").await;
    let service = RemoteExtensionService::new(db.clone());

    let request = InstallExtensionRequest {
        external_key: "smart-thermostat".into(),
        display_name: "Smart Thermostat Control".into(),
        protocol: ExtensionProtocol::Direct,
        endpoint_url: "https://api.thermostat.example.com/v1".into(),
        operator: ExtensionOperator {
            operator_id: "thermostat-inc".into(),
            operator_name: "Thermostat Inc.".into(),
            support_email: None,
            terms_url: None,
        },
        capabilities: vec![
            ExtensionCapability {
                external_key: "thermostat.get_temperature".into(),
                display_name: "Get Temperature".into(),
                effect: ExtensionEffect::Read,
                consequential: false,
                data_recipients: vec![],
                access_needs: vec![],
                optional_guarantees: json!({}),
            },
            ExtensionCapability {
                external_key: "thermostat.set_temperature".into(),
                display_name: "Set Temperature".into(),
                effect: ExtensionEffect::Write,
                consequential: true,
                data_recipients: vec!["Thermostat Cloud API".into()],
                access_needs: vec!["device_control".into()],
                optional_guarantees: json!({"idempotent": true}),
            },
        ],
    };

    let ext = service.install(&context, request).await.unwrap();

    // 1. Enabling operator alone without passing conformance does NOT allow consequential action
    service
        .set_operator_enabled(&context, ext.id, true)
        .await
        .unwrap();
    let err = service
        .authorize_call(&context, ext.id, "thermostat.set_temperature")
        .await
        .unwrap_err();
    assert!(matches!(err, RemoteExtensionError::NotActive(_)));

    // 2. Failed conformance does not activate
    service
        .record_conformance(&context, ext.id, 1, false, json!({"error": "timed out"}))
        .await
        .unwrap();
    let err = service
        .authorize_call(&context, ext.id, "thermostat.set_temperature")
        .await
        .unwrap_err();
    assert!(matches!(err, RemoteExtensionError::NotActive(_)));

    // 3. Passing conformance while operator is enabled activates the extension
    service
        .record_conformance(&context, ext.id, 1, true, json!({"status": "passed"}))
        .await
        .unwrap();

    let authorized = service
        .authorize_call(&context, ext.id, "thermostat.set_temperature")
        .await
        .unwrap();

    assert_eq!(authorized.extension_id, ext.id);
    assert_eq!(
        authorized.endpoint_url,
        "https://api.thermostat.example.com/v1"
    );
    assert_eq!(
        authorized.capability.external_key,
        "thermostat.set_temperature"
    );
    assert!(authorized.capability.consequential);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn operator_change_requires_renewed_consent() {
    let db = setup().await;
    let (_, context, _, _) = host(&db, "user-operator-change").await;
    let service = RemoteExtensionService::new(db.clone());

    let request = InstallExtensionRequest {
        external_key: "doc-signer".into(),
        display_name: "Doc Signer".into(),
        protocol: ExtensionProtocol::Mcp,
        endpoint_url: "https://signer.example.com/mcp".into(),
        operator: ExtensionOperator {
            operator_id: "signer-corp-v1".into(),
            operator_name: "Signer Corp".into(),
            support_email: None,
            terms_url: None,
        },
        capabilities: vec![ExtensionCapability {
            external_key: "doc.sign".into(),
            display_name: "Sign Document".into(),
            effect: ExtensionEffect::Write,
            consequential: true,
            data_recipients: vec!["Doc Cloud".into()],
            access_needs: vec![],
            optional_guarantees: json!({}),
        }],
    };

    let ext = service.install(&context, request).await.unwrap();
    service
        .set_operator_enabled(&context, ext.id, true)
        .await
        .unwrap();
    service
        .record_conformance(&context, ext.id, 1, true, json!({"passed": true}))
        .await
        .unwrap();

    // Verify it is active
    assert!(
        service
            .authorize_call(&context, ext.id, "doc.sign")
            .await
            .is_ok()
    );

    // Update with CHANGED operator (operator acquisition or transfer)
    let update = UpdateExtensionRequest {
        endpoint_url: None,
        operator: Some(ExtensionOperator {
            operator_id: "new-conglomerate".into(),
            operator_name: "New Conglomerate LLC".into(),
            support_email: Some("legal@conglomerate.com".into()),
            terms_url: Some("https://conglomerate.com/terms".into()),
        }),
        capabilities: None,
    };

    let updated = service.update(&context, ext.id, update).await.unwrap();
    assert_eq!(updated.current_version, 2);
    assert_eq!(updated.consent_status, ConsentStatus::ConsentRequired);

    // Call MUST be rejected because renewed consent is required
    let err = service
        .authorize_call(&context, ext.id, "doc.sign")
        .await
        .unwrap_err();
    assert!(matches!(err, RemoteExtensionError::NotActive(_)));

    // User explicitly grants renewed consent for version 2
    service.renew_consent(&context, ext.id, 2).await.unwrap();
    // And conformance passes on new version
    service
        .record_conformance(&context, ext.id, 2, true, json!({"passed": true}))
        .await
        .unwrap();

    // Now call succeeds with renewed consent
    let authorized = service
        .authorize_call(&context, ext.id, "doc.sign")
        .await
        .unwrap();
    assert_eq!(authorized.extension_id, ext.id);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn expanded_data_recipients_require_renewed_consent() {
    let db = setup().await;
    let (_, context, _, _) = host(&db, "user-recipients-change").await;
    let service = RemoteExtensionService::new(db.clone());

    let request = InstallExtensionRequest {
        external_key: "crm-sync".into(),
        display_name: "CRM Synchronizer".into(),
        protocol: ExtensionProtocol::Direct,
        endpoint_url: "https://crm.example.com/api".into(),
        operator: ExtensionOperator {
            operator_id: "crm-tools".into(),
            operator_name: "CRM Tools Inc.".into(),
            support_email: None,
            terms_url: None,
        },
        capabilities: vec![ExtensionCapability {
            external_key: "crm.sync_contacts".into(),
            display_name: "Sync Contacts".into(),
            effect: ExtensionEffect::Mixed,
            consequential: true,
            data_recipients: vec!["Internal CRM Server".into()],
            access_needs: vec!["contacts".into()],
            optional_guarantees: json!({}),
        }],
    };

    let ext = service.install(&context, request).await.unwrap();
    service
        .set_operator_enabled(&context, ext.id, true)
        .await
        .unwrap();
    service
        .record_conformance(&context, ext.id, 1, true, json!({"passed": true}))
        .await
        .unwrap();

    assert!(
        service
            .authorize_call(&context, ext.id, "crm.sync_contacts")
            .await
            .is_ok()
    );

    // Update adding a NEW third-party data recipient (e.g. ad network or analytics broker)
    let update = UpdateExtensionRequest {
        endpoint_url: None,
        operator: None,
        capabilities: Some(vec![ExtensionCapability {
            external_key: "crm.sync_contacts".into(),
            display_name: "Sync Contacts".into(),
            effect: ExtensionEffect::Mixed,
            consequential: true,
            data_recipients: vec![
                "Internal CRM Server".into(),
                "Third-Party Marketing Analytics Broker".into(), // Expanded recipient!
            ],
            access_needs: vec!["contacts".into()],
            optional_guarantees: json!({}),
        }]),
    };

    let updated = service.update(&context, ext.id, update).await.unwrap();
    assert_eq!(updated.consent_status, ConsentStatus::ConsentRequired);

    // Call MUST be rejected
    let err = service
        .authorize_call(&context, ext.id, "crm.sync_contacts")
        .await
        .unwrap_err();
    assert!(matches!(err, RemoteExtensionError::NotActive(_)));

    // Renew consent
    service.renew_consent(&context, ext.id, 2).await.unwrap();
    service
        .record_conformance(&context, ext.id, 2, true, json!({"passed": true}))
        .await
        .unwrap();

    assert!(
        service
            .authorize_call(&context, ext.id, "crm.sync_contacts")
            .await
            .is_ok()
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn quarantine_and_removal_preserve_historical_evidence() {
    let db = setup().await;
    let (_, context, _, _) = host(&db, "user-quarantine").await;
    let service = RemoteExtensionService::new(db.clone());

    let request = InstallExtensionRequest {
        external_key: "suspicious-ext".into(),
        display_name: "Suspicious Extension".into(),
        protocol: ExtensionProtocol::Mcp,
        endpoint_url: "https://suspicious.example.com/sse".into(),
        operator: ExtensionOperator {
            operator_id: "suspicious-org".into(),
            operator_name: "Suspicious Org".into(),
            support_email: None,
            terms_url: None,
        },
        capabilities: vec![ExtensionCapability {
            external_key: "test.op".into(),
            display_name: "Test Op".into(),
            effect: ExtensionEffect::Read,
            consequential: false,
            data_recipients: vec![],
            access_needs: vec![],
            optional_guarantees: json!({}),
        }],
    };

    let ext = service.install(&context, request).await.unwrap();
    service
        .set_operator_enabled(&context, ext.id, true)
        .await
        .unwrap();
    service
        .record_conformance(&context, ext.id, 1, true, json!({"passed": true}))
        .await
        .unwrap();

    // Quarantine version 1
    let quarantined = service.quarantine(&context, ext.id, 1).await.unwrap();
    assert_eq!(quarantined.lifecycle_state, LifecycleState::Quarantined);
    assert!(matches!(
        service.renew_consent(&context, ext.id, 1).await,
        Err(RemoteExtensionError::Quarantined)
    ));

    let err = service
        .authorize_call(&context, ext.id, "test.op")
        .await
        .unwrap_err();
    assert!(matches!(err, RemoteExtensionError::Quarantined));

    // Remove extension
    let removed = service.remove(&context, ext.id).await.unwrap();
    assert_eq!(removed.lifecycle_state, LifecycleState::Removed);
    assert!(matches!(
        service.renew_consent(&context, ext.id, 1).await,
        Err(RemoteExtensionError::NotActive(LifecycleState::Removed))
    ));

    // Records are preserved in DB
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM remote_extension_versions WHERE extension_id = $1",
    )
    .bind(ext.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);

    let run_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM remote_extension_conformance_runs WHERE extension_id = $1",
    )
    .bind(ext.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(run_count, 1);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn rejects_invalid_urls_and_local_code_uploads() {
    let db = setup().await;
    let (_, context, _, _) = host(&db, "user-malicious").await;
    let service = RemoteExtensionService::new(db.clone());

    // Non-HTTP/HTTPS schemes
    for invalid_url in [
        "file:///etc/passwd",
        "ftp://remote.example.com",
        "javascript:alert(1)",
        "data:text/html,<html>",
        "http://insecure-remote.example.com",
        "https://127.0.0.1/mcp",
        "https://10.0.0.4/mcp",
        "https://[::1]/mcp",
        "https://metadata.internal/mcp",
        "https://user:password@example.com/mcp",
    ] {
        let request = InstallExtensionRequest {
            external_key: "bad-url".into(),
            display_name: "Bad Url".into(),
            protocol: ExtensionProtocol::Mcp,
            endpoint_url: invalid_url.into(),
            operator: ExtensionOperator {
                operator_id: "op".into(),
                operator_name: "Op".into(),
                support_email: None,
                terms_url: None,
            },
            capabilities: vec![],
        };
        let res = service.install(&context, request).await;
        assert!(res.is_err(), "Expected error for URL: {invalid_url}");
    }
}

fn signed_request(
    uri: String,
    method: &str,
    body: Vec<u8>,
    assertion: &vox_core::host_trust::HostContextAssertion,
) -> Request<Body> {
    Request::builder()
        .method(method)
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

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn remote_extension_http_endpoints_require_signed_assertions() {
    let db = setup().await;
    let (_, _, registered, host_context) = host(&db, "http-ext-user").await;
    let app = vox_core::http::router(vox_core::http::AppState::with_host_trust(
        db,
        "operator-token".into(),
    ));
    let now = Utc::now();

    let uri = "/v1/remote-extensions".to_string();
    let body = serde_json::to_vec(&serde_json::json!({
        "host_context": host_context,
        "extension": {
            "external_key": "http-ext",
            "display_name": "HTTP Extension",
            "protocol": "mcp",
            "endpoint_url": "https://mcp.example.com",
            "operator": {
                "operator_id": "http-op",
                "operator_name": "HTTP Operator"
            },
            "capabilities": []
        }
    }))
    .unwrap();

    // 1. Unauthenticated request rejected
    let unauthed = Request::builder()
        .method("POST")
        .uri(&uri)
        .header("content-type", "application/json")
        .body(Body::from(body.clone()))
        .unwrap();

    let res = app.clone().oneshot(unauthed).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // 2. Authenticated request succeeds
    let assertion = registered
        .credential
        .sign_context_request(&host_context, now, Uuid::new_v4())
        .unwrap();

    let signed = signed_request(uri.clone(), "POST", body.clone(), &assertion);
    let res = app.clone().oneshot(signed).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    // 3. List extensions with fresh signed assertion
    let list_uri = "/v1/remote-extensions/list".to_string();
    let list_body = serde_json::to_vec(&serde_json::json!({
        "host_context": host_context
    }))
    .unwrap();
    let list_assertion = registered
        .credential
        .sign_context_request(&host_context, now, Uuid::new_v4())
        .unwrap();
    let list_signed = signed_request(list_uri, "POST", list_body, &list_assertion);
    let list_res = app.clone().oneshot(list_signed).await.unwrap();
    assert_eq!(list_res.status(), StatusCode::OK);
}
#[tokio::test]
async fn host_cannot_certify_or_enable_its_own_remote_extension() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let app = vox_core::http::router(vox_core::http::AppState::new(false));
    for (path, body) in [
        (
            "/v1/remote-extensions/00000000-0000-0000-0000-000000000001/enable",
            serde_json::json!({"host_context":{"host_user_id":"user"},"enabled":true}),
        ),
        (
            "/v1/remote-extensions/00000000-0000-0000-0000-000000000001/conformance",
            serde_json::json!({"host_context":{"host_user_id":"user"},"version":1,"passed":true,"report":{}}),
        ),
        (
            "/v1/remote-extensions/00000000-0000-0000-0000-000000000001/quarantine",
            serde_json::json!({"host_context":{"host_user_id":"user"},"version":1}),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    let renewal = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/remote-extensions/00000000-0000-0000-0000-000000000001/renew-consent")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"host_context":{"host_user_id":"user"},"version":2})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renewal.status(), StatusCode::SERVICE_UNAVAILABLE);
}
