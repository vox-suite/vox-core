use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;
use vox_core::{
    agents::{
        AgentError,
        event_planner::{EventPlanning, EventPlanningPrompt, PlannedAction},
    },
    db::Db,
    events::{IngestEventRequest, handler::EventHandler, service::EventService},
    identity::{DeploymentId, HostAppId, IdentityService, ResourceOwner, UserContextSubject},
};

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

async fn owner(db: &Db, host_user_id: &str) -> ResourceOwner {
    let deployment_id: Uuid = sqlx::query_scalar("INSERT INTO platform_deployments (external_key) VALUES ('test.events') ON CONFLICT (external_key) DO UPDATE SET external_key=EXCLUDED.external_key RETURNING id").fetch_one(db.pool()).await.unwrap();
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
    let service = EventService::new(db.clone());
    let owner = owner(&db, "device-1").await;

    let mut ids = Vec::new();
    for _ in 0..2 {
        ids.push(
            service
                .ingest(
                    owner,
                    IngestEventRequest {
                        idempotency_key: "location-42".into(),
                        event_type: "location_update".into(),
                        occurred_at: chrono::DateTime::parse_from_rfc3339("2026-09-13T08:00:00Z")
                            .unwrap()
                            .with_timezone(&chrono::Utc),
                        payload: serde_json::json!({"place":"Pune"}),
                    },
                )
                .await
                .unwrap()
                .event_id,
        );
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
async fn retrying_event_processing_is_idempotent_without_legacy_actions() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    let event = EventService::new(db.clone())
        .ingest(
            owner(&db, "device-1").await,
            IngestEventRequest {
                idempotency_key: "weather-42".into(),
                event_type: "weather_update".into(),
                occurred_at: chrono::Utc::now(),
                payload: serde_json::json!({"place":"Pune"}),
            },
        )
        .await
        .unwrap();
    let handler = EventHandler::new(db.clone(), Arc::new(CallPlanner));

    handler.handle(event.event_id).await.unwrap();
    handler.handle(event.event_id).await.unwrap();

    let processed: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT processed_at FROM events WHERE id = $1")
            .bind(event.event_id.0)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(processed.is_some());
}
