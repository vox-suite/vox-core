use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use vox_core::{
    application::pulse::service::PulseService,
    domain::{
        charts::ChartType,
        identity::Actor,
        pulse::*,
        spaces::{NodeState, SpaceState},
    },
    storage::{pulse::PulseRepository, spaces::SpaceRepository},
};

async fn database() -> PgPool {
    let pool = PgPool::connect(&std::env::var("VOX_SPACES_INSIGHTS_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    pool
}

#[tokio::test]
#[ignore = "requires isolated VOX_SPACES_INSIGHTS_TEST_DATABASE_URL"]
async fn spaces_ordering_filters_graph_and_tenant_scope() {
    let pool = database().await;
    let repo = SpaceRepository::new(pool.clone());
    let user = Uuid::new_v4();
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id) VALUES($1),($2)")
        .bind(user)
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    let spec = json!({"large":"agent context"});
    let a = repo
        .create_space(user, "Alpha", "First", SpaceState::Ideating, spec.clone())
        .await
        .unwrap();
    let b = repo
        .create_space(user, "Beta", "Second", SpaceState::Committed, spec.clone())
        .await
        .unwrap();
    repo.create_space(user, "Dropped", "Hidden", SpaceState::Dropped, spec)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE spaces SET created_at='2026-10-01',updated_at='2026-10-01' WHERE user_id=$1",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    let page = repo
        .list_spaces(user, 25, 0, None, None, None, false)
        .await
        .unwrap();
    let mut expected = vec![a.id, b.id];
    expected.sort_by(|a, b| b.cmp(a));
    assert_eq!(page.iter().map(|s| s.id).collect::<Vec<_>>(), expected);
    assert_eq!(page[0].agent_spec, json!({"large":"agent context"}));
    let summaries = repo
        .list_spaces(user, 25, 0, None, None, None, true)
        .await
        .unwrap();
    assert!(summaries.iter().all(|s| s.agent_spec == json!({})));
    assert_eq!(
        repo.list_spaces(user, 1, 1, None, None, None, true)
            .await
            .unwrap()[0]
            .id,
        expected[1]
    );
    assert_eq!(
        repo.list_spaces(
            user,
            25,
            0,
            Some("sEcOnD"),
            Some("committed"),
            Some("title"),
            true
        )
        .await
        .unwrap()[0]
            .id,
        b.id
    );
    assert_eq!(
        repo.list_spaces(user, 25, 0, None, Some("active"), Some("updated"), true)
            .await
            .unwrap()[0]
            .id,
        a.id
    );
    assert!(
        repo.list_spaces(user, 25, 0, Some("' OR true --"), None, None, true)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repo.list_spaces(other, 25, 0, None, None, None, true)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(repo.get_graph(other, a.id).await.unwrap().is_none());
    assert!(
        repo.get_graph(user, a.id)
            .await
            .unwrap()
            .unwrap()
            .nodes
            .is_empty()
    );
    assert_eq!(repo.count_spaces(user).await.unwrap().0, 2);
    let first = repo
        .add_node(
            a.id,
            "goal",
            "Goal",
            "Fixture",
            json!({}),
            NodeState::Done,
            json!({}),
            &[],
            json!({}),
        )
        .await
        .unwrap();
    let second = repo
        .add_node(
            a.id,
            "task",
            "Task",
            "Fixture",
            json!({}),
            NodeState::Done,
            json!({}),
            &[],
            json!({}),
        )
        .await
        .unwrap();
    repo.add_edge(a.id, first.id, second.id).await.unwrap();
    let graph = repo.get_graph(user, a.id).await.unwrap().unwrap();
    assert_eq!(graph.nodes.len(), 2);
    assert_eq!(graph.edges.len(), 1);
    assert_eq!(graph.edges[0].from_node, first.id);
    assert_eq!(graph.edges[0].to_node, second.id);
    sqlx::query("DELETE FROM users WHERE id=ANY($1)")
        .bind(&[user, other][..])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires isolated VOX_SPACES_INSIGHTS_TEST_DATABASE_URL"]
async fn spaces_indexes_cover_both_list_orders() {
    let pool = database().await;
    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE tablename='spaces'")
            .fetch_all(&pool)
            .await
            .unwrap();
    for order in [
        "(user_id, created_at DESC, id DESC)",
        "(user_id, updated_at DESC, id DESC)",
    ] {
        assert!(
            indexes.iter().any(|s| s.contains(order)),
            "missing complete Spaces ordering index: {order}"
        );
    }
}

async fn calls(pool: &PgPool, pattern: &str) -> i64 {
    sqlx::query_scalar("SELECT coalesce(sum(calls),0)::bigint FROM pg_stat_statements WHERE dbid=(SELECT oid FROM pg_database WHERE datname=current_database()) AND query LIKE $1")
        .bind(pattern).fetch_one(pool).await.unwrap()
}

#[tokio::test]
#[ignore = "requires isolated VOX_SPACES_INSIGHTS_TEST_DATABASE_URL with pg_stat_statements; run serially"]
async fn insights_rollups_are_batched_and_results_cache_is_written_once() {
    let pool = database().await;
    let user = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id) VALUES($1)")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO timeline_events(user_id,event_type_id,group_id,title,occurred_at,content) SELECT $1,t.id,t.group_id,'Synthetic listen',now()-n*interval '1 day','{}' FROM timeline_event_types t CROSS JOIN generate_series(0,89) n WHERE t.value='music' AND t.owner_user_id IS NULL")
        .bind(user).execute(&pool).await.unwrap();
    let repo = PulseRepository::new(pool.clone());
    let actor = Actor::user(user);
    let service = PulseService::new(repo.clone());
    let measurement = service
        .measurements(&actor, "UTC")
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.kind == MeasurementKind::EventCount)
        .expect("music count measurement");
    let definition = PulseDefinition {
        version: 2,
        measurement_id: measurement.id,
        bucket: Some(Bucket::Day),
        dimension: None,
        offset_days: 0,
        top_n: None,
        period_days: 90,
        timezone: "UTC".into(),
        chart_type: ChartType::Line,
        compare_with: vec![],
    };
    let before_rollups = calls(&pool, "INSERT INTO pulse_daily_aggregates%").await;
    let before_cache = calls(&pool, "WITH pruned AS%INSERT INTO pulse_cache%").await;
    let start = std::time::Instant::now();
    let first = service.preview(&actor, definition.clone()).await.unwrap();
    let cold_ms = start.elapsed().as_secs_f64() * 1000.;
    let rollups = calls(&pool, "INSERT INTO pulse_daily_aggregates%").await - before_rollups;
    let cache_writes = calls(&pool, "WITH pruned AS%INSERT INTO pulse_cache%").await - before_cache;
    assert_eq!(first.record_count, 90);
    assert_eq!(
        first.points.iter().filter_map(|p| p.value).sum::<f64>(),
        90.
    );
    let mut warm_times = Vec::new();
    for _ in 0..10 {
        let start = std::time::Instant::now();
        let warm = service.preview(&actor, definition.clone()).await.unwrap();
        warm_times.push(start.elapsed().as_secs_f64() * 1000.);
        assert_eq!(first.computed_at, warm.computed_at);
    }
    warm_times.sort_by(f64::total_cmp);
    println!(
        "INSIGHTS local debug 90 days: cold={cold_ms:.3}ms, warm median={:.3}ms, rollup insert statements={rollups}, result cache writes={cache_writes}",
        warm_times[5]
    );
    assert_eq!(rollups, 1, "daily rollups should use one INSERT statement");
    assert_eq!(
        cache_writes, 1,
        "a computed result should be cached only once"
    );
    let numeric = service
        .measurements(&actor, "UTC")
        .await
        .unwrap()
        .into_iter()
        .find(|m| {
            m.kind == MeasurementKind::NumericSum && m.field.as_deref() == Some("duration_seconds")
        })
        .unwrap();
    let missing = service
        .preview(
            &actor,
            PulseDefinition {
                measurement_id: numeric.id,
                ..definition.clone()
            },
        )
        .await
        .unwrap();
    assert!(
        missing.points.iter().all(|p| p.value.is_none()),
        "missing values must not become recorded zeroes"
    );
    sqlx::query("UPDATE timeline_events SET content=jsonb_build_object('artist',CASE WHEN extract(day FROM occurred_at)::int%2=0 THEN 'Artist A' ELSE 'Artist B' END) WHERE user_id=$1")
        .bind(user).execute(&pool).await.unwrap();
    let by_artist = service
        .preview(
            &actor,
            PulseDefinition {
                bucket: None,
                dimension: Some("artist".into()),
                chart_type: ChartType::Bar,
                ..definition.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(by_artist.points.len(), 2);
    assert_eq!(
        by_artist.points.iter().filter_map(|p| p.value).sum::<f64>(),
        90.
    );
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO users(id) VALUES($1)")
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    let foreign = PulseService::new(repo);
    assert!(
        foreign
            .measurements(&Actor::user(other), "UTC")
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(other)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM timeline_events WHERE user_id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires isolated VOX_SPACES_INSIGHTS_TEST_DATABASE_URL and benchmark SQL fixtures"]
async fn benchmark_spaces_summary_projection() {
    let pool = database().await;
    let repo = SpaceRepository::new(pool);
    let user = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
    let mut full_times = Vec::new();
    let mut summary_times = Vec::new();
    for i in 0..25 {
        let start = std::time::Instant::now();
        let full = repo
            .list_spaces(user, 25, 0, None, None, Some("updated"), false)
            .await
            .unwrap();
        let full_ms = start.elapsed().as_secs_f64() * 1000.;
        let start = std::time::Instant::now();
        let summary = repo
            .list_spaces(user, 25, 0, None, None, Some("updated"), true)
            .await
            .unwrap();
        let summary_ms = start.elapsed().as_secs_f64() * 1000.;
        assert_eq!(full.len(), 25, "seed the benchmark first");
        assert_eq!(
            full.iter().map(|s| s.id).collect::<Vec<_>>(),
            summary.iter().map(|s| s.id).collect::<Vec<_>>()
        );
        assert!(full[0].agent_spec["context"].as_str().unwrap().len() >= 8192);
        assert!(summary.iter().all(|s| s.agent_spec == json!({})));
        if i >= 5 {
            full_times.push(full_ms);
            summary_times.push(summary_ms);
        }
    }
    full_times.sort_by(f64::total_cmp);
    summary_times.sort_by(f64::total_cmp);
    println!(
        "SPACES local debug 25 rows, same new index: full spec median={:.3}ms; SQL summary median={:.3}ms",
        full_times[10], summary_times[10]
    );
}
