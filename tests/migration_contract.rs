/**
* Integration tests verifying full database schema migrations.
*/
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
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE' ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .expect("list public tables");
    let names: Vec<String> = rows.iter().map(|row| row.get("table_name")).collect();

    for expected in [
        "users",
        "auth_identities",
        "channel_identities",
        "auth_sessions",
        "conversations",
        "messages",
        "collections",
        "spans",
        "schedules",
        "jobs",
        "job_attempts",
        "data_schemas",
        "records",
        "devices",
        "connections",
        "action_proposals",
        "action_approvals",
        "executions",
        "execution_attempts",
        "inbound_events",
        "audit_events",
        "platform_deployments",
        "host_apps",
        "host_organizations",
        "user_contexts",
        "host_app_credentials",
        "host_app_assertion_nonces",
        "identity_adapters",
        "login_identities",
        "federated_identity_nonces",
        "passwordless_recovery_challenges",
        "identity_authentication_sessions",
        "identity_links",
        "identity_link_events",
        "agent_definitions",
        "agent_model_configurations",
        "deployment_agent_selections",
        "integration_definitions",
        "integration_capability_declarations",
        "integration_declaration_versions",
        "external_connections",
        "agent_capability_grants",
        "connection_authorization_sessions",
        "reminders",
        "reminder_deliveries",
        "remote_extensions",
        "remote_extension_versions",
        "remote_extension_conformance_runs",
        "user_preferences",
        "portable_exports",
        "status_events",
        "status_webhook_subscriptions",
        "spending_policies",
        "operational_quotas",
        "operational_quota_reservations",
        "schedule_occurrence_dispatches",
        "auth_identities",
        "channel_identities",
        "auth_sessions",
        "conversations",
        "collections",
        "spans",
        "schedules",
        "jobs",
        "data_schemas",
        "records",
        "devices",
        "connections",
        "action_proposals",
        "action_approvals",
        "executions",
        "inbound_events",
        "audit_events",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing base table {expected}"
        );
    }
    assert!(
        names
            .iter()
            .filter(|name| *name != "_sqlx_migrations")
            .count()
            >= 56,
        "expected consumer and platform tables, found: {names:?}"
    );

    let view_rows = sqlx::query(
        "SELECT table_name FROM information_schema.views \
         WHERE table_schema = 'public' ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .expect("list public views");
    let view_names: Vec<String> = view_rows.iter().map(|row| row.get("table_name")).collect();
    for expected in [
        "scheduled_tasks",
        "events",
        "user_records",
        "user_goals",
        "user_insights",
        "client_devices",
    ] {
        assert!(
            view_names.iter().any(|name| name == expected),
            "missing compatibility view {expected}"
        );
    }

    let scoped_resources: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND column_name = 'user_context_id' \
         ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .expect("list tables with user_context_id");
    for expected in [
        "login_identities",
        "passwordless_recovery_challenges",
        "external_connections",
        "agent_capability_grants",
    ] {
        assert!(
            scoped_resources.iter().any(|table| table == expected),
            "missing context scope for {expected}: {scoped_resources:?}"
        );
    }
}
