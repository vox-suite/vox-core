/**
* Integration tests verifying Row Level Security (RLS) enforcement and isolation.
*/
use sqlx::Row;
use uuid::Uuid;
use vox_core::db::Db;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn rls_is_enabled_on_all_core_tables() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect db");
    db.migrate().await.expect("migrate");

    let pool = db.pool();
    let rows = sqlx::query(
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public' AND rowsecurity = true",
    )
    .fetch_all(pool)
    .await
    .expect("fetch rls tables");

    let rls_tables: Vec<String> = rows.iter().map(|r| r.get("tablename")).collect();

    let expected = [
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
    ];

    for table in expected {
        assert!(
            rls_tables.contains(&table.to_string()),
            "Table {table} does not have RLS enabled"
        );
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn rls_enforces_user_isolation_and_allows_service_role() {
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&database_url).await.expect("connect db");
    db.migrate().await.expect("migrate");

    let pool = db.pool();
    let mut tx = pool.begin().await.expect("begin tx");

    let user_a = Uuid::new_v4();
    let user_b = Uuid::new_v4();

    sqlx::query("INSERT INTO users (id, status) VALUES ($1, 'active'), ($2, 'active')")
        .bind(user_a)
        .bind(user_b)
        .execute(&mut *tx)
        .await
        .expect("insert test users");

    sqlx::query("INSERT INTO records (id, user_id, kind, title, data) VALUES (gen_random_uuid(), $1, 'note', 'Note A', '{}'), (gen_random_uuid(), $2, 'note', 'Note B', '{}')")
        .bind(user_a)
        .bind(user_b)
        .execute(&mut *tx)
        .await
        .expect("insert test records");

    // 1. Authenticated as User A
    sqlx::query("SET LOCAL ROLE authenticated")
        .execute(&mut *tx)
        .await
        .expect("set role authenticated");
    sqlx::query(&format!("SET LOCAL \"request.jwt.claim.sub\" = '{user_a}'"))
        .execute(&mut *tx)
        .await
        .expect("set jwt claim user_a");

    let rows_a = sqlx::query("SELECT user_id FROM records")
        .fetch_all(&mut *tx)
        .await
        .expect("select records as user a");
    assert_eq!(rows_a.len(), 1);
    let owner_a: Uuid = rows_a[0].get("user_id");
    assert_eq!(owner_a, user_a);

    // Attempt to insert record belonging to User B should fail under User A session
    let insert_b_result = sqlx::query(
        "INSERT INTO records (id, user_id, kind, title, data) VALUES (gen_random_uuid(), $1, 'note', 'Illicit B', '{}')",
    )
    .bind(user_b)
    .execute(&mut *tx)
    .await;
    assert!(
        insert_b_result.is_err(),
        "RLS must block inserting rows for another user"
    );

    // 2. Switch to service_role (backend worker / admin)
    sqlx::query("SET LOCAL ROLE service_role")
        .execute(&mut *tx)
        .await
        .expect("set role service_role");

    let all_records = sqlx::query("SELECT user_id FROM records")
        .fetch_all(&mut *tx)
        .await
        .expect("select records as service_role");
    assert_eq!(
        all_records.len(),
        2,
        "service_role must have access to all records"
    );

    tx.rollback().await.expect("rollback clean");
}
