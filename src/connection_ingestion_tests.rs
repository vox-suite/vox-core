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
    CoreIngestor { hub: None }
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
    CoreIngestor { hub: None }
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
    CoreIngestor { hub: None }
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
    CoreIngestor { hub: None }
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
    CoreIngestor { hub: None }
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
        CoreIngestor { hub: None }
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
        CoreIngestor { hub: None }
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
    async fn receive_for_user(
        listener: &mut sqlx::postgres::PgListener,
        user: Uuid,
    ) -> serde_json::Value {
        loop {
            let event = listener.recv().await.unwrap();
            let payload: serde_json::Value = serde_json::from_str(event.payload()).unwrap();
            if payload["user_id"] == user.to_string() {
                return payload;
            }
        }
    }
    let unrelated = serde_json::json!({
        "type": "span_updated",
        "user_id": Uuid::new_v4().to_string(),
    });
    sqlx::query("SELECT pg_notify('vox_connection_spans', $1)")
        .bind(unrelated.to_string())
        .execute(db.pool())
        .await
        .unwrap();
    let now = Utc::now();
    let connection = Uuid::new_v4();
    let mut tx = db.pool().begin().await.unwrap();
    CoreIngestor { hub: None }
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
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            receive_for_user(&mut listener, user),
        )
        .await
        .is_err()
    );
    let mut tx = db.pool().begin().await.unwrap();
    CoreIngestor { hub: None }
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
    let payload = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        receive_for_user(&mut listener, user),
    )
    .await
    .unwrap();
    assert_eq!(payload["type"], "span_updated");
    assert_eq!(payload["user_id"], user.to_string());
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
async fn food_ingestion_preserves_times_and_does_not_invent_missing_history() {
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
    let mut order:FoodDeliveryOrder=serde_json::from_value(json!({
        "order_id":"real-order", "provider":"swiggy", "restaurant_name":"Provider restaurant", "restaurant_location":null,
        "delivery_address":null,"delivery_location":null,"rider_name":null,"rider_phone":null,"rider_location":null,
        "status":"delivered","order_time":"2026-09-30T08:30:00Z","delivered_time":null,"eta_minutes":null,
        "total_amount":425.50,"currency":null,"items":["2 Dosas"],"provider_data":{"orderId":"real-order"}
    })).unwrap();
    let ingestor = CoreIngestor { hub: None };
    let mut tx = db.pool().begin().await.unwrap();
    assert_eq!(
        ingestor
            .food_order(&mut tx, user, connection, &[order.clone()])
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    let mut tx = db.pool().begin().await.unwrap();
    assert_eq!(
        ingestor
            .food_order(&mut tx, user, connection, &[order.clone()])
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    let row = sqlx::query("SELECT start_at,end_at,status,source_ref FROM spans WHERE user_id=$1")
        .bind(user)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(
        row.get::<DateTime<Utc>, _>("start_at").to_rfc3339(),
        "2026-09-30T08:30:00+00:00"
    );
    assert_eq!(row.get::<Option<DateTime<Utc>>, _>("end_at"), None);
    assert_eq!(row.get::<String, _>("status"), "done");
    order.order_id = "unknown-time".into();
    order.order_time = None;
    let mut tx = db.pool().begin().await.unwrap();
    assert_eq!(
        ingestor
            .food_order(&mut tx, user, connection, &[order])
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM spans WHERE user_id=$1")
        .bind(user)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
async fn personal_ingestion_is_atomic_deduplicated_and_preserves_unknown_duration() {
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
    let activity = PersonalActivity {
        source_id: "verified-account:track:2026-10-01T08:00:00Z".into(),
        title: "Track".into(),
        occurred_at: "2026-10-01T08:00:00Z".parse().unwrap(),
        ended_at: None,
        provider_data: json!({"track_id":"track", "duration_ms":180000}),
    };
    let ingestor = CoreIngestor { hub: None };
    let mut tx = db.pool().begin().await.unwrap();
    ingestor
        .personal_activity(
            &mut tx,
            user,
            Uuid::new_v4(),
            "spotify",
            std::slice::from_ref(&activity),
        )
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM inbound_events WHERE user_id=$1")
            .bind(user)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    let connection = Uuid::new_v4();
    for expected in [1, 0] {
        let mut tx = db.pool().begin().await.unwrap();
        assert_eq!(
            ingestor
                .personal_activity(
                    &mut tx,
                    user,
                    connection,
                    "spotify",
                    std::slice::from_ref(&activity)
                )
                .await
                .unwrap(),
            expected
        );
        tx.commit().await.unwrap();
    }
    let row = sqlx::query(
        "SELECT id,start_at,end_at,version,source_event_id,data FROM spans WHERE user_id=$1",
    )
    .bind(user)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        row.get::<DateTime<Utc>, _>("start_at"),
        activity.occurred_at
    );
    assert_eq!(row.get::<Option<DateTime<Utc>>, _>("end_at"), None);
    assert_eq!(row.get::<i32, _>("version"), 1);
    assert!(row.get::<Option<Uuid>, _>("source_event_id").is_some());
    assert_eq!(
        row.get::<serde_json::Value, _>("data")["session_times_known"],
        false
    );
    let span: Uuid = row.get("id");
    sqlx::query("UPDATE spans SET notes='Keep note',data=data||'{\"local_annotation\":true}'::jsonb WHERE id=$1").bind(span).execute(db.pool()).await.unwrap();
    let mut tx = db.pool().begin().await.unwrap();
    assert_eq!(
        ingestor
            .personal_activity(
                &mut tx,
                user,
                Uuid::new_v4(),
                "spotify",
                std::slice::from_ref(&activity)
            )
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM inbound_events WHERE user_id=$1")
            .bind(user)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM spans WHERE user_id=$1")
            .bind(user)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    let row = sqlx::query("SELECT notes,data FROM spans WHERE id=$1")
        .bind(span)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("notes"), "Keep note");
    assert_eq!(
        row.get::<serde_json::Value, _>("data")["local_annotation"],
        true
    );
    assert!(matches!(
        crate::storage::spans::SpanRepository::new(db.pool().clone())
            .update(
                user,
                span,
                crate::domain::spans::SpanPatch {
                    start_at: Some(None),
                    ..Default::default()
                }
            )
            .await
            .unwrap(),
        crate::domain::ConcurrencyOutcome::Conflict
    ));
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
async fn youtube_ingestion_keeps_playlist_events_distinct_from_watches() {
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
    let items = [
        PersonalActivity {
            source_id: "account:likes:video".into(),
            title: "Video".into(),
            occurred_at: "2026-10-01T08:00:00Z".parse().unwrap(),
            ended_at: None,
            provider_data: json!({"action":"like","watch_event":false}),
        },
        PersonalActivity {
            source_id: "takeout:video:time".into(),
            title: "Video".into(),
            occurred_at: "2026-10-02T08:00:00Z".parse().unwrap(),
            ended_at: None,
            provider_data: json!({"action":"watch","watch_event":true}),
        },
    ];
    let mut tx = db.pool().begin().await.unwrap();
    assert_eq!(
        CoreIngestor { hub: None }
            .personal_activity(&mut tx, user, Uuid::new_v4(), "youtube", &items)
            .await
            .unwrap(),
        2
    );
    tx.commit().await.unwrap();
    let rows=sqlx::query("SELECT s.title,s.start_at,s.end_at,e.event_type FROM spans s JOIN inbound_events e ON e.id=s.source_event_id WHERE s.user_id=$1 ORDER BY s.start_at").bind(user).fetch_all(db.pool()).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<String, _>("title"), "YouTube like: Video");
    assert_eq!(rows[0].get::<String, _>("event_type"), "video.liked");
    assert_eq!(rows[1].get::<String, _>("title"), "Video");
    assert_eq!(rows[1].get::<String, _>("event_type"), "video.watched");
    assert_eq!(
        rows[1].get::<DateTime<Utc>, _>("start_at"),
        items[1].occurred_at
    );
    assert_eq!(rows[1].get::<Option<DateTime<Utc>>, _>("end_at"), None);
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
async fn takeout_import_reaches_core_without_oauth_and_requires_consent() {
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
    let service =
        FreshConnectionsService::new(db.pool().clone(), None, None, None, None, None).unwrap();
    let history = json!([{"title":"Watched Actual video","products":["YouTube"],"titleUrl":"https://www.youtube.com/watch?v=dQw4w9WgXcQ","time":"2026-10-01T08:00:00Z"},{"title":"No time","titleUrl":"https://www.youtube.com/watch?v=dQw4w9WgXcQ"}]);
    assert!(
        service
            .import_youtube_history(
                &service.native_scope(user).await.unwrap(),
                history.clone(),
                false
            )
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM vox_connections WHERE user_id=$1")
            .bind(user)
            .fetch_one(db.pool())
            .await
            .unwrap(),
        0
    );
    let first = service
        .import_youtube_history(
            &service.native_scope(user).await.unwrap(),
            history.clone(),
            true,
        )
        .await
        .unwrap();
    assert_eq!(first["imported"], 1);
    assert_eq!(first["skipped"], 1);
    let second = service
        .import_youtube_history(&service.native_scope(user).await.unwrap(), history, true)
        .await
        .unwrap();
    assert_eq!(second["imported"], 0);
    assert_eq!(first["connection_id"], second["connection_id"]);
    let row=sqlx::query("SELECT s.title,s.start_at,s.end_at,s.source,e.event_type,c.connector_id FROM spans s JOIN inbound_events e ON e.id=s.source_event_id JOIN vox_connections c ON c.id=(s.data->>'connection_id')::uuid WHERE s.user_id=$1").bind(user).fetch_one(db.pool()).await.unwrap();
    assert_eq!(row.get::<String, _>("title"), "Actual video");
    assert_eq!(row.get::<String, _>("source"), "youtube");
    assert_eq!(row.get::<String, _>("connector_id"), "youtube_history");
    assert_eq!(row.get::<String, _>("event_type"), "video.watched");
    assert_eq!(
        row.get::<DateTime<Utc>, _>("start_at"),
        "2026-10-01T08:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(row.get::<Option<DateTime<Utc>>, _>("end_at"), None);
}
