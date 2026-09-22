use sqlx::Row;
use vox_core::db::Db;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn migration_creates_the_complete_core_schema() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url)
        .await
        .expect("connect to test database");
    db.migrate().await.expect("run migrations");

    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .expect("inspect test database");
    let rows = sqlx::query(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public' ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .expect("list public tables");
    let names: Vec<String> = rows.iter().map(|row| row.get("table_name")).collect();

    for expected in [
        "execution_attempts",
        "audit_events",
        "audit_sink_definitions",
        "audit_sink_deliveries",
        "executions",
        "conversation_summaries",
        "conversations",
        "events",
        "jobs",
        "messages",
        "host_apps",
        "host_app_assertion_nonces",
        "host_app_credentials",
        "host_organizations",
        "identity_adapters",
        "identity_authentication_sessions",
        "identity_link_events",
        "identity_links",
        "agent_definitions",
        "agent_model_configurations",
        "deployment_agent_selections",
        "integration_definitions",
        "integration_capability_declarations",
        "external_connections",
        "agent_capability_grants",
        "action_proposals",
        "action_approvals",
        "spending_policies",
        "operational_quotas",
        "operational_quota_reservations",
        "execution_policy_decisions",
        "task_runs",
        "login_identities",
        "federated_identity_nonces",
        "passwordless_recovery_challenges",
        "platform_deployments",
        "scheduled_tasks",
        "outbound_calls",
        "user_contexts",
        "user_contact_points",
        "user_profiles",
        "users",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing {expected}"
        );
    }

    let scoped_resources: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND column_name = 'user_context_id' \
         ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .expect("list context-owned resources");
    for expected in [
        "executions",
        "conversations",
        "events",
        "scheduled_tasks",
        "tasks",
        "outbound_calls",
        "user_contact_points",
    ] {
        assert!(
            scoped_resources.iter().any(|table| table == expected),
            "{expected} is missing canonical user-context ownership"
        );
    }

    let nullable_authority_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name = ANY($1) AND column_name='user_context_id' AND is_nullable='YES'",
    ).bind(vec!["conversations", "events", "scheduled_tasks", "tasks", "outbound_calls", "user_contact_points"]).fetch_one(&pool).await.unwrap();
    assert_eq!(nullable_authority_columns, 0);
    assert!(!names.iter().any(|name| name == "user_identities"));
    let legacy_scopes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform_deployments d LEFT JOIN host_apps h ON h.deployment_id=d.id WHERE d.external_key='vox.legacy.deployment' OR h.external_key='vox.legacy.channel-host'",
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(legacy_scopes, 0);
    let legacy_conversation_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns WHERE table_schema='public' AND table_name='conversations' AND column_name IN ('active_user_id','verification_state')",
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(legacy_conversation_columns, 0);
    let legacy_null_indexes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND indexdef ILIKE '%user_context_id IS NULL%'",
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(legacy_null_indexes, 0);
}
