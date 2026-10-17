/**
* Handlers executed when scheduled cron triggers fire.
*/
use super::ScheduleId;
use crate::{
    agents::event_planner::EventPlanning,
    db::Db,
    identity::{ResourceOwner, UserContextId, UserId},
    jev::JevClient,
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
        Self::with_jev(db, planner, None)
    }

    pub fn with_jev(db: Db, planner: Arc<dyn EventPlanning>, jev: Option<JevClient>) -> Self {
        Self {
            db,
            planner,
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
            "SELECT s.user_id, s.user_context_id, s.span_id, s.instruction, \
                    sp.category AS span_category, sp.title AS span_title, sp.notes AS span_notes \
             FROM schedules s LEFT JOIN spans sp ON sp.id = s.span_id WHERE s.id = $1",
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
        let call_span = match (
            row.get::<Option<String>, _>("span_category").as_deref(),
            row.get::<Option<String>, _>("span_title"),
            row.get::<Option<String>, _>("span_notes"),
        ) {
            (Some("call"), Some(title), Some(notes)) if self.outbound.is_some() => {
                Some((title, notes))
            }
            _ => None,
        };

        if call_span.is_none()
            && let Some(jev) = &self.jev
        {
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

        let planned = if call_span.is_some() {
            Vec::new()
        } else {
            let user_context = String::new();
            self.planner
                .plan(crate::agents::event_planner::EventPlanningPrompt {
                    user_id,
                    user_context,
                    event_type: "scheduled_task".into(),
                    occurred_at: occurrence_at,
                    payload: serde_json::json!({ "instruction": instruction }),
                })
                .await?
        };

        // A valid empty action list is an explicit no-op. Provider errors above
        // stay errors, so neither outcome can become an unsolicited phone call.
        if call_span.is_none() && planned.is_empty() {
            return Ok(());
        }
        if self.outbound.is_none() {
            return Err(OutboundError::Unavailable.into());
        }

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

        let dispatch = async {
            Ok::<bool, OutboundError>(
                if let (Some((title, notes)), Some(outbound)) = (&call_span, &self.outbound) {
                    outbound
                        .initiate_call_for_user(owner, title, notes, Some(schedule_id.0), span_id)
                        .await?;
                    true
                } else if !planned.is_empty() {
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
                } else {
                    false
                },
            )
        }
        .await;
        let dispatched = match dispatch {
            Ok(dispatched) => dispatched,
            Err(error) => {
                if outbound_not_placed(&error) {
                    sqlx::query(
                        "DELETE FROM schedule_occurrence_dispatches WHERE schedule_id=$1 AND occurrence_at=$2",
                    )
                    .bind(schedule_id.0)
                    .bind(occurrence_at)
                    .execute(self.db.pool())
                    .await?;
                }
                return Err(error.into());
            }
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

fn outbound_not_placed(error: &OutboundError) -> bool {
    match error {
        OutboundError::Unavailable
        | OutboundError::Database(_)
        | OutboundError::NoPhoneNumber
        | OutboundError::InvalidInput(_) => true,
        OutboundError::Bridge(crate::bridge_client::BridgeError::Status { .. }) => true,
        OutboundError::Bridge(crate::bridge_client::BridgeError::Http(error)) => error.is_connect(),
        OutboundError::Bridge(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agents::{
            AgentError,
            event_planner::{EventPlanningPrompt, PlannedAction},
        },
        bridge_client::{BridgeError, OutboundBridge, OutboundCallRequest, OutboundCallResponse},
    };
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Planner(bool);
    #[async_trait]
    impl EventPlanning for Planner {
        async fn plan(&self, _: EventPlanningPrompt) -> Result<Vec<PlannedAction>, AgentError> {
            if self.0 {
                Err(AgentError::Provider)
            } else {
                Ok(vec![])
            }
        }
    }
    struct Bridge(AtomicUsize);
    #[async_trait]
    impl OutboundBridge for Bridge {
        async fn initiate_outbound_call(
            &self,
            _: OutboundCallRequest,
        ) -> Result<OutboundCallResponse, BridgeError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(OutboundCallResponse {
                provider_call_id: "fixture-call".into(),
            })
        }
        async fn send_verification_code(&self, _: &str, _: &str) -> Result<(), BridgeError> {
            Ok(())
        }
    }

    #[tokio::test]
    #[ignore = "requires isolated TEST_DATABASE_URL"]
    async fn failed_or_noop_planner_never_dispatches_a_call() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let user = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO users(id) VALUES($1)")
            .bind(user)
            .execute(db.pool())
            .await
            .unwrap();
        crate::identity::IdentityService::new(db.clone())
            .resolve_for_user(user)
            .await
            .unwrap();
        sqlx::query("INSERT INTO channel_identities(user_id,channel,normalized_external_id) VALUES($1,'phone',$2)").bind(user).bind(format!("1555{:07}", user.as_u128() % 10_000_000)).execute(db.pool()).await.unwrap();
        let schedule: uuid::Uuid = sqlx::query_scalar("INSERT INTO schedules(user_id,instruction,kind) VALUES($1,'Check whether anything is needed','once') RETURNING id").bind(user).fetch_one(db.pool()).await.unwrap();
        let bridge = Arc::new(Bridge(AtomicUsize::new(0)));
        for failed in [true, false] {
            let handler =
                ScheduleHandler::new(db.clone(), Arc::new(Planner(failed))).with_outbound(
                    Arc::new(OutboundCallService::new(db.clone(), Some(bridge.clone()))),
                );
            let result = handler.handle(ScheduleId(schedule), Utc::now()).await;
            if failed {
                assert!(matches!(
                    result,
                    Err(ScheduleHandlerError::Agent(AgentError::Provider))
                ));
            } else {
                assert!(result.is_ok());
            }
            assert_eq!(bridge.0.load(Ordering::SeqCst), 0);
        }
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(user)
            .execute(db.pool())
            .await
            .unwrap();
    }
}
