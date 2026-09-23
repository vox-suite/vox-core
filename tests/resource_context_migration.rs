use sqlx::{Connection, Executor, PgConnection, Row};
use uuid::Uuid;

async fn migration_connection() -> (PgConnection, String) {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let mut connection = PgConnection::connect(&database_url)
        .await
        .expect("connect to isolated test database");
    // The baseline creates shared test roles and auth helpers, so serialize
    // that portion even though each resource fixture uses its own schema.
    sqlx::query("SELECT pg_advisory_lock(9092305)")
        .execute(&mut connection)
        .await
        .expect("lock shared baseline setup");
    let schema = format!("e05_{}", Uuid::new_v4().simple());
    connection
        .execute(format!("CREATE SCHEMA {schema}").as_str())
        .await
        .expect("create isolated schema");
    connection
        .execute(format!("SET search_path TO {schema}, public").as_str())
        .await
        .expect("select isolated schema");
    sqlx::raw_sql(include_str!(
        "../migrations/20260923000000_initial_core.sql"
    ))
    .execute(&mut connection)
    .await
    .expect("apply consumer baseline");
    sqlx::raw_sql(include_str!(
        "../migrations/20260923000001_restore_platform_contract.sql"
    ))
    .execute(&mut connection)
    .await
    .expect("apply platform restore");
    sqlx::query("SELECT pg_advisory_unlock(9092305)")
        .execute(&mut connection)
        .await
        .expect("unlock shared baseline setup");
    (connection, schema)
}

async fn seed_users(connection: &mut PgConnection) {
    sqlx::raw_sql(
        "INSERT INTO users (id) VALUES
            ('00000000-0000-0000-0000-000000000101'),
            ('00000000-0000-0000-0000-000000000102');
         INSERT INTO platform_deployments (id, external_key) VALUES
            ('00000000-0000-0000-0000-000000000201', 'test-deployment');
         INSERT INTO host_apps (id, deployment_id, external_key) VALUES
            ('00000000-0000-0000-0000-000000000301',
             '00000000-0000-0000-0000-000000000201', 'test-host');
         INSERT INTO user_contexts
            (id, deployment_id, host_app_id, host_user_id, user_id) VALUES
            ('00000000-0000-0000-0000-000000000401',
             '00000000-0000-0000-0000-000000000201',
             '00000000-0000-0000-0000-000000000301',
             'canonical-user',
             '00000000-0000-0000-0000-000000000101');",
    )
    .execute(connection)
    .await
    .expect("seed canonical and legacy users");
}

