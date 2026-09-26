/**
* Handlers executed when scheduled cron triggers fire.
*/
use super::ScheduleId;
use crate::{
    agents::event_planner::EventPlanning,
    db::Db,
    identity::{ResourceOwner, UserContextId, UserId},
    jev::JevClient,
    memory::MemoryService,
    outbound::{OutboundCallService, OutboundError},
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
    outbound: Option<Arc<OutboundCallService>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleHandlerError {
    #[error("schedule storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("planner agent unavailable")]
    Agent(#[from] crate::agents::AgentError),
    #[error("schedule not found")]
    NotFound,
    #[error("scheduled external dispatch outcome is unknown")]
    Outbound(#[from] OutboundError),
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
            outbound: None,
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
            outbound: None,
        }
    }

    pub fn with_outbound(mut self, outbound: Arc<OutboundCallService>) -> Self {
        self.outbound = Some(outbound);
        self
    }

    pub async fn handle(
        &self,
        schedule_id: ScheduleId,
        occurrence_at: DateTime<Utc>,
    ) -> Result<(), ScheduleHandlerError> {
        let row = sqlx::query(
            "SELECT user_id, user_context_id, span_id, instruction FROM schedules WHERE id = $1",
        )
        .bind(schedule_id.0)
        .fetch_optional(self.db.pool())
        .await?;

        let row = row.ok_or(ScheduleHandlerError::NotFound)?;
        let user_id = UserId(row.get("user_id"));
        let owner = ResourceOwner {
            user_context_id: UserContextId(row.get("user_context_id")),
            user_id,
        };
        let span_id: Option<uuid::Uuid> = row.get("span_id");
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

        let user_context = self.memory.load(user_id).await.unwrap_or_default();
        let planned = self
            .planner
            .plan(crate::agents::event_planner::EventPlanningPrompt {
                user_id,
                user_context,
                event_type: "scheduled_task".into(),
                occurred_at: occurrence_at,
                payload: serde_json::json!({ "instruction": instruction }),
            })
            .await
            .unwrap_or_default();

        if self.outbound.is_some() {
            let claimed = sqlx::query(
                "INSERT INTO schedule_occurrence_dispatches (schedule_id,occurrence_at,state)
                 VALUES ($1,$2,'claimed') ON CONFLICT DO NOTHING",
            )
            .bind(schedule_id.0)
            .bind(occurrence_at)
            .execute(self.db.pool())
            .await?
            .rows_affected();
            if claimed == 0 {
                return Ok(());
            }
        }

        let dispatched = if !planned.is_empty() {
            let mut sent = false;
            for action in planned {
                match action {
                    crate::agents::event_planner::PlannedAction::OutboundCall {
                        reason,
                        opening_instruction,
                    } => {
                        if let Some(outbound) = &self.outbound {
                            outbound
                                .initiate_call_for_user(
                                    owner,
                                    &reason,
                                    &opening_instruction,
                                    Some(schedule_id.0),
                                    None,
                                )
                                .await?;
                            sent = true;
                        }
                    }
                }
            }
            sent
        } else if let Some(outbound) = &self.outbound {
            let reason = format!("Scheduled reminder: {}", instruction);
            let opening = format!(
                "Remind the user of their scheduled reminder: {}",
                instruction
            );
            outbound
                .initiate_call_for_user(owner, &reason, &opening, Some(schedule_id.0), None)
                .await?;
            true
        } else {
            false
        };
        if dispatched {
            sqlx::query(
                "UPDATE schedule_occurrence_dispatches SET state='dispatched',updated_at=now()
                 WHERE schedule_id=$1 AND occurrence_at=$2",
            )
            .bind(schedule_id.0)
            .bind(occurrence_at)
            .execute(self.db.pool())
            .await?;
        }

        let _ = sqlx::query(
            "UPDATE spans SET status = 'done', completed_at = now(), updated_at = now() \
             WHERE id = $1 AND user_context_id = $2 AND user_id = $3 \
               AND status IN ('planned', 'active')",
        )
        .bind(span_id)
        .bind(owner.user_context_id.0)
        .bind(user_id.0)
        .execute(self.db.pool())
        .await;

        Ok(())
    }
}
