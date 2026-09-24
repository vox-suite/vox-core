/**
* Integration tests for recurring schedules and ticker execution.
*/
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
    bridge_client::{BridgeError, OutboundBridge, OutboundCallRequest, OutboundCallResponse},
    db::Db,
    identity::ChannelIdentity,
    outbound::OutboundCallService,
    schedules::{
        CreateScheduleRequest, ScheduleKind, UpdateScheduleRequest, handler::ScheduleHandler,
        service::ScheduleService, ticker::ScheduleTicker,
    },
};

#[derive(Clone, Default)]
struct MockBridge;

#[async_trait]
impl OutboundBridge for MockBridge {
    async fn initiate_outbound_call(
        &self,
        _: OutboundCallRequest,
    ) -> Result<OutboundCallResponse, BridgeError> {
        Ok(OutboundCallResponse {
            provider_call_id: format!("CA_{}", Uuid::new_v4().simple()),
        })
    }
}

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

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn recurring_schedule_uses_timezone_and_each_occurrence_creates_actions_once() {
    let db = setup().await;
    let service = ScheduleService::new(db.clone());
    let ticker = ScheduleTicker::new(db.clone());
    let outbound = Arc::new(OutboundCallService::new(
        db.clone(),
        Some(Arc::new(MockBridge)),
    ));
    let handler = ScheduleHandler::new(db.clone(), Arc::new(CallPlanner)).with_outbound(outbound);
    let now = at("2026-09-13T03:00:00Z");

    let schedule = service
        .create_at(
            CreateScheduleRequest {
                identity: ChannelIdentity {
                    channel: "phone".into(),
                    external_id: "+919999999999".into(),
                },
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
    let dispatch_jobs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'dispatch_action'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(run_jobs, 2);
    assert_eq!(dispatch_jobs, 2);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn validates_creation_and_accepts_exactly_one_update_operation() {
    let db = setup().await;
    let service = ScheduleService::new(db);
    let now = Utc.with_ymd_and_hms(2026, 9, 13, 10, 0, 0).unwrap();
    let identity = ChannelIdentity {
        channel: "phone".into(),
        external_id: "+919999999998".into(),
    };

    let invalid_zone = service
        .create_at(
            CreateScheduleRequest {
                identity: identity.clone(),
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
            CreateScheduleRequest {
                identity: identity.clone(),
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
                once.id,
                UpdateScheduleRequest {
                    identity: identity.clone(),
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
            once.id,
            UpdateScheduleRequest {
                identity,
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
