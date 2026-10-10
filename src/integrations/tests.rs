use super::*;
#[test]
fn pkce_requires_valid_verifier_and_exact_challenge() {
    let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    assert!(verify_pkce(
        v,
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    ));
    assert!(!verify_pkce("short", "anything"));
    assert!(!verify_pkce(v, "wrong"));
}
#[test]
fn scope_requires_explicit_bounded_window() {
    let now = chrono::Utc::now();
    assert!(validate_window(None, None).is_ok());
    assert!(validate_window(Some(now), None).is_err());
    assert!(validate_window(Some(now), Some(now + chrono::Duration::days(366))).is_err());
    assert!(validate_window(Some(now), Some(now - chrono::Duration::days(1))).is_err());
    assert!(validate_window(Some(now), Some(now + chrono::Duration::days(30))).is_ok());
}

#[tokio::test]
#[ignore = "requires dedicated INTEGRATION_TEST_DATABASE_URL with pgvector"]
async fn postgres_grant_boundaries_rotation_revocation_and_concurrent_idempotency() {
    let pool = PgPool::connect(
        &std::env::var("INTEGRATION_TEST_DATABASE_URL").expect("dedicated test database required"),
    )
    .await
    .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let service = IntegrationService {
        pool: pool.clone(),
        spans: SpanService::new(
            crate::storage::spans::SpanRepository::new(pool.clone()),
            crate::realtime::UserEventHub::new(),
        ),
        collections: CollectionService::new(
            crate::storage::collections::CollectionRepository::new(pool.clone()),
        ),
        charts: PulseRepository::new(pool.clone()),
        client_id: "share_to_action".into(),
        redirect_uri: Some("http://localhost/callback".into()),
    };
    async fn user(pool: &PgPool) -> Actor {
        let user = sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO user_contexts(deployment_id,host_app_id,host_user_id,user_id) SELECT d.id,h.id,$1::text,$1 FROM platform_deployments d JOIN host_apps h ON h.deployment_id=d.id WHERE d.external_key='vox.standalone.deployment' AND h.external_key='vox.standalone.web'").bind(user).execute(pool).await.unwrap();
        Actor::user(user)
    }
    let a = user(&pool).await;
    let b = user(&pool).await;
    let own = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO collections(user_id,name) VALUES($1,'selected') RETURNING id",
    )
    .bind(a.user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let other = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO collections(user_id,name) VALUES($1,'private') RETURNING id",
    )
    .bind(b.user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let now = Utc::now();
    let auth = |collection_ids: Vec<Uuid>| Authorization {
        client_id: "share_to_action".into(),
        redirect_uri: "http://localhost/callback".into(),
        state: "state".into(),
        code_challenge: "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM".into(),
        collection_ids,
        chart_ids: vec![],
        span_from: Some(now - Duration::days(1)),
        span_to: Some(now + Duration::days(1)),
        allow_create_plans: true,
    };
    assert!(matches!(
        service.authorize(&a, auth(vec![other])).await,
        Err(Error::Forbidden)
    ));
    assert!(
        service
            .validate_client("share_to_action", "http://localhost/callback/evil")
            .is_err()
    );
    let mut no_config = service.clone();
    no_config.redirect_uri = None;
    assert!(matches!(
        no_config.authorize(&a, auth(vec![])).await,
        Err(Error::Unavailable)
    ));
    let redirect = service.authorize(&a, auth(vec![own])).await.unwrap();
    let code = url::Url::parse(&redirect)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "code")
        .unwrap()
        .1
        .into_owned();
    let exchange = |verifier: &str| TokenRequest {
        client_id: "share_to_action".into(),
        grant_type: None,
        code: Some(code.clone()),
        redirect_uri: Some("http://localhost/callback".into()),
        code_verifier: Some(verifier.into()),
        refresh_token: None,
    };
    assert!(service.token(exchange("wrong")).await.is_err());
    let (first, second) = tokio::join!(
        service.token(exchange("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")),
        service.token(exchange("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"))
    );
    assert_eq!(first.is_ok() as u8 + second.is_ok() as u8, 1);
    let tokens = first.or(second).unwrap();
    sqlx::query("UPDATE integration_tokens SET access_expires_at=now()-interval '1 second' WHERE access_hash=$1").bind(hash(&tokens.access_token)).execute(&pool).await.unwrap();
    assert!(service.authenticate(&tokens.access_token).await.is_err());
    sqlx::query("UPDATE integration_tokens SET access_expires_at=now()+interval '1 hour' WHERE access_hash=$1").bind(hash(&tokens.access_token)).execute(&pool).await.unwrap();
    let grant = service.authenticate(&tokens.access_token).await.unwrap();
    assert_eq!(grant.user_id, a.user_id);
    let request_id = Uuid::new_v4();
    let source_id = Uuid::new_v4();
    let plan = || Plan {
        request_id,
        title: "real saved plan".into(),
        notes: None,
        starts_at: Some(now),
        ends_at: None,
        due_at: None,
        source_url: Some("https://example.org/original".into()),
        source_id,
        collection_id: Some(own),
    };
    let mut denied = plan();
    denied.collection_id = Some(other);
    assert!(matches!(
        service.plan(&grant, denied).await,
        Err(Error::Forbidden)
    ));
    let mut no_create = grant.clone();
    no_create.allow_create_plans = false;
    assert!(matches!(
        service.plan(&no_create, plan()).await,
        Err(Error::Forbidden)
    ));
    let (p1, p2) = tokio::join!(service.plan(&grant, plan()), service.plan(&grant, plan()));
    let id = p1.unwrap();
    assert_eq!(id, p2.unwrap());
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM spans WHERE user_id=$1 AND source='share_to_action'",
    )
    .bind(a.user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let mut changed = plan();
    changed.title = "different".into();
    assert!(service.plan(&grant, changed).await.is_err());
    let stored = service.spans.get_span(&a, id).await.unwrap().unwrap();
    assert_eq!(stored.data["source_id"], source_id.to_string());
    assert_eq!(stored.data["source_url"], "https://example.org/original");
    service
        .spans
        .create_span(
            &a,
            NewSpan {
                title: "outside".into(),
                start_at: Some(now - Duration::days(5)),
                collection_ids: vec![own],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    service
        .spans
        .create_span(
            &b,
            NewSpan {
                title: "other user".into(),
                start_at: Some(now),
                collection_ids: vec![other],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let context = service.context(&grant).await.unwrap();
    assert_eq!(context["spans"].as_array().unwrap().len(), 1);
    assert_eq!(context["collections"].as_array().unwrap().len(), 1);
    sqlx::query("INSERT INTO timeline_events(user_id,event_type_id,group_id,title,occurred_at,content) SELECT $1,id,group_id,'Metric', $2, '{\"amount\":1.5,\"currency\":\"INR\",\"direction\":\"debit\",\"is_spending\":true}'::jsonb FROM timeline_event_types WHERE value='transaction' AND owner_user_id IS NULL")
        .bind(a.user_id).bind(now).execute(&pool).await.unwrap();
    let inventory = service.charts.inventory(a.user_id).await.unwrap();
    let catalog = crate::application::pulse::measurements::measurement_catalog(&inventory.profiles);
    let measurement = catalog.iter().find(|m| m.kind == crate::domain::pulse::MeasurementKind::NumericSum).unwrap();
    let definition = crate::domain::pulse::PulseDefinition {
        version: 2, measurement_id: measurement.id.clone(), bucket: Some(crate::domain::pulse::Bucket::Day),
        dimension: None, period_days: 1, offset_days: 0, top_n: None,
        timezone: "UTC".into(), chart_type: crate::domain::charts::ChartType::Bar,
    };
    let input = crate::domain::pulse::SavePulseInput { title: "Metric".into(), definition, idempotency_key: Uuid::new_v4() };
    let chart = service.charts.save(a.user_id,&input,"","").await.unwrap();
    let other_chart = service.charts.save(b.user_id,&input,"","").await.unwrap();
    let mut invalid_chart = auth(vec![]);
    invalid_chart.chart_ids = vec![other_chart.id];
    assert!(matches!(service.authorize(&a,invalid_chart).await,Err(Error::Forbidden)));
    let mut pulse_grant = grant.clone();
    pulse_grant.collection_ids = vec![];
    pulse_grant.chart_ids = vec![chart.id];
    let pulse = service.context(&pulse_grant).await.unwrap();
    assert_eq!(pulse["spans"].as_array().unwrap().len(), 0);
    assert_eq!(pulse["pulse"][0]["data"]["total"],1.5);
    let mut no_reads = grant.clone();
    no_reads.collection_ids = vec![];
    no_reads.chart_ids = vec![];
    let no_context = service.context(&no_reads).await.unwrap();
    assert_eq!(
        no_context,
        serde_json::json!({"collections":[],"spans":[],"pulse":[]})
    );
    assert!(
        service
            .authenticate("ordinary-session-token")
            .await
            .is_err()
    );
    let api = super::http::public_router(service.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
    let client = reqwest::Client::new();
    let unauth = client
        .get(format!("http://{address}/v1/integrations/context"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 403);
    let delegated = client
        .get(format!("http://{address}/v1/integrations/context"))
        .bearer_auth(&tokens.access_token)
        .send()
        .await
        .unwrap();
    assert_eq!(delegated.status(), 200);
    let general = client
        .post(format!("http://{address}/v1/spans"))
        .bearer_auth(&tokens.access_token)
        .json(&serde_json::json!({"title":"no"}))
        .send()
        .await
        .unwrap();
    assert_eq!(general.status(), 404);
    server.abort();
    let empty = service.authorize(&a, auth(vec![])).await.unwrap();
    assert!(empty.contains("code="));
    let expired_code = url::Url::parse(&empty)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "code")
        .unwrap()
        .1
        .into_owned();
    sqlx::query(
        "UPDATE integration_codes SET expires_at=now()-interval '1 second' WHERE code_hash=$1",
    )
    .bind(hash(&expired_code))
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        service
            .token(TokenRequest {
                client_id: "share_to_action".into(),
                grant_type: None,
                code: Some(expired_code),
                redirect_uri: Some("http://localhost/callback".into()),
                code_verifier: Some("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into()),
                refresh_token: None
            })
            .await
            .is_err()
    );
    let refresh = || TokenRequest {
        client_id: "share_to_action".into(),
        grant_type: Some("refresh_token".into()),
        code: None,
        redirect_uri: None,
        code_verifier: None,
        refresh_token: Some(tokens.refresh_token.clone()),
    };
    sqlx::query("UPDATE integration_tokens SET refresh_expires_at=now()-interval '1 second' WHERE refresh_hash=$1").bind(hash(&tokens.refresh_token)).execute(&pool).await.unwrap();
    assert!(service.token(refresh()).await.is_err());
    sqlx::query("UPDATE integration_tokens SET refresh_expires_at=now()+interval '30 days' WHERE refresh_hash=$1").bind(hash(&tokens.refresh_token)).execute(&pool).await.unwrap();
    let (r1, r2) = tokio::join!(service.token(refresh()), service.token(refresh()));
    assert_eq!(r1.is_ok() as u8 + r2.is_ok() as u8, 1);
    let rotated = r1.or(r2).unwrap();
    assert!(service.authenticate(&tokens.access_token).await.is_err());
    let rotated_grant = service.authenticate(&rotated.access_token).await.unwrap();
    service.revoke(&rotated_grant).await.unwrap();
    assert!(service.authenticate(&rotated.access_token).await.is_err());
    assert!(
        service
            .token(TokenRequest {
                refresh_token: Some(rotated.refresh_token),
                ..refresh()
            })
            .await
            .is_err()
    );
    assert!(service.plan(&rotated_grant, plan()).await.is_err());
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(vec![a.user_id, b.user_id])
        .execute(&pool)
        .await
        .unwrap();
}
