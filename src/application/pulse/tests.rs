use super::measurements::*;
use crate::domain::{charts::ChartType, pulse::*};
use serde_json::json;
fn profile(source: &str, action: &str, fields: serde_json::Value) -> SourceProfile {
    serde_json::from_value(json!({"key":"fixture","source":source,"category":"activity","action":action,"timing":"provider_timestamp","count":12,"dated_count":12,"fields":fields,"samples":[],"currency":"","known_intervals":0})).unwrap()
}
#[test]
fn spotify_plays_do_not_require_numeric_fields() {
    let catalog = measurement_catalog(&[profile("spotify", "listen", json!({}))]);
    assert!(
        catalog
            .iter()
            .any(|m| m.kind == MeasurementKind::EventCount)
    );
}
#[test]
fn spotify_duration_is_qualified_as_estimate() {
    let catalog = measurement_catalog(&[profile(
        "spotify",
        "listen",
        json!({"provider_data.reported_track_duration_ms":"number"}),
    )]);
    let duration = catalog
        .iter()
        .find(|m| m.field.as_deref() == Some("provider_data.reported_track_duration_ms"))
        .expect("estimate available");
    assert_eq!(duration.quality, "estimated");
    assert_eq!(duration.scale, 1.0 / 3_600_000.0);
}
#[test]
fn youtube_playlist_is_not_watch_history() {
    let catalog = measurement_catalog(&[profile("youtube", "playlist_addition", json!({}))]);
    assert!(!catalog.iter().any(|m| m.title.contains("Watched")));
    assert!(catalog.iter().any(|m| m.title.contains("playlist")));
}
#[test]
fn playstation_deltas_cannot_be_bucketed_by_day() {
    let mut p = profile(
        "playstation",
        "",
        json!({"duration_seconds":"number","game":"string"}),
    );
    p.timing = "observed_counter_delta".into();
    let catalog = measurement_catalog(&[p]);
    let m = catalog
        .iter()
        .find(|m| m.kind == MeasurementKind::NumericSum)
        .unwrap();
    let d = PulseDefinition {
        version: 2,
        measurement_id: m.id.clone(),
        bucket: Some(Bucket::Day),
        dimension: None,
        period_days: 30,
        offset_days: 0,
        timezone: "Asia/Kolkata".into(),
        chart_type: ChartType::Bar,
    };
    assert!(validate_definition(&d, &catalog).is_err());
}
#[test]
fn manual_definition_rejects_invalid_timezone_and_unknown_metric() {
    let d = PulseDefinition {
        version: 2,
        measurement_id: "fabricated".into(),
        bucket: Some(Bucket::Day),
        dimension: None,
        period_days: 30,
        offset_days: 0,
        timezone: "invalid".into(),
        chart_type: ChartType::Line,
    };
    assert!(validate_definition(&d, &[]).is_err());
}

