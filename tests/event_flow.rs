/**
* Integration tests for end-to-end domain event dispatching.
*/
use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use std::sync::Arc;
use tower::ServiceExt;
use vox_core::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder},
        event_planner::{EventPlanning, EventPlanningPrompt, PlannedAction},
    },
    db::Db,
    events::{IngestEventRequest, handler::EventHandler, service::EventService},
    http::{AppState, router},
    identity::ChannelIdentity,
};

struct UnusedAgent;

#[async_trait]
impl ConversationResponder for UnusedAgent {
    async fn respond(&self, _prompt: ConversationPrompt) -> Result<String, AgentError> {
        unreachable!()
    }
}

struct CallPlanner;

#[async_trait]
impl EventPlanning for CallPlanner {
    async fn plan(&self, _prompt: EventPlanningPrompt) -> Result<Vec<PlannedAction>, AgentError> {
        Ok(vec![PlannedAction::OutboundCall {
            reason: "Weather warning".into(),
            opening_instruction: "Explain the warning".into(),
        }])
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn duplicate_event_ingestion_creates_one_event_and_one_job() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    let app = router(AppState::with_dependencies(
        db.clone(),
        Arc::new(UnusedAgent),
        "test-token".into(),
    ));
    let payload = r#"{"idempotency_key":"location-42","identity":{"channel":"vox-client","external_id":"device-1"},"event_type":"location_update","occurred_at":"2026-09-13T08:00:00Z","payload":{"place":"Pune"}}"#;

    let mut ids = Vec::new();
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::post("/v1/events")
                    .header("authorization", "Bearer test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 202);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        ids.push(serde_json::from_slice::<serde_json::Value>(&body).unwrap()["event_id"].clone());
    }
    assert_eq!(ids[0], ids[1]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind = 'process_event'")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn retrying_event_processing_is_idempotent_and_marks_events_processed() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    let event = EventService::new(db.clone())
        .ingest(IngestEventRequest {
            idempotency_key: "weather-42".into(),
            identity: ChannelIdentity {
                channel: "vox-client".into(),
                external_id: "device-1".into(),
            },
            event_type: "weather_update".into(),
            occurred_at: chrono::Utc::now(),
            payload: serde_json::json!({"place":"Pune"}),
        })
        .await
        .unwrap();
    let handler = EventHandler::new(db.clone(), Arc::new(CallPlanner));

    handler.handle(event.event_id).await.unwrap();
    handler.handle(event.event_id).await.unwrap();

    let processed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM inbound_events WHERE id = $1 AND processed_at IS NOT NULL",
    )
    .bind(event.event_id.0)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(processed, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM inbound_events")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        1
    );
}
