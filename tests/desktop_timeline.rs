use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use vox_core::{domain::timeline::TimelineQuery, storage::timeline::TimelineRepository};
#[tokio::test]
#[ignore = "requires isolated VOX_DESKTOP_TEST_DATABASE_URL"]
async fn spending_filter_is_applied_before_pagination() {
    let pool = PgPool::connect(&std::env::var("VOX_DESKTOP_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let user = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id) VALUES($1)")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let event_type:Uuid=sqlx::query_scalar("SELECT id FROM timeline_event_types WHERE value='transaction' AND owner_user_id IS NULL LIMIT 1").fetch_one(&pool).await.unwrap();
    let group: Uuid = sqlx::query_scalar("SELECT group_id FROM timeline_event_types WHERE id=$1")
        .bind(event_type)
        .fetch_one(&pool)
        .await
        .unwrap();
    for (i, spend) in [(0, true), (1, false), (2, true)] {
        sqlx::query("INSERT INTO timeline_events(id,user_id,event_type_id,group_id,title,occurred_at,content) VALUES($1,$2,$3,$4,$5,now()+($6::int*interval '1 minute'),$7)").bind(Uuid::new_v4()).bind(user).bind(event_type).bind(group).bind(format!("Item {i}")).bind(i).bind(json!({"amount":10,"is_spending":spend,"direction":"debit","merchant":if i==2{"Other"}else{"Cafe"}})).execute(&pool).await.unwrap();
    }
    let page = TimelineRepository::new(pool)
        .query_events(
            user,
            TimelineQuery {
                spending_only: Some(true),
                merchant: Some("Cafe".into()),
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].event.title, "Item 0");
    assert!(page.next_cursor.is_none());
}