async fn database() -> crate::db::Db {
    let url = std::env::var("TEST_DATABASE_URL").expect("disposable TEST_DATABASE_URL");
    let db = crate::db::Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    db
}
async fn user(db: &crate::db::Db) -> uuid::Uuid {
    let id = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    crate::identity::IdentityService::new(db.clone())
        .resolve_for_user(id)
        .await
        .unwrap();
    id
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn revisions_advance_on_edit_delete_and_revocation() {
    let db = database().await;
    let id = user(&db).await;
    let span:uuid::Uuid=sqlx::query_scalar("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,'Expense','done','user','expense',now(),'{\"amount\":100,\"currency\":\"INR\"}') RETURNING id").bind(id).fetch_one(db.pool()).await.unwrap();
    let before: i64 =
        sqlx::query_scalar("SELECT data_revision FROM pulse_revisions WHERE user_id=$1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    sqlx::query("UPDATE spans SET data=data||'{\"amount\":200}' WHERE id=$1")
        .bind(span)
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM spans WHERE id=$1")
        .bind(span)
        .execute(db.pool())
        .await
        .unwrap();
    let after: i64 =
        sqlx::query_scalar("SELECT data_revision FROM pulse_revisions WHERE user_id=$1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(after, before + 2);
    sqlx::query(
        "INSERT INTO vox_connections(user_id,connector_id,consented_at) VALUES($1,'spotify',now())",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();
    let before: i64 =
        sqlx::query_scalar("SELECT discovery_revision FROM pulse_revisions WHERE user_id=$1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    sqlx::query("UPDATE vox_connections SET assistant_read=false WHERE user_id=$1")
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
    let after: i64 =
        sqlx::query_scalar("SELECT discovery_revision FROM pulse_revisions WHERE user_id=$1")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(after > before);
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn profiles_include_unschematized_spans_but_exclude_revoked_sources_and_plans() {
    let db = database().await;
    let id = user(&db).await;
    let connection:uuid::Uuid=sqlx::query_scalar("INSERT INTO vox_connections(user_id,connector_id,consented_at,assistant_read) VALUES($1,'spotify',now(),false) RETURNING id").bind(id).fetch_one(db.pool()).await.unwrap();
    for (source, status, data) in [
        ("user", "done", json!({"amount":100,"currency":"INR"})),
        ("user", "planned", json!({"amount":999,"currency":"INR"})),
        (
            "spotify",
            "done",
            json!({"connection_id":connection,"provider_data":{"action":"listen"}}),
        ),
    ] {
        sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,'Fixture',$2,$3,'expense',now(),$4)").bind(id).bind(status).bind(source).bind(data).execute(db.pool()).await.unwrap();
    }
    let profiles = crate::storage::pulse::PulseRepository::new(db.pool().clone())
        .profiles(id)
        .await
        .unwrap();
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].count, 1);
    assert_eq!(
        profiles[0].fields.get("amount").map(String::as_str),
        Some("number")
    );
    assert_eq!(profiles[0].currency, "INR");
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn previews_count_plays_without_numeric_fields_and_keep_currency_separate() {
    let db = database().await;
    let id = user(&db).await;
    for data in [
        json!({"provider_data":{"action":"listen"}}),
        json!({"provider_data":{"action":"listen"}}),
        json!({"provider_data":{"action":"listen"}}),
    ] {
        sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,'Song','done','spotify','music',now(),$2)").bind(id).bind(data).execute(db.pool()).await.unwrap();
    }
    for (amount, currency) in [(100, "INR"), (200, "INR"), (7, "USD")] {
        sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,'Expense','done','user','expense',now(),$2)").bind(id).bind(json!({"amount":amount,"currency":currency})).execute(db.pool()).await.unwrap();
    }
    let profiles = crate::storage::pulse::PulseRepository::new(db.pool().clone())
        .profiles(id)
        .await
        .unwrap();
    let catalog = measurement_catalog(&profiles);
    let measurements: Vec<_> = catalog
        .into_iter()
        .filter(|m| m.profile.source == "spotify" || m.field.as_deref() == Some("amount"))
        .collect();
    let inputs: Vec<_> = measurements
        .into_iter()
        .map(|m| {
            (
                PulseDefinition {
                    version: 2,
                    measurement_id: m.id.clone(),
                    bucket: Some(Bucket::Day),
                    dimension: None,
                    period_days: 30,
                    offset_days: 0,
                    timezone: "Asia/Kolkata".into(),
                    chart_type: ChartType::Bar,
                },
                m,
            )
        })
        .collect();
    let results = super::execution::execute(db.pool(), id, &inputs)
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(
        results
            .iter()
            .find(|r| r.unit == "events")
            .unwrap()
            .points
            .iter()
            .filter_map(|p| p.value)
            .sum::<f64>(),
        3.0
    );
    assert_eq!(
        results
            .iter()
            .find(|r| r.unit == "INR")
            .unwrap()
            .points
            .iter()
            .filter_map(|p| p.value)
            .sum::<f64>(),
        300.0
    );
    assert_eq!(
        results
            .iter()
            .find(|r| r.unit == "USD")
            .unwrap()
            .points
            .iter()
            .filter_map(|p| p.value)
            .sum::<f64>(),
        7.0
    );
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn discovery_save_cache_and_revocation_are_scoped() {
    use super::service::PulseService;
    use crate::{domain::identity::Actor, storage::pulse::PulseRepository};
    let db = database().await;
    let id = user(&db).await;
    let other = user(&db).await;
    let conn:uuid::Uuid=sqlx::query_scalar("INSERT INTO vox_connections(user_id,connector_id,consented_at) VALUES($1,'spotify',now()) RETURNING id").bind(id).fetch_one(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,'Song','done','spotify','music',now(),$2)").bind(id).bind(json!({"connection_id":conn,"provider_data":{"action":"listen"}})).execute(db.pool()).await.unwrap();
    let service = PulseService::new(PulseRepository::new(db.pool().clone()));
    let actor = Actor::user(id);
    let input = DiscoveryInput {
        timezone: "Asia/Kolkata".into(),
        refresh: false,
        more: false,
        prompt: None,
    };
    let first = service.discover(&actor, input.clone()).await.unwrap();
    assert!(!first.suggestions.is_empty());
    let second = service.discover(&actor, input.clone()).await.unwrap();
    assert_eq!(second.computed_at, first.computed_at);
    let save = SavePulseInput {
        idempotency_key: uuid::Uuid::new_v4(),
        title: "Music habits".into(),
        definition: first.suggestions[0].definition.clone(),
    };
    let chart = service.save(&actor, save.clone()).await.unwrap();
    let replay = service.save(&actor, save.clone()).await.unwrap();
    assert_eq!(chart.id, replay.id);
    let canvas = service
        .canvas(&actor, "Asia/Kolkata", false, None)
        .await
        .unwrap();
    assert_eq!(canvas.charts.len(), 1);
    assert!(
        canvas.charts[0]
            .result
            .as_ref()
            .unwrap()
            .points
            .iter()
            .any(|p| p.value == Some(1.0))
    );
    let other_canvas = service
        .canvas(&Actor::user(other), "Asia/Kolkata", false, None)
        .await
        .unwrap();
    assert!(other_canvas.charts.is_empty());
    sqlx::query("UPDATE vox_connections SET assistant_read=false WHERE id=$1")
        .bind(conn)
        .execute(db.pool())
        .await
        .unwrap();
    let revoked = service
        .canvas(&actor, "Asia/Kolkata", false, None)
        .await
        .unwrap();
    assert!(revoked.charts[0].result.as_ref().unwrap().error.is_some());
    assert!(
        service
            .save(
                &actor,
                SavePulseInput {
                    idempotency_key: uuid::Uuid::new_v4(),
                    ..save
                }
            )
            .await
            .is_err()
    );
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn known_intervals_split_at_local_midnight_and_undated_entries_stay_unknown() {
    let db = database().await;
    let id = user(&db).await;
    let now = chrono::Utc::now();
    let date = now.date_naive() - chrono::Duration::days(2);
    let start = date.and_hms_opt(17, 30, 0).unwrap().and_utc();
    let end = date.and_hms_opt(19, 30, 0).unwrap().and_utc();
    sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,end_at) VALUES($1,'Work','done','user','work',$2,$3)").bind(id).bind(start).bind(end).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO spans(user_id,title,status,source,category,data) VALUES($1,'Undated','done','user','work','{}')").bind(id).execute(db.pool()).await.unwrap();
    let profiles = crate::storage::pulse::PulseRepository::new(db.pool().clone())
        .profiles(id)
        .await
        .unwrap();
    let m = measurement_catalog(&profiles)
        .into_iter()
        .find(|m| m.kind == MeasurementKind::KnownIntervalDuration)
        .unwrap();
    let d = PulseDefinition {
        version: 2,
        measurement_id: m.id.clone(),
        bucket: Some(Bucket::Day),
        dimension: None,
        period_days: 7,
        offset_days: 0,
        timezone: "Asia/Kolkata".into(),
        chart_type: ChartType::Line,
    };
    let results = super::execution::execute(db.pool(), id, &[(d, m)])
        .await
        .unwrap();
    let points = &results[0].points;
    assert_eq!(
        points
            .iter()
            .find(|p| p.label == date.to_string())
            .unwrap()
            .value,
        Some(1.0)
    );
    assert_eq!(
        points
            .iter()
            .find(|p| p.label == (date + chrono::Duration::days(1)).to_string())
            .unwrap()
            .value,
        Some(1.0)
    );
    assert_eq!(points.iter().filter(|p| p.value.is_some()).count(), 2);
    assert_eq!(results[0].undated_count, 1);
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn cached_chart_changes_after_edit_and_delete_and_conflicting_save_is_rejected() {
    use super::service::PulseService;
    use crate::{domain::identity::Actor, storage::pulse::PulseRepository};
    let db = database().await;
    let id = user(&db).await;
    let actor = Actor::user(id);
    let span:uuid::Uuid=sqlx::query_scalar("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,'Expense','done','user','expense',now(),'{\"amount\":100,\"currency\":\"INR\"}') RETURNING id").bind(id).fetch_one(db.pool()).await.unwrap();
    let service = PulseService::new(PulseRepository::new(db.pool().clone()));
    let m = service
        .measurements(&actor, "Asia/Kolkata")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.unit == "INR")
        .unwrap();
    let d = PulseDefinition {
        version: 2,
        measurement_id: m.id.clone(),
        bucket: Some(Bucket::Day),
        dimension: None,
        period_days: 7,
        offset_days: 0,
        timezone: "Asia/Kolkata".into(),
        chart_type: ChartType::Bar,
    };
    let save = SavePulseInput {
        idempotency_key: uuid::Uuid::new_v4(),
        title: "Expenses".into(),
        definition: d.clone(),
    };
    service.save(&actor, save.clone()).await.unwrap();
    let first = service
        .canvas(&actor, "Asia/Kolkata", false, None)
        .await
        .unwrap();
    assert_eq!(
        first.charts[0]
            .result
            .as_ref()
            .unwrap()
            .points
            .iter()
            .filter_map(|p| p.value)
            .sum::<f64>(),
        100.0
    );
    assert!(
        service
            .save(
                &actor,
                SavePulseInput {
                    title: "Conflicting".into(),
                    ..save
                }
            )
            .await
            .is_err()
    );
    sqlx::query("UPDATE spans SET data=data||'{\"amount\":250}' WHERE id=$1")
        .bind(span)
        .execute(db.pool())
        .await
        .unwrap();
    let edited = service
        .canvas(&actor, "Asia/Kolkata", false, None)
        .await
        .unwrap();
    assert_eq!(
        edited.charts[0]
            .result
            .as_ref()
            .unwrap()
            .points
            .iter()
            .filter_map(|p| p.value)
            .sum::<f64>(),
        250.0
    );
    sqlx::query("DELETE FROM spans WHERE id=$1")
        .bind(span)
        .execute(db.pool())
        .await
        .unwrap();
    let deleted = service
        .canvas(&actor, "Asia/Kolkata", false, None)
        .await
        .unwrap();
    assert!(deleted.charts[0].result.as_ref().unwrap().error.is_some());
}
#[test]
fn financial_pie_is_rejected_because_refunds_can_be_signed() {
    let p = profile("user", "debit", json!({"amount":"number"}));
    let mut p = p;
    p.currency = "INR".into();
    let catalog = measurement_catalog(&[p]);
    let m = catalog.iter().find(|m| m.unit == "INR").unwrap();
    let mut m = m.clone();
    m.dimensions.push("merchant".into());
    let catalog = vec![m.clone()];
    let d = PulseDefinition {
        version: 2,
        measurement_id: m.id.clone(),
        bucket: None,
        dimension: Some("merchant".into()),
        period_days: 30,
        offset_days: 0,
        timezone: "Asia/Kolkata".into(),
        chart_type: ChartType::Pie,
    };
    assert!(validate_definition(&d, &catalog).is_err());
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL and pg_stat_statements"]
async fn six_chart_database_budget_and_fixture_performance() {
    use super::service::PulseService;
    use crate::{domain::identity::Actor, storage::pulse::PulseRepository};
    let db = database().await;
    for size in [1_000i64, 100_000i64] {
        let id = user(&db).await;
        let actor = Actor::user(id);
        sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) SELECT $1,'Expense','done','user','expense',now()-(g%29)*interval '1 day',jsonb_build_object('amount',1+(g%100),'currency','INR','merchant','Merchant '||(g%5)) FROM generate_series(1,$2)g").bind(id).bind(size).execute(db.pool()).await.unwrap();
        let repo = PulseRepository::new(db.pool().clone());
        let m = measurement_catalog(&repo.profiles(id).await.unwrap())
            .into_iter()
            .find(|m| m.unit == "INR")
            .unwrap();
        for (period, bucket, dimension) in [
            (7, Some(Bucket::Day), None),
            (30, Some(Bucket::Day), None),
            (90, Some(Bucket::Week), None),
            (365, Some(Bucket::Month), None),
            (30, None, Some("merchant")),
            (90, None, Some("merchant")),
        ] {
            let d = PulseDefinition {
                version: 2,
                measurement_id: m.id.clone(),
                bucket,
                dimension: dimension.map(str::to_owned),
                period_days: period,
                offset_days: 0,
                timezone: "Asia/Kolkata".into(),
                chart_type: ChartType::Bar,
            };
            sqlx::query("INSERT INTO pulse_saved_charts(user_id,idempotency_key,title,definition,definition_hash) VALUES($1,$2,'Expense',$3,$4)").bind(id).bind(uuid::Uuid::new_v4()).bind(json!(d)).bind(super::measurements::definition_hash(&d)).execute(db.pool()).await.unwrap();
        }
        let service = PulseService::new(repo);
        async fn calls(db: &crate::db::Db) -> i64 {
            sqlx::query_scalar("SELECT COALESCE(sum(calls),0)::bigint FROM pg_stat_statements WHERE dbid=(SELECT oid FROM pg_database WHERE datname=current_database()) AND query NOT LIKE '%pg_stat_statements%'").fetch_one(db.pool()).await.unwrap()
        }
        let before = calls(&db).await;
        let start = std::time::Instant::now();
        let cold = service
            .canvas(&actor, "Asia/Kolkata", false, None)
            .await
            .unwrap();
        let cold_ms = start.elapsed().as_secs_f64() * 1000.;
        let cold_calls = calls(&db).await - before;
        assert_eq!(cold.charts.len(), 6);
        assert!(cold_calls <= 6, "cold calls: {cold_calls}");
        let mut warm_times = vec![];
        let before = calls(&db).await;
        for _ in 0..20 {
            let start = std::time::Instant::now();
            let warm = service
                .canvas(&actor, "Asia/Kolkata", false, None)
                .await
                .unwrap();
            assert_eq!(
                warm.charts[0].result.as_ref().unwrap().computed_at,
                cold.charts[0].result.as_ref().unwrap().computed_at
            );
            warm_times.push(start.elapsed().as_secs_f64() * 1000.);
        }
        let warm_calls = calls(&db).await - before;
        assert_eq!(warm_calls, 20);
        warm_times.sort_by(f64::total_cmp);
        let before = calls(&db).await;
        let start = std::time::Instant::now();
        service
            .canvas(&actor, "Asia/Kolkata", true, None)
            .await
            .unwrap();
        let refresh_ms = start.elapsed().as_secs_f64() * 1000.;
        let refresh_calls = calls(&db).await - before;
        assert!(refresh_calls <= 6);
        println!(
            "PULSE_PERF rows={size} charts=6 cold_calls={cold_calls} cold_ms={cold_ms:.2} warm_calls_per_load={} warm_p50_ms={:.2} warm_p95_ms={:.2} refresh_calls={refresh_calls} refresh_ms={refresh_ms:.2}",
            warm_calls / 20,
            warm_times[10],
            warm_times[18]
        );
    }
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn gameplay_totals_and_deltas_are_distinct_and_subscriptions_are_projections() {
    let db = database().await;
    let id = user(&db).await;
    let now = chrono::Utc::now();
    for data in [
        json!({"game":"Game","total_seconds":7200,"timing":"first_to_last_played"}),
        json!({"game":"Game","duration_seconds":1800,"timing":"observed_counter_delta","observation_start":now-chrono::Duration::days(1),"observation_end":now}),
    ] {
        sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,'Game','done','playstation','gaming',CASE WHEN $2->>'timing'='first_to_last_played' THEN now()-interval '1000 days' ELSE NULL END,$2)").bind(id).bind(data).execute(db.pool()).await.unwrap();
    }
    for (title, amount, interval, active) in [
        ("Annual", 1200, "yearly", true),
        ("Monthly", 100, "monthly", true),
        ("Cancelled", 900, "monthly", false),
        ("Unknown", 500, "unknown", true),
    ] {
        sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,$2,'done','user','subscription',now(),$3)").bind(id).bind(title).bind(json!({"amount":amount,"currency":"INR","billing_interval":interval,"active":active})).execute(db.pool()).await.unwrap();
    }
    let profiles = crate::storage::pulse::PulseRepository::new(db.pool().clone())
        .profiles(id)
        .await
        .unwrap();
    let catalog = measurement_catalog(&profiles);
    let inputs: Vec<_> = catalog
        .into_iter()
        .filter(|m| {
            m.kind == MeasurementKind::NumericSum
                || m.kind == MeasurementKind::RecurringCostProjection
        })
        .map(|m| {
            let d = PulseDefinition {
                version: 2,
                measurement_id: m.id.clone(),
                bucket: None,
                dimension: m.default_dimension.clone(),
                period_days: 30,
                offset_days: 0,
                timezone: "Asia/Kolkata".into(),
                chart_type: ChartType::Bar,
            };
            assert!(validate_definition(&d, std::slice::from_ref(&m)).is_ok());
            (d, m)
        })
        .collect();
    let results = super::execution::execute(db.pool(), id, &inputs)
        .await
        .unwrap();
    assert!(
        results
            .iter()
            .any(|r| r.unit == "hours"
                && r.points.iter().filter_map(|p| p.value).sum::<f64>() == 2.0)
    );
    assert!(
        results
            .iter()
            .any(|r| r.unit == "hours"
                && r.points.iter().filter_map(|p| p.value).sum::<f64>() == 0.5)
    );
    let projected = results.iter().find(|r| r.quality == "projected").unwrap();
    assert_eq!(
        projected.points.iter().filter_map(|p| p.value).sum::<f64>(),
        200.0
    );
    assert!(
        !projected
            .points
            .iter()
            .any(|p| p.label == "Cancelled" || p.label == "Unknown")
    );
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn lifetime_gameplay_sums_games_when_grouped_by_platform() {
    let db = database().await;
    let id = user(&db).await;
    for (title, hours) in [("Game A", 10), ("Game B", 20)] {
        sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,$2,'done','playstation','gaming',now(),$3)").bind(id).bind(title).bind(json!({"game":title,"platform":"PS5","title_id":title,"total_seconds":hours*3600,"timing":"first_to_last_played"})).execute(db.pool()).await.unwrap();
    }
    let profiles = crate::storage::pulse::PulseRepository::new(db.pool().clone())
        .profiles(id)
        .await
        .unwrap();
    let m = measurement_catalog(&profiles)
        .into_iter()
        .find(|m| m.field.as_deref() == Some("total_seconds"))
        .unwrap();
    let d = PulseDefinition {
        version: 2,
        measurement_id: m.id.clone(),
        bucket: None,
        dimension: Some("platform".into()),
        period_days: 30,
        offset_days: 0,
        timezone: "Asia/Kolkata".into(),
        chart_type: ChartType::Bar,
    };
    let results = super::execution::execute(db.pool(), id, &[(d, m)])
        .await
        .unwrap();
    assert!((results[0].points[0].value.unwrap() - 30.0).abs() < 1e-9);
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn connected_and_imported_activity_have_distinct_measurement_identity() {
    let db = database().await;
    let id = user(&db).await;
    let conn:uuid::Uuid=sqlx::query_scalar("INSERT INTO vox_connections(user_id,connector_id,consented_at) VALUES($1,'spotify',now()) RETURNING id").bind(id).fetch_one(db.pool()).await.unwrap();
    for data in [
        json!({"provider_data":{"action":"listen"}}),
        json!({"connection_id":conn,"provider_data":{"action":"listen"}}),
    ] {
        sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,'Song','done','spotify','music',now(),$2)").bind(id).bind(data).execute(db.pool()).await.unwrap();
    }
    let repo = crate::storage::pulse::PulseRepository::new(db.pool().clone());
    let profiles = repo.profiles(id).await.unwrap();
    assert_eq!(profiles.len(), 2);
    let catalog = measurement_catalog(&profiles);
    assert_ne!(catalog[0].id, catalog[1].id);
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn old_high_count_sources_do_not_hide_recent_suggestions() {
    use super::service::PulseService;
    use crate::{domain::identity::Actor, storage::pulse::PulseRepository};
    let db = database().await;
    let id = user(&db).await;
    for index in 0..13 {
        sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at) SELECT $1,'Old','done','user',$2,now()-interval '1000 days' FROM generate_series(1,10)").bind(id).bind(format!("old{index}")).execute(db.pool()).await.unwrap();
    }
    sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at) VALUES($1,'Recent','done','user','recent',now())").bind(id).execute(db.pool()).await.unwrap();
    let service = PulseService::new(PulseRepository::new(db.pool().clone()));
    let response = service
        .discover(
            &Actor::user(id),
            DiscoveryInput {
                timezone: "Asia/Kolkata".into(),
                refresh: false,
                more: false,
                prompt: None,
            },
        )
        .await
        .unwrap();
    assert!(
        response
            .suggestions
            .iter()
            .any(|s| s.measurement.profile.category == "recent")
    );
}
#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn concurrent_forced_loads_share_the_same_computed_result_across_services() {
    use super::service::PulseService;
    use crate::{domain::identity::Actor, storage::pulse::PulseRepository};
    let db = database().await;
    let id = user(&db).await;
    let actor = Actor::user(id);
    let repo = PulseRepository::new(db.pool().clone());
    sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) SELECT $1,'Expense','done','user','expense',now(),'{\"amount\":100,\"currency\":\"INR\"}'::jsonb FROM generate_series(1,5000)").bind(id).execute(db.pool()).await.unwrap();
    let service = PulseService::new(repo.clone());
    let m = service
        .measurements(&actor, "Asia/Kolkata")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.unit == "INR")
        .unwrap();
    let d = PulseDefinition {
        version: 2,
        measurement_id: m.id.clone(),
        bucket: Some(Bucket::Day),
        dimension: None,
        period_days: 30,
        offset_days: 0,
        timezone: "Asia/Kolkata".into(),
        chart_type: ChartType::Bar,
    };
    service
        .save(
            &actor,
            SavePulseInput {
                idempotency_key: uuid::Uuid::new_v4(),
                title: "Expense".into(),
                definition: d,
            },
        )
        .await
        .unwrap();
    let other_service = PulseService::new(repo);
    let (a, b) = tokio::join!(
        service.canvas(&actor, "Asia/Kolkata", true, None),
        other_service.canvas(&actor, "Asia/Kolkata", true, None)
    );
    assert_eq!(
        a.unwrap().charts[0].result.as_ref().unwrap().computed_at,
        b.unwrap().charts[0].result.as_ref().unwrap().computed_at
    );
}

#[tokio::test]
#[ignore = "requires disposable TEST_DATABASE_URL"]
async fn deleting_a_user_cascades_pulse_revisions_without_recreating_the_owner() {
    let db = database().await;
    let id = user(&db).await;
    sqlx::query("INSERT INTO spans(user_id,title,status,source,category,start_at,data) VALUES($1,'Play','done','spotify','music',now(),'{}')")
        .bind(id).execute(db.pool()).await.unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pulse_revisions WHERE user_id=$1")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 0);
}
