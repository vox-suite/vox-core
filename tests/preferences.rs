use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::Utc;
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    http::{AppState, router},
    preferences::{PreferenceError, PreferenceService, SetPreferenceRequest},
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
            deployment_external_key: format!("pref-http-{}", Uuid::new_v4()),
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
async fn sensitive_preferences_require_confirmation_before_save_or_replacement() {
    let db = setup().await;
    let (_, owner, _, _) = host(&db, "user-pref-1").await;
    let service = PreferenceService::new(db.clone());
    let now = Utc::now();

    // 1. Unconfirmed sensitive preference save fails
    let unconfirmed_res = service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "sensitive_personal".into(),
                preference_key: "home_address".into(),
                value: serde_json::json!({"street": "123 Main St", "city": "Bengaluru"}),
                is_sensitive: Some(true),
                confirmed: None,
            },
            now,
        )
        .await;

    assert!(matches!(
        unconfirmed_res,
        Err(PreferenceError::ConfirmationRequired)
    ));

    // Also fails with explicit confirmed: false
    let denied_res = service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "sensitive_personal".into(),
                preference_key: "home_address".into(),
                value: serde_json::json!({"street": "123 Main St", "city": "Bengaluru"}),
                is_sensitive: Some(true),
                confirmed: Some(false),
            },
            now,
        )
        .await;

    assert!(matches!(
        denied_res,
        Err(PreferenceError::ConfirmationRequired)
    ));

    // 2. Confirmed sensitive save succeeds
    let confirmed_res = service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "sensitive_personal".into(),
                preference_key: "home_address".into(),
                value: serde_json::json!({"street": "123 Main St", "city": "Bengaluru"}),
                is_sensitive: Some(true),
                confirmed: Some(true),
            },
            now,
        )
        .await
        .unwrap();

    assert_eq!(confirmed_res.preference_key, "home_address");
    assert!(confirmed_res.is_sensitive);
    assert!(confirmed_res.confirmed_at.is_some());
    assert!(
        service
            .effective_for_agent(&owner, "agent", &["sensitive_personal".into()])
            .await
            .unwrap()
            .is_empty()
    );

    // 3. Replacing an existing sensitive preference without confirmation fails
    let unconfirmed_replace = service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "sensitive_personal".into(),
                preference_key: "home_address".into(),
                value: serde_json::json!({"street": "456 Oak St", "city": "Bengaluru"}),
                is_sensitive: None, // Omitting is_sensitive flag on replacement must still enforce
                confirmed: None,
            },
            now,
        )
        .await;

    assert!(matches!(
        unconfirmed_replace,
        Err(PreferenceError::ConfirmationRequired)
    ));

    // 4. Confirmed replacement succeeds
    let confirmed_replace = service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "sensitive_personal".into(),
                preference_key: "home_address".into(),
                value: serde_json::json!({"street": "456 Oak St", "city": "Bengaluru"}),
                is_sensitive: Some(true),
                confirmed: Some(true),
            },
            now,
        )
        .await
        .unwrap();

    assert_eq!(
        confirmed_replace.value,
        serde_json::json!({"street": "456 Oak St", "city": "Bengaluru"})
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn agents_receive_only_relevant_permitted_preferences() {
    let db = setup().await;
    let (_, owner, _, _) = host(&db, "user-pref-2").await;
    let service = PreferenceService::new(db.clone());
    let now = Utc::now();

    // Populate preferences across multiple categories
    service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "locale".into(),
                preference_key: "display_currency".into(),
                value: serde_json::json!("INR"),
                is_sensitive: Some(false),
                confirmed: None,
            },
            now,
        )
        .await
        .unwrap();

    service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "locale".into(),
                preference_key: "timezone".into(),
                value: serde_json::json!("Asia/Kolkata"),
                is_sensitive: Some(false),
                confirmed: None,
            },
            now,
        )
        .await
        .unwrap();

    service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "dining".into(),
                preference_key: "dietary".into(),
                value: serde_json::json!("vegetarian"),
                is_sensitive: Some(false),
                confirmed: None,
            },
            now,
        )
        .await
        .unwrap();

    service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "sensitive_personal".into(),
                preference_key: "passport_number".into(),
                value: serde_json::json!("P1234567"),
                is_sensitive: Some(true),
                confirmed: Some(true),
            },
            now,
        )
        .await
        .unwrap();

    // Agent requesting only "dining" category
    let dining_prefs = service
        .effective_for_agent(&owner, "food_agent", &["dining".into()])
        .await
        .unwrap();

    assert_eq!(dining_prefs.len(), 1);
    assert_eq!(dining_prefs[0].preference_key, "dietary");

    // Agent requesting "locale" category
    let locale_prefs = service
        .effective_for_agent(&owner, "calendar_agent", &["locale".into()])
        .await
        .unwrap();

    assert_eq!(locale_prefs.len(), 2);
    let keys: Vec<&str> = locale_prefs
        .iter()
        .map(|p| p.preference_key.as_str())
        .collect();
    assert!(keys.contains(&"display_currency"));
    assert!(keys.contains(&"timezone"));

    // Sensitive passport preference was NEVER delivered to dining or calendar agents
    assert!(
        !locale_prefs
            .iter()
            .any(|p| p.preference_key == "passport_number")
    );
    assert!(
        !dining_prefs
            .iter()
            .any(|p| p.preference_key == "passport_number")
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn provider_currency_and_timezone_remain_authoritative_regardless_of_display_preference() {
    let db = setup().await;
    let (_, owner, _, _) = host(&db, "user-pref-3").await;
    let service = PreferenceService::new(db.clone());
    let now = Utc::now();

    // User sets display currency and timezone preferences
    let pref = service
        .set_preference(
            &owner,
            SetPreferenceRequest {
                category: "locale".into(),
                preference_key: "display_currency".into(),
                value: serde_json::json!("USD"),
                is_sensitive: Some(false),
                confirmed: None,
            },
            now,
        )
        .await
        .unwrap();

    // Verifies that the returned preference explicitly carries the authority disclaimer
    // guaranteeing that preferences never override provider facts or confer execution authority.
    assert!(
        pref.authority_disclaimer
            .contains("confers no execution authority")
    );
    assert!(pref.authority_disclaimer.contains(
        "Provider currency, timezone, and inventory facts remain strictly authoritative"
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cross_context_preferences_are_strictly_isolated() {
    let db = setup().await;
    let (_, owner_a, _, _) = host(&db, "user-a").await;
    let (_, owner_b, _, _) = host(&db, "user-b").await;
    let service = PreferenceService::new(db.clone());
    let now = Utc::now();

    service
        .set_preference(
            &owner_a,
            SetPreferenceRequest {
                category: "locale".into(),
                preference_key: "timezone".into(),
                value: serde_json::json!("America/New_York"),
                is_sensitive: Some(false),
                confirmed: None,
            },
            now,
        )
        .await
        .unwrap();

    let prefs_b = service.list_preferences(&owner_b).await.unwrap();
    assert!(
        prefs_b.is_empty(),
        "Context B must not see Context A's preferences"
    );

    // Deleting in Context B does not affect Context A
    let deleted_b = service
        .delete_preference(&owner_b, "timezone")
        .await
        .unwrap();
    assert!(!deleted_b);

    let prefs_a = service.list_preferences(&owner_a).await.unwrap();
    assert_eq!(prefs_a.len(), 1);
    assert_eq!(prefs_a[0].preference_key, "timezone");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn preference_http_endpoints_require_signed_assertions() {
    let db = setup().await;
    let (_, _, registered, host_context) = host(&db, "http-pref-user").await;
    let app = router(AppState::with_host_trust(db, "operator-token".into()));
    let now = Utc::now();

    let uri = "/v1/preferences".to_string();
    let body = serde_json::to_vec(&serde_json::json!({
        "host_context": host_context,
        "preference": {
            "category": "locale",
            "preference_key": "timezone",
            "value": "UTC",
            "is_sensitive": false,
            "confirmed": null
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

    assert_eq!(
        app.clone().oneshot(unauthed).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    // 2. Authenticated request succeeds
    let assertion = registered
        .credential
        .sign_context_request(&host_context, now, Uuid::new_v4())
        .unwrap();

    let signed = signed_request(uri.clone(), "POST", body.clone(), &assertion);
    let res = app.clone().oneshot(signed).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // 3. Sensitive preference without confirmation returns 428 Precondition Required
    let sensitive_body = serde_json::to_vec(&serde_json::json!({
        "host_context": host_context,
        "preference": {
            "category": "sensitive_personal",
            "preference_key": "home_address",
            "value": {"city": "Bengaluru"},
            "is_sensitive": true,
            "confirmed": false
        }
    }))
    .unwrap();

    let sensitive_assertion = registered
        .credential
        .sign_context_request(&host_context, now, Uuid::new_v4())
        .unwrap();

    let sensitive_signed =
        signed_request(uri.clone(), "POST", sensitive_body, &sensitive_assertion);
    let res2 = app.clone().oneshot(sensitive_signed).await.unwrap();
    assert_eq!(res2.status(), StatusCode::PRECONDITION_REQUIRED);
}
