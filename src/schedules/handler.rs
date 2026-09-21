use super::ScheduleId;
use crate::{
    agents::event_planner::EventPlanning,
    db::Db,
    identity::{ResourceOwner, UserContextId, UserId},
    jev::JevClient,
    memory::MemoryService,
};
use chrono::{DateTime, Utc};
use sqlx::Row;
use std::sync::Arc;

#[derive(Clone)]
pub struct ScheduleHandler {
    db: Db,
    #[allow(dead_code)]
    planner: Arc<dyn EventPlanning>,
    #[allow(dead_code)]
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
        let row = sqlx::query(
            "SELECT s.user_id, COALESCE(s.user_context_id, c.id) AS user_context_id, s.instruction \
             FROM scheduled_tasks s \
             JOIN user_contexts c ON c.user_id = s.user_id \
             WHERE s.id = $1",
        )
        .bind(schedule_id.0)
        .fetch_optional(self.db.pool())
        .await?;

        let row = row.ok_or(ScheduleHandlerError::NotFound)?;
        let user_id = UserId(row.get("user_id"));
        let _owner = ResourceOwner {
            user_context_id: UserContextId(row.get("user_context_id")),
            user_id,
        };
        let instruction: String = row.get("instruction");

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

        Ok(())
    }
}
