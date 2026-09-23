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
        "tasks",
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
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "missing base table {expected}"
        );
    }
    assert_eq!(
        names.len(),
        21,
        "expected exactly 21 core tables, found: {names:?}"
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
        "projects",
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
    assert!(
        scoped_resources.is_empty(),
        "core schema must not use user_context_id columns: {scoped_resources:?}"
    );
}
