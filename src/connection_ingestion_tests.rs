use super::*;
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
async fn calendar_ingestion_is_atomic_versioned_scoped_and_preserves_local_annotations() {
    let db = crate::db::Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    crate::identity::IdentityService::new(db.clone())
        .resolve_for_user(user)
        .await
        .unwrap();
    let connection = Uuid::new_v4();
    let other_connection = Uuid::new_v4();
    let now = Utc::now();
    let mut event = GoogleCalendarEvent {
        id: "instance-1".into(),
        summary: "Meeting".into(),
        description: None,
        location: None,
        start: now,
        end: now + chrono::Duration::hours(1),
        all_day: false,
        time_zone: Some("Asia/Kolkata".into()),
        status: "confirmed".into(),
        recurring_event_id: Some("series".into()),
        original_start: Some(json!({"dateTime":now})),
        start_date: None,
        end_date: None,
        has_time: true,
    };
    let mut tx = db.pool().begin().await.unwrap();
    CoreIngestor
        .calendar(
            &mut tx,
            user,
            connection,
            "google-account",
            &[event.clone()],
            now - chrono::Duration::days(1),
            now + chrono::Duration::days(1),
        )
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM spans WHERE user_id=$1")
        .bind(user)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
    let mut tx = db.pool().begin().await.unwrap();
    CoreIngestor
        .calendar(
            &mut tx,
            user,
            connection,
            "google-account",
            &[event.clone()],
            now - chrono::Duration::days(1),
            now + chrono::Duration::days(1),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let span: Uuid = sqlx::query_scalar("SELECT id FROM spans WHERE user_id=$1")
        .bind(user)
        .fetch_one(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE spans SET notes='Local note',data=data||'{\"local_annotation\":\"keep\"}'::jsonb WHERE id=$1").bind(span).execute(db.pool()).await.unwrap();
    event.summary = "Changed by provider".into();
    let mut tx = db.pool().begin().await.unwrap();
    CoreIngestor
        .calendar(
            &mut tx,
            user,
            connection,
            "google-account",
            &[event.clone()],
            now - chrono::Duration::days(1),
            now + chrono::Duration::days(1),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let row = sqlx::query("SELECT title,notes,data,version FROM spans WHERE id=$1")
        .bind(span)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("notes"), "Local note");
    assert_eq!(row.get::<i32, _>("version"), 2);
    assert_eq!(
        row.get::<serde_json::Value, _>("data")["local_annotation"],
        "keep"
    );
    let mut tx = db.pool().begin().await.unwrap();
    CoreIngestor
        .calendar(
            &mut tx,
            user,
            other_connection,
            "other-google-account",
            &[event.clone()],
            now - chrono::Duration::days(1),
            now + chrono::Duration::days(1),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let row = sqlx::query("SELECT status FROM spans WHERE id=$1")
        .bind(span)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_ne!(row.get::<String, _>("status"), "cancelled");
    event.status = "cancelled".into();
    event.has_time = false;
    let mut tx = db.pool().begin().await.unwrap();
    CoreIngestor
        .calendar(
            &mut tx,
            user,
            connection,
            "google-account",
            &[event.clone()],
            now - chrono::Duration::days(1),
            now + chrono::Duration::days(1),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let row = sqlx::query("SELECT status,start_at,notes FROM spans WHERE id=$1")
        .bind(span)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("status"), "cancelled");
    assert_eq!(
        row.get::<DateTime<Utc>, _>("start_at").timestamp(),
        now.timestamp()
    );
    assert_eq!(row.get::<String, _>("notes"), "Local note");
    let service = crate::application::spans::SpanService::new(
        crate::storage::spans::SpanRepository::new(db.pool().clone()),
        crate::realtime::UserEventHub::new(),
    );
    let actor = crate::domain::identity::Actor {
        user_id: user,
        principal_id: user,
        principal_kind: crate::domain::identity::PrincipalKind::User,
        grants: vec![],
    };
    assert!(
        service
            .update_span(
                &actor,
                span,
                crate::domain::spans::SpanPatch {
                    title: Some("Local change".into()),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
async fn calendar_repository_rejects_provider_owned_fields() {
    let db = crate::db::Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    crate::identity::IdentityService::new(db.clone())
        .resolve_for_user(user)
        .await
        .unwrap();
    let id:Uuid=sqlx::query_scalar("INSERT INTO spans(user_id,title,source) VALUES($1,'Provider title','google_calendar') RETURNING id").bind(user).fetch_one(db.pool()).await.unwrap();
    let repo = crate::storage::spans::SpanRepository::new(db.pool().clone());
    for patch in [
        crate::domain::spans::SpanPatch {
            title: Some("Override".into()),
            ..Default::default()
        },
        crate::domain::spans::SpanPatch {
            start_at: Some(None),
            ..Default::default()
        },
        crate::domain::spans::SpanPatch {
            status: Some(crate::domain::spans::SpanStatus::Done),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            repo.update(user, id, patch).await.unwrap(),
            crate::domain::ConcurrencyOutcome::Conflict
        ));
    }
    assert!(matches!(
        repo.update(
            user,
            id,
            crate::domain::spans::SpanPatch {
                notes: Some("My note".into()),
                ..Default::default()
            }
        )
        .await
        .unwrap(),
        crate::domain::ConcurrencyOutcome::Success(_)
    ));
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
async fn gaming_ingestion_retains_unknown_session_times_and_deduplicates() {
    let db = crate::db::Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    crate::identity::IdentityService::new(db.clone())
        .resolve_for_user(user)
        .await
        .unwrap();
    let connection = Uuid::new_v4();
    let now = Utc::now();
    let activity = ObservedActivity {
        game: vox_connections::providers::playstation::PlayStationGame {
            title_id: "game".into(),
            name: "Game".into(),
            platform: "PS5".into(),
            category: "gaming".into(),
            image_url: None,
            first_played_at: None,
            last_played_at: None,
            play_duration_seconds: 200,
            play_count: 1,
        },
        duration_seconds: 100,
        observation_start: now - chrono::Duration::days(1),
        observation_end: now,
        source_ref: "verified-account:game:100:200".into(),
    };
    for _ in 0..2 {
        let mut tx = db.pool().begin().await.unwrap();
        CoreIngestor
            .gaming(&mut tx, user, connection, std::slice::from_ref(&activity))
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let row = sqlx::query("SELECT start_at,end_at,data FROM spans WHERE user_id=$1")
        .bind(user)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert!(row.get::<Option<DateTime<Utc>>, _>("start_at").is_none());
    assert!(row.get::<Option<DateTime<Utc>>, _>("end_at").is_none());
    assert_eq!(
        row.get::<serde_json::Value, _>("data")["session_times_known"],
        false
    );
    assert_eq!(
        row.get::<serde_json::Value, _>("data")["duration_seconds"],
        100
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM spans WHERE user_id=$1")
            .bind(user)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
async fn reconnect_same_calendar_preserves_one_annotated_span() {
    let db = crate::db::Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    crate::identity::IdentityService::new(db.clone())
        .resolve_for_user(user)
        .await
        .unwrap();
    let now = Utc::now();
    let event = GoogleCalendarEvent {
        id: "stable-instance".into(),
        summary: "Meeting".into(),
        description: None,
        location: None,
        start: now,
        end: now + chrono::Duration::hours(1),
        all_day: false,
        time_zone: Some("UTC".into()),
        status: "confirmed".into(),
        recurring_event_id: None,
        original_start: None,
        start_date: None,
        end_date: None,
        has_time: true,
    };
    for connection in [Uuid::new_v4(), Uuid::new_v4()] {
        let mut tx = db.pool().begin().await.unwrap();
        CoreIngestor
            .calendar(
                &mut tx,
                user,
                connection,
                "same-account",
                std::slice::from_ref(&event),
                now - chrono::Duration::days(1),
                now + chrono::Duration::days(1),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        sqlx::query("UPDATE spans SET notes='Retained note' WHERE user_id=$1")
            .bind(user)
            .execute(db.pool())
            .await
            .unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM spans WHERE user_id=$1")
            .bind(user)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT notes FROM spans WHERE user_id=$1")
            .bind(user)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        "Retained note"
    );
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
async fn committed_ingestion_publishes_the_native_span_contract() {
    let url = std::env::var("TEST_DATABASE_URL").unwrap();
    let db = crate::db::Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    crate::identity::IdentityService::new(db.clone())
        .resolve_for_user(user)
        .await
        .unwrap();
    let mut listener = sqlx::postgres::PgListener::connect(&url).await.unwrap();
    listener.listen("vox_connection_spans").await.unwrap();
    let now = Utc::now();
    let connection = Uuid::new_v4();
    let mut tx = db.pool().begin().await.unwrap();
    CoreIngestor
        .calendar(
            &mut tx,
            user,
            connection,
            "account",
            &[],
            now - chrono::Duration::days(1),
            now + chrono::Duration::days(1),
        )
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.recv())
            .await
            .is_err()
    );
    let mut tx = db.pool().begin().await.unwrap();
    CoreIngestor
        .calendar(
            &mut tx,
            user,
            connection,
            "account",
            &[],
            now - chrono::Duration::days(1),
            now + chrono::Duration::days(1),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), listener.recv())
        .await
        .unwrap()
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
    assert_eq!(payload["type"], "span_updated");
    assert_eq!(payload["user_id"], user.to_string());
}
