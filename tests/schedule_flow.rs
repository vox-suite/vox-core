use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;
use vox_core::{
    agents::{
        AgentError,
        event_planner::{EventPlanning, EventPlanningPrompt, PlannedAction},
    },
    db::Db,
    identity::{DeploymentId, HostAppId, IdentityService, ResourceOwner, UserContextSubject},
    schedules::{
        CreateScheduleRequest, ScheduleKind, UpdateScheduleRequest, handler::ScheduleHandler,
        service::ScheduleService, ticker::ScheduleTicker,
    },
};

struct CallPlanner;

#[async_trait]
impl EventPlanning for CallPlanner {
    async fn plan(&self, _: EventPlanningPrompt) -> Result<Vec<PlannedAction>, AgentError> {
        Ok(vec![PlannedAction::OutboundCall {
            reason: "Scheduled update".into(),
            opening_instruction: "Give the scheduled update".into(),
        }])
    }
}

async fn setup() -> Db {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    db
}

async fn owner(db: &Db, host_user_id: &str) -> ResourceOwner {
    let deployment_id: Uuid = sqlx::query_scalar("INSERT INTO platform_deployments (external_key) VALUES ('test.schedules') ON CONFLICT (external_key) DO UPDATE SET external_key=EXCLUDED.external_key RETURNING id").fetch_one(db.pool()).await.unwrap();
    let host_app_id: Uuid = sqlx::query_scalar("INSERT INTO host_apps (deployment_id, external_key) VALUES ($1, 'test.host') ON CONFLICT (deployment_id, external_key) DO UPDATE SET external_key=EXCLUDED.external_key RETURNING id").bind(deployment_id).fetch_one(db.pool()).await.unwrap();
    IdentityService::new(db.clone())
        .resolve_context(&UserContextSubject {
            deployment_id: DeploymentId(deployment_id),
            host_app_id: HostAppId(host_app_id),
            organization_id: None,
            host_user_id: host_user_id.into(),
        })
        .await
        .unwrap()
        .owner()
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn recurring_schedule_uses_timezone_and_claims_each_occurrence_once() {
    let db = setup().await;
    let service = ScheduleService::new(db.clone());
    let ticker = ScheduleTicker::new(db.clone());
    let handler = ScheduleHandler::new(db.clone(), Arc::new(CallPlanner));
    let now = at("2026-09-13T03:00:00Z");
    let owner = owner(&db, "phone:+919999999999").await;

    let schedule = service
        .create_at(
            owner,
            CreateScheduleRequest {
                instruction: "Daily briefing".into(),
                schedule_kind: ScheduleKind::Recurring,
                run_at: None,
                recurrence_expression: Some("0 30 9 * * * *".into()),
                timezone: "Asia/Kolkata".into(),
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(schedule.next_run_at, Some(at("2026-09-13T04:00:00Z")));

    ticker.tick(at("2026-09-13T04:00:00Z")).await.unwrap();
    ticker.tick(at("2026-09-13T04:00:00Z")).await.unwrap();
    let first_occurrence = sqlx::query_scalar::<_, DateTime<Utc>>(
        "SELECT occurrence_at FROM jobs WHERE kind = 'run_schedule' ORDER BY occurrence_at LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    handler.handle(schedule.id, first_occurrence).await.unwrap();
    handler.handle(schedule.id, first_occurrence).await.unwrap();

    ticker.tick(at("2026-09-14T04:00:00Z")).await.unwrap();
    let second_occurrence = sqlx::query_scalar::<_, DateTime<Utc>>(
        "SELECT occurrence_at FROM jobs WHERE kind = 'run_schedule' ORDER BY occurrence_at DESC LIMIT 1",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    handler
        .handle(schedule.id, second_occurrence)
        .await
        .unwrap();

    let run_jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'run_schedule'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(run_jobs, 2);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn validates_creation_and_accepts_exactly_one_update_operation() {
    let db = setup().await;
    let service = ScheduleService::new(db.clone());
    let now = Utc.with_ymd_and_hms(2026, 9, 13, 10, 0, 0).unwrap();
    let owner = owner(&db, "phone:+919999999998").await;

    let invalid_zone = service
        .create_at(
            owner,
            CreateScheduleRequest {
                instruction: "Brief me".into(),
                schedule_kind: ScheduleKind::Recurring,
                run_at: None,
                recurrence_expression: Some("not cron".into()),
                timezone: "Mars/Base".into(),
            },
            now,
        )
        .await;
    assert!(invalid_zone.is_err());

    let once = service
        .create_at(
            owner,
            CreateScheduleRequest {
                instruction: "One-time reminder".into(),
                schedule_kind: ScheduleKind::Once,
                run_at: Some(now + Duration::hours(1)),
                recurrence_expression: None,
                timezone: "Asia/Kolkata".into(),
            },
            now,
        )
        .await
        .unwrap();

    assert!(
        service
            .update_at(
                owner,
                once.id,
                UpdateScheduleRequest {
                    state: Some("paused".into()),
                    run_at: Some(now + Duration::hours(2)),
                    recurrence_expression: None,
                },
                now,
            )
            .await
            .is_err()
    );
    let paused = service
        .update_at(
            owner,
            once.id,
            UpdateScheduleRequest {
                state: Some("paused".into()),
                run_at: None,
                recurrence_expression: None,
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(paused.state, "paused");
    let _ = json!(paused);
}