async fn remove_schema(connection: &mut PgConnection, schema: &str) {
    connection
        .execute("SET search_path TO public")
        .await
        .expect("leave test schema");
    connection
        .execute(format!("DROP SCHEMA {schema} CASCADE").as_str())
        .await
        .expect("remove isolated schema");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn populated_migration_preserves_contexts_and_backfills_legacy_resources() {
    let (mut connection, schema) = migration_connection().await;
    seed_users(&mut connection).await;
    sqlx::raw_sql(
        "INSERT INTO conversations (user_id, channel, external_conversation_id)
         VALUES ('00000000-0000-0000-0000-000000000101', 'web', 'shared'),
                ('00000000-0000-0000-0000-000000000102', 'phone', 'legacy');
         INSERT INTO tasks (user_id, title, instruction)
         VALUES ('00000000-0000-0000-0000-000000000102', 'Task', 'Do work');
         INSERT INTO schedules (user_id, instruction, kind)
         VALUES ('00000000-0000-0000-0000-000000000102', 'Reminder', 'once');",
    )
    .execute(&mut connection)
    .await
    .expect("seed existing resources");

    sqlx::raw_sql(include_str!(
        "../migrations/20260923000002_resource_context_ownership.sql"
    ))
    .execute(&mut connection)
    .await
    .expect("migrate populated schema");

    let counts = sqlx::query(
        "SELECT (SELECT count(*) FROM users) AS users,
                (SELECT count(*) FROM user_contexts) AS contexts,
                (SELECT count(*) FROM conversations WHERE user_context_id IS NULL) AS conversation_orphans,
                (SELECT count(*) FROM tasks WHERE user_context_id IS NULL) AS task_orphans,
                (SELECT count(*) FROM schedules WHERE user_context_id IS NULL) AS schedule_orphans",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(counts.get::<i64, _>("users"), 2);
    assert_eq!(counts.get::<i64, _>("contexts"), 2);
    assert_eq!(counts.get::<i64, _>("conversation_orphans"), 0);
    assert_eq!(counts.get::<i64, _>("task_orphans"), 0);
    assert_eq!(counts.get::<i64, _>("schedule_orphans"), 0);

    let canonical_context: Uuid = sqlx::query_scalar(
        "SELECT user_context_id FROM conversations WHERE external_conversation_id = 'shared'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert_eq!(
        canonical_context,
        Uuid::parse_str("00000000-0000-0000-0000-000000000401").unwrap()
    );
    let legacy_context: Uuid =
        sqlx::query_scalar("SELECT user_context_id FROM tasks WHERE title = 'Task'")
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_ne!(legacy_context, canonical_context);
    assert_ne!(
        legacy_context,
        Uuid::parse_str("00000000-0000-0000-0000-000000000102").unwrap()
    );

    sqlx::query(
        "INSERT INTO conversations (user_id, channel, external_conversation_id)
         VALUES ('00000000-0000-0000-0000-000000000102', 'web', 'shared')",
    )
    .execute(&mut connection)
    .await
    .expect("same host conversation ID can belong to another context");
    let forged = sqlx::query(
        "INSERT INTO conversations
            (user_id, user_context_id, channel, external_conversation_id)
         VALUES ('00000000-0000-0000-0000-000000000102', $1, 'web', 'forged')",
    )
    .bind(canonical_context)
    .execute(&mut connection)
    .await;
    assert!(
        forged.is_err(),
        "mismatched context and user must be rejected"
    );
    sqlx::query(
        "INSERT INTO audit_events (user_id, actor, event_type)
         VALUES ('00000000-0000-0000-0000-000000000102', 'test', 'user.deleted')",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    sqlx::query("DELETE FROM users WHERE id = '00000000-0000-0000-0000-000000000102'")
        .execute(&mut connection)
        .await
        .expect("user deletion retains audit row without dangling context");
    let audit = sqlx::query(
        "SELECT user_id, user_context_id FROM audit_events WHERE event_type = 'user.deleted'",
    )
    .fetch_one(&mut connection)
    .await
    .unwrap();
    assert!(audit.get::<Option<Uuid>, _>("user_id").is_none());
    assert!(audit.get::<Option<Uuid>, _>("user_context_id").is_none());
    remove_schema(&mut connection, &schema).await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn malformed_cross_owner_reference_halts_migration() {
    let (mut connection, schema) = migration_connection().await;
    seed_users(&mut connection).await;
    sqlx::raw_sql(
        "INSERT INTO collections (id, user_id, name)
         VALUES ('00000000-0000-0000-0000-000000000501',
                 '00000000-0000-0000-0000-000000000102', 'Other user');
         INSERT INTO tasks (user_id, collection_id, title, instruction)
         VALUES ('00000000-0000-0000-0000-000000000101',
                 '00000000-0000-0000-0000-000000000501', 'Bad task', 'Do work');",
    )
    .execute(&mut connection)
    .await
    .expect("old schema permits malformed ownership");

    let migration = sqlx::raw_sql(include_str!(
        "../migrations/20260923000002_resource_context_ownership.sql"
    ))
    .execute(&mut connection)
    .await;
    let error = migration.expect_err("malformed cross-owner row must halt migration");
    assert!(
        error.to_string().contains("tasks_collection_owner_fk"),
        "expected the task collection owner constraint, got: {error}"
    );
    remove_schema(&mut connection, &schema).await;
}
