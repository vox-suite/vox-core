use super::EventId;
use crate::{
    agents::event_planner::{EventPlanning, EventPlanningPrompt, PlannedAction},
    db::Db,
    identity::UserId,
};
use chrono::{DateTime, Utc};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct EventHandler {
    db: Db,
    planner: Arc<dyn EventPlanning>,
}

#[derive(Debug, thiserror::Error)]
pub enum EventHandlerError {
    #[error("event storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("event planner unavailable")]
    Agent(#[from] crate::agents::AgentError),
}

impl EventHandler {
    pub fn new(db: Db, planner: Arc<dyn EventPlanning>) -> Self {
        Self { db, planner }
    }

    pub async fn handle(&self, event_id: EventId) -> Result<(), EventHandlerError> {
        let row = sqlx::query(
            "SELECT user_id, event_type, occurred_at, payload FROM events WHERE id = $1",
        )
        .bind(event_id.0)
        .fetch_one(self.db.pool())
        .await?;
        let user_id = UserId(row.get("user_id"));
        let actions = self
            .planner
            .plan(EventPlanningPrompt {
                user_id,
                user_context: String::new(),
                event_type: row.get("event_type"),
                occurred_at: row.get::<DateTime<Utc>, _>("occurred_at"),
                payload: row.get("payload"),
            })
            .await?;

        let mut tx = self.db.pool().begin().await?;
        for (index, action) in actions.into_iter().enumerate() {
            let (kind, payload) = match action {
                PlannedAction::OutboundCall {
                    reason,
                    opening_instruction,
                } => (
                    "outbound_call",
                    serde_json::json!({"reason": reason, "opening_instruction": opening_instruction}),
                ),
            };
            let key = format!("event:{}:action:{index}", event_id.0);
            let inserted = sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO actions (user_id, event_id, kind, payload, idempotency_key) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (idempotency_key) DO NOTHING RETURNING id",
            )
            .bind(user_id.0)
            .bind(event_id.0)
            .bind(kind)
            .bind(payload)
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(action_id) = inserted {
                sqlx::query(
                    "INSERT INTO jobs (kind, payload_reference_id) VALUES ('dispatch_action', $1)",
                )
                .bind(action_id)
                .execute(&mut *tx)
                .await?;
            }
        }
        sqlx::query("UPDATE events SET processed_at = COALESCE(processed_at, now()) WHERE id = $1")
            .bind(event_id.0)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
