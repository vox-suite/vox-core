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
        "status_events",
        "status_webhook_subscriptions",
        "status_webhook_secrets",
        "status_webhook_deliveries",
        "verified_integration_events",
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

    let deployment: Uuid = sqlx::query_scalar(
        "INSERT INTO platform_deployments (external_key) VALUES ($1) RETURNING id",
    )
    .bind(format!("rls-test-{}", Uuid::new_v4()))
    .fetch_one(&mut *tx)
    .await
    .expect("insert test deployment");
    let host: Uuid = sqlx::query_scalar(
        "INSERT INTO host_apps (deployment_id,external_key) VALUES ($1,'rls-test') RETURNING id",
    )
    .bind(deployment)
    .fetch_one(&mut *tx)
    .await
    .expect("insert test host");
    sqlx::query(
        "INSERT INTO user_contexts (deployment_id,host_app_id,host_user_id,user_id)
         VALUES ($1,$2,'user-a',$3),($1,$2,'user-b',$4)",
    )
    .bind(deployment)
    .bind(host)
    .bind(user_a)
    .bind(user_b)
    .execute(&mut *tx)
    .await
    .expect("insert canonical contexts");

    let schema_id: Uuid = sqlx::query_scalar(
        "INSERT INTO data_schemas (namespace,name,json_schema)
         VALUES ('rls-test','note','{}'::jsonb) RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await
    .expect("insert global test schema");

    sqlx::query(
        "INSERT INTO records (id, user_id, schema_id, schema_scope, kind, title, data)
                 VALUES (gen_random_uuid(), $1, $3, 'global', 'fact', 'Note A', '{}'),
                        (gen_random_uuid(), $2, $3, 'global', 'fact', 'Note B', '{}')",
    )
    .bind(user_a)
    .bind(user_b)
    .bind(schema_id)
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
    sqlx::query("SAVEPOINT cross_user_insert")
        .execute(&mut *tx)
        .await
        .expect("savepoint before expected RLS denial");
    let insert_b_result = sqlx::query(
        "INSERT INTO records (id, user_id, schema_id, schema_scope, kind, title, data)
         VALUES (gen_random_uuid(), $1, $2, 'global', 'fact', 'Illicit B', '{}')",
    )
    .bind(user_b)
    .bind(schema_id)
    .execute(&mut *tx)
    .await;
    assert!(
        insert_b_result.is_err(),
        "RLS must block inserting rows for another user"
    );
    sqlx::query("ROLLBACK TO SAVEPOINT cross_user_insert")
        .execute(&mut *tx)
        .await
        .expect("clear expected RLS error");

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
