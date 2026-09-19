use super::ScheduleId;
use crate::{
    agents::event_planner::{EventPlanning, EventPlanningPrompt, PlannedAction},
    db::Db,
    identity::UserId,
    jev::JevClient,
    memory::MemoryService,
};
use chrono::{DateTime, Utc};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct ScheduleHandler {
    db: Db,
    planner: Arc<dyn EventPlanning>,
    memory: MemoryService,
    jev: Option<JevClient>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleHandlerError {
    #[error("schedule storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("planner agent unavailable")]
    Agent(#[from] crate::agents::AgentError),
    #[error("schedule not found")]
    NotFound,
}

impl ScheduleHandler {
    pub fn new(db: Db, planner: Arc<dyn EventPlanning>) -> Self {
        let memory = MemoryService::new(db.clone(), None);
        Self::with_memory(db, planner, memory)
    }

    pub fn with_memory(db: Db, planner: Arc<dyn EventPlanning>, memory: MemoryService) -> Self {
        Self {
            db,
            planner,
            memory,
            jev: None,
        }
    }

    pub fn with_jev(
        db: Db,
        planner: Arc<dyn EventPlanning>,
        memory: MemoryService,
        jev: Option<JevClient>,
    ) -> Self {
        Self {
            db,
            planner,
            memory,
            jev,
        }
    }

    pub async fn handle(
        &self,
        schedule_id: ScheduleId,
        occurrence_at: DateTime<Utc>,
    ) -> Result<(), ScheduleHandlerError> {
        let row = sqlx::query("SELECT user_id, instruction FROM scheduled_tasks WHERE id = $1")
            .bind(schedule_id.0)
            .fetch_optional(self.db.pool())
            .await?;

        let row = row.ok_or(ScheduleHandlerError::NotFound)?;
        let user_id = UserId(row.get("user_id"));
        let instruction: String = row.get("instruction");

        // Jev System 1 Pre-Filter: check if this occurrence requires an external action or phone call
        if let Some(jev) = &self.jev {
            let state = serde_json::json!({
                "instruction": instruction,
                "occurrence_at": occurrence_at.to_rfc3339(),
            });
            if let Ok(needs_planning) = jev.noul(
                state,
                "Does this scheduled task instruction specifically require taking an external action, dispatching an alert, or placing a telephone call?",
            ).await
                && needs_planning < 0.20 {
                    tracing::info!(
                        schedule_id = %schedule_id.0,
                        needs_planning,
                        "Jev System 1: scheduled occurrence requires no external action, skipping Gemini planner"
                    );
                    return Ok(());
                }
        }

        let actions = self
            .planner
            .plan(EventPlanningPrompt {
                user_id,
                user_context: self.memory.load(user_id).await?,
                event_type: "scheduled_task".into(),
                occurred_at: occurrence_at,
                payload: serde_json::json!({ "instruction": instruction }),
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
                    serde_json::json!({
                        "reason": reason,
                        "opening_instruction": opening_instruction
                    }),
                ),
            };
            let key = format!(
                "schedule:{}:occurrence:{}:action:{index}",
                schedule_id.0,
                occurrence_at.to_rfc3339()
            );
            let inserted = sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO actions (user_id, schedule_id, kind, payload, idempotency_key) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (idempotency_key) DO NOTHING RETURNING id",
            )
            .bind(user_id.0)
            .bind(schedule_id.0)
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
        tx.commit().await?;
        Ok(())
    }
}
