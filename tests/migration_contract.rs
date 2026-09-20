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
        "action_attempts",
        "actions",
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
        "login_identities",
        "federated_identity_nonces",
        "passwordless_recovery_challenges",
        "platform_deployments",
        "scheduled_tasks",
        "user_contexts",
        "user_identities",
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
    for expected in ["actions", "conversations", "scheduled_tasks", "tasks"] {
        assert!(
            scoped_resources.iter().any(|table| table == expected),
            "{expected} is missing canonical user-context ownership"
        );
    }
}
