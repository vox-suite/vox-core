//! Exercise both upgrade paths in isolated databases, including surviving ciphertext.
use sqlx::{PgPool, Row};
use uuid::Uuid;
use vox_core::{agent_registry::AgentRegistry, db::Db, identity::IdentityService};

async fn database() -> (PgPool, Db, String) {
    let mut url =
        url::Url::parse(&std::env::var("TEST_DATABASE_URL").expect("disposable database required"))
            .unwrap();
    let admin = PgPool::connect(url.as_str()).await.unwrap();
    let name = format!("vox_recovery_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
    url.set_path(&name);
    let db = Db::connect(url.as_str()).await.unwrap();
    (admin, db, name)
}
async fn apply(db: &Db, low: i64, high: i64) {
    let migrator = sqlx::migrate!();
    for m in migrator
        .iter()
        .filter(|m| m.version >= low && m.version < high)
    {
        let mut tx = db.pool().begin().await.unwrap();
        sqlx::raw_sql(&m.sql)
            .execute(&mut *tx)
            .await
            .unwrap_or_else(|e| panic!("migration {}: {e}", m.version));
        tx.commit().await.unwrap();
    }
}
async fn seed(db: &Db) -> (Uuid, Uuid, Uuid, Vec<u8>) {
    let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let context = IdentityService::new(db.clone())
        .resolve_for_user(user)
        .await
        .unwrap();
    let agent = AgentRegistry::new(db.clone())
        .owned_for_context(&context)
        .await
        .unwrap()
        .remove(0);
    let integration:Uuid=sqlx::query_scalar("INSERT INTO integration_definitions(deployment_id,external_key,protocol,display_name,declaration_version,state) VALUES($1,$2,'direct','Upgrade fixture',1,'enabled') RETURNING id")
        .bind(context.subject.deployment_id.0).bind(format!("recovery-{user}")).fetch_one(db.pool()).await.unwrap();
    let connection:Uuid=sqlx::query_scalar("INSERT INTO external_connections(user_context_id,integration_id,external_account_hash,credential_custody,authorization_state,authorized_capabilities) VALUES($1,$2,$3,'platform_held','authorized',ARRAY['upgrade.read']) RETURNING id")
        .bind(context.id.0).bind(integration).bind(vec![7u8;32]).fetch_one(db.pool()).await.unwrap();
    let grant:Uuid=sqlx::query_scalar("INSERT INTO agent_capability_grants(user_context_id,agent_definition_id,connection_id,capability_external_key) VALUES($1,$2,$3,'upgrade.read') RETURNING id")
        .bind(context.id.0).bind(agent.definition.id).bind(connection).fetch_one(db.pool()).await.unwrap();
    let ciphertext = vox_connections::crypto::CredentialCipher::from_hex_key(&"ab".repeat(32))
        .unwrap()
        .seal(b"fixture", "protected")
        .unwrap();
    sqlx::query("INSERT INTO playstation_accounts(connection_id,account_id,access_ciphertext,refresh_ciphertext,access_expires_at,refresh_expires_at) VALUES($1,'verified',$2,$2,now()+interval '1 hour',now()+interval '1 day')")
        .bind(connection).bind(&ciphertext).execute(db.pool()).await.unwrap();
    (user, connection, grant, ciphertext)
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL, PostgreSQL 18 and pgvector"]
async fn old_platform_upgrade_preserves_credentials_ids_and_existing_authority() {
    let (admin, db, name) = database().await;
    apply(&db, 0, 20261003235959).await;
    let (_, connection, grant, cipher) = seed(&db).await;
    apply(&db, 20261003235959, i64::MAX).await;
    let row=sqlx::query("SELECT c.authorization_state,p.access_ciphertext FROM external_connections c JOIN playstation_accounts p ON p.connection_id=c.id WHERE c.id=$1").bind(connection).fetch_one(db.pool()).await.unwrap();
    assert_eq!(row.get::<String, _>("authorization_state"), "authorized");
    assert_eq!(row.get::<Vec<u8>, _>("access_ciphertext"), cipher);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM agent_capability_grants WHERE id=$1")
            .bind(grant)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        "enabled"
    );
    assert_eq!(
        sqlx::query_scalar::<_, bool>(
            "SELECT to_regnamespace('connector_upgrade_snapshot') IS NULL"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        true
    );
    db.pool().close().await;
    sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL, PostgreSQL 18 and pgvector"]
async fn retired_upgrade_does_not_revive_access_and_preserves_ambiguous_accounts() {
    let (admin, db, name) = database().await;
    // Simulate a deployment which ran the destructive retirement before recovery existed.
    apply(&db, 0, 20261003235959).await;
    let (user, connection, grant, _) = seed(&db).await;
    apply(&db, 20261004000000, 20261005000000).await;
    let context = IdentityService::new(db.clone())
        .resolve_for_user(user)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE user_contexts DROP CONSTRAINT user_contexts_user_id_key")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_contexts(deployment_id,host_app_id,host_user_id,user_id) VALUES($1,$2,'second-host-subject',$3)").bind(context.subject.deployment_id.0).bind(context.subject.host_app_id.0).bind(user).execute(db.pool()).await.unwrap();
    let cipher = vox_connections::crypto::CredentialCipher::from_hex_key(&"ab".repeat(32))
        .unwrap()
        .seal(format!("{user}:spotify").as_bytes(), "retained")
        .unwrap();
    let id:Uuid=sqlx::query_scalar("INSERT INTO vox_connections(user_id,connector_id,consented_at,access_ciphertext) VALUES($1,'spotify',now(),$2) RETURNING id").bind(user).bind(&cipher).fetch_one(db.pool()).await.unwrap();
    // The additive earlier safeguard is a no-op for an already-retired database.
    apply(&db, 20261003235959, 20261004000000).await;
    apply(&db, 20261005000000, i64::MAX).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT authorization_state FROM external_connections WHERE id=$1"
        )
        .bind(connection)
        .fetch_one(db.pool())
        .await
        .unwrap(),
        "revoked"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM agent_capability_grants WHERE id=$1")
            .bind(grant)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        "revoked"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM playstation_accounts WHERE connection_id=$1"
        )
        .bind(connection)
        .fetch_one(db.pool())
        .await
        .unwrap(),
        0
    );
    let row = sqlx::query(
        "SELECT user_context_id,access_ciphertext,failure_code FROM vox_connections WHERE id=$1",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<Option<Uuid>, _>("user_context_id"), None);
    assert_eq!(row.get::<Vec<u8>, _>("access_ciphertext"), cipher);
    assert_eq!(
        row.get::<String, _>("failure_code"),
        "scope_reassociation_required"
    );
    db.pool().close().await;
    sqlx::query(&format!("DROP DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
}
