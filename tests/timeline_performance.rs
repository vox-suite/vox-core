use chrono::{TimeZone, Utc};
use sqlx::PgPool;
use uuid::Uuid;
use vox_core::{
    domain::timeline::{TimelineCountsQuery, TimelineQuery},
    storage::timeline::TimelineRepository,
};

#[tokio::test]
#[ignore = "requires isolated VOX_DESKTOP_TEST_DATABASE_URL"]
async fn timeline_paging_filters_and_day_revisions() {
    let pool = PgPool::connect(&std::env::var("VOX_DESKTOP_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let user = Uuid::new_v4();
    let other_user = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id) VALUES($1),($2)")
        .bind(user)
        .bind(other_user)
        .execute(&pool)
        .await
        .unwrap();
    let kind: Uuid = sqlx::query_scalar("SELECT id FROM timeline_event_types WHERE value='transaction' AND owner_user_id IS NULL LIMIT 1")
        .fetch_one(&pool).await.unwrap();
    let group: Uuid = sqlx::query_scalar("SELECT group_id FROM timeline_event_types WHERE id=$1")
        .bind(kind)
        .fetch_one(&pool)
        .await
        .unwrap();
    let start = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
    let end = start + chrono::Duration::days(1);
    let mut ids = Vec::new();
    for _ in 0..65 {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO timeline_events(id,user_id,event_type_id,group_id,title,occurred_at,content) VALUES($1,$2,$3,$4,'Cafe purchase',$5,'{\"is_spending\":true,\"merchant\":\"Cafe\",\"category\":\"food\"}')")
            .bind(id).bind(user).bind(kind).bind(group).bind(start).execute(&pool).await.unwrap();
        ids.push(id);
    }
    sqlx::query("INSERT INTO timeline_events(user_id,event_type_id,group_id,title,occurred_at,content) VALUES($1,$2,$3,'Other user',$4,'{}')")
        .bind(other_user).bind(kind).bind(group).bind(start).execute(&pool).await.unwrap();
    ids.sort_by(|a, b| b.cmp(a));
    let repo = TimelineRepository::new(pool.clone());
    let query = TimelineQuery {
        start_at: Some(start),
        end_at: Some(end),
        group_id: Some(group),
        group_value: Some("finance".into()),
        event_type_id: Some(kind),
        event_type_value: Some("transaction".into()),
        spending_only: Some(true),
        merchant: Some("cAf".into()),
        category: Some("FOOD".into()),
        search: Some(" purchase ".into()),
        ..Default::default()
    };
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = repo
            .query_events(
                user,
                TimelineQuery {
                    cursor,
                    ..query.clone()
                },
            )
            .await
            .unwrap();
        assert!(page.events.len() <= 30);
        seen.extend(page.events.iter().map(|item| item.event.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, ids);
    let counts = TimelineCountsQuery {
        start_at: start,
        end_at: end,
        timezone: "UTC".into(),
        group_value: None,
    };
    let before = repo.day_summaries(user, counts.clone()).await.unwrap();
    assert_eq!(before.days[0].count, 65);
    assert_eq!(before.days[0].categories[0].count, 65);
    repo.delete_event(user, ids[0]).await.unwrap();
    let after = repo.day_summaries(user, counts).await.unwrap();
    assert_eq!(after.days[0].count, 64);
    assert_ne!(before.days[0].rev, after.days[0].rev);
    assert!(after.revision > before.revision);
    let empty = repo
        .day_summaries(
            Uuid::new_v4(),
            TimelineCountsQuery {
                start_at: start,
                end_at: end,
                timezone: "America/Los_Angeles".into(),
                group_value: None,
            },
        )
        .await
        .unwrap();
    assert!(empty.days.is_empty());
    assert_eq!(empty.revision, 0);
    sqlx::query("DELETE FROM timeline_events WHERE user_id=ANY($1)")
        .bind(&[user, other_user][..])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(&[user, other_user][..])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires isolated VOX_DESKTOP_TEST_DATABASE_URL"]
async fn pagination_index_covers_the_complete_cursor_order() {
    let pool = PgPool::connect(&std::env::var("VOX_DESKTOP_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE tablename='timeline_events'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        indexes.iter().any(
            |index| index.contains("(user_id, occurred_at DESC, id DESC)")
                && index.contains("record_state = 'active'")
        ),
        "active timeline index must cover both cursor columns"
    );
}
