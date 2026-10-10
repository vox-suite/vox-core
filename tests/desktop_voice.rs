use sqlx::PgPool;
use uuid::Uuid;
use vox_core::desktop_voice::DesktopVoiceBroker;

#[tokio::test]
#[ignore = "requires isolated VOX_DESKTOP_TEST_DATABASE_URL"]
async fn tickets_are_owned_expiring_and_single_use() {
    let pool = PgPool::connect(&std::env::var("VOX_DESKTOP_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let user = Uuid::new_v4();
    let other = Uuid::new_v4();
    for id in [user, other] {
        sqlx::query("INSERT INTO users(id) VALUES($1)")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let broker = DesktopVoiceBroker::new(pool.clone());
    let issued = broker.issue(user, None).await.unwrap();
    assert!(broker.redeem("invalid").await.unwrap().is_none());
    let redeemed = broker.redeem(&issued.ticket).await.unwrap().unwrap();
    assert_eq!(redeemed.user_id, user);
    assert!(broker.redeem(&issued.ticket).await.unwrap().is_none());
    assert!(broker.active(issued.session_id).await.unwrap().is_some());
    broker.complete(issued.session_id).await.unwrap();
    assert!(broker.active(issued.session_id).await.unwrap().is_none());
    let expired = broker.issue(user, None).await.unwrap();
    sqlx::query(
        "UPDATE desktop_voice_sessions SET expires_at=now()-interval '1 second' WHERE id=$1",
    )
    .bind(expired.session_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(broker.redeem(&expired.ticket).await.unwrap().is_none());
    assert!(broker.issue(other, Some(Uuid::new_v4())).await.is_err());
    sqlx::query("INSERT INTO user_contexts(deployment_id,host_app_id,host_user_id,user_id) SELECT deployment_id,id,$1,$2 FROM host_apps LIMIT 1").bind(user.to_string()).bind(user).execute(&pool).await.unwrap();
    let device:Uuid=sqlx::query_scalar("INSERT INTO devices(user_id,device_identifier,platform,execution_consent,capabilities) VALUES($1,$2,'test',true,'{\"vox_app_control\":true}'::jsonb) RETURNING id").bind(user).bind(Uuid::new_v4().to_string()).fetch_one(&pool).await.unwrap();
    let control = vox_core::desktop::service::DesktopControlService {
        pool: pool.clone(),
        hub: vox_core::realtime::DeviceHub::default(),
    };
    assert_eq!(
        control.target(user, Some(device), None).await.unwrap(),
        device
    );
    assert!(control.context(device).await.is_err());
    assert!(control.target(other, Some(device), None).await.is_err());
    let bound = broker.issue(user, Some(device)).await.unwrap();
    broker.redeem(&bound.ticket).await.unwrap().unwrap();
    sqlx::query("UPDATE devices SET revoked_at=now() WHERE id=$1")
        .bind(device)
        .execute(&pool)
        .await
        .unwrap();
    assert!(broker.active(bound.session_id).await.unwrap().is_none());
    assert!(broker.issue(user, Some(device)).await.is_err());
}
