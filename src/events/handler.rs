use super::EventId;
use crate::{
    agents::{
        event_planner::{EventPlanning, EventPlanningPrompt, PlannedAction},
        tools::records::validate_data_against_schema,
    },
    db::Db,
    identity::{IdentityService, UserId},
    jev::{
        event_triage::{EventTriageAction, EventTriager},
        schema_classifier::{SchemaClassificationResult, SchemaClassifier},
    },
    memory::MemoryService,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct EventHandler {
    db: Db,
    planner: Arc<dyn EventPlanning>,
    memory: MemoryService,
    triager: Option<Arc<EventTriager>>,
    schema_classifier: Option<Arc<SchemaClassifier>>,
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
        let memory = MemoryService::new(db.clone(), None);
        Self::with_memory(db, planner, memory)
    }

    pub fn with_memory(db: Db, planner: Arc<dyn EventPlanning>, memory: MemoryService) -> Self {
        Self {
            db,
            planner,
            memory,
            triager: None,
            schema_classifier: None,
        }
    }

    pub fn with_jev(
        db: Db,
        planner: Arc<dyn EventPlanning>,
        memory: MemoryService,
        triager: Option<Arc<EventTriager>>,
        schema_classifier: Option<Arc<SchemaClassifier>>,
    ) -> Self {
        Self {
            db,
            planner,
            memory,
            triager,
            schema_classifier,
        }
    }

    pub async fn handle(&self, event_id: EventId) -> Result<(), EventHandlerError> {
        let row = sqlx::query(
            "SELECT user_id, event_type, occurred_at, payload FROM events WHERE id = $1",
        )
        .bind(event_id.0)
        .fetch_one(self.db.pool())
        .await?;
        let user_id = UserId(row.get("user_id"));
        let owner = IdentityService::new(self.db.clone())
            .owner_for_user(user_id)
            .await
            .map_err(|error| match error {
                crate::identity::IdentityError::Database(error) => error,
                other => sqlx::Error::Protocol(other.to_string()),
            })?;
        let event_type: String = row.get("event_type");
        let occurred_at: DateTime<Utc> = row.get("occurred_at");
        let payload: Value = row.get("payload");

        // 1. Jev System 1 Triage Gate
        if let Some(triager) = &self.triager {
            match triager.triage(&event_type, &payload).await {
                Ok(triage) => {
                    tracing::info!(
                        event_id = %event_id.0,
                        action = ?triage.action,
                        confidence = triage.confidence,
                        is_critical_alert = triage.is_critical_alert,
                        "Jev System 1: event triage decision"
                    );

                    // Ignore routine / benign telemetry without LLM overhead (confidence >= 0.80)
                    if triage.action == EventTriageAction::Ignore && triage.confidence >= 0.80 {
                        tracing::info!(event_id = %event_id.0, "Jev System 1: ignored routine event");
                        sqlx::query("UPDATE events SET processed_at = COALESCE(processed_at, now()) WHERE id = $1")
                            .bind(event_id.0)
                            .execute(self.db.pool())
                            .await?;
                        return Ok(());
                    }

                    // Direct ingestion fast-path for structured data records
                    if triage.action == EventTriageAction::StoreRecord
                        && let Some(classifier) = &self.schema_classifier
                        && let Ok(class_res) = classifier.classify(user_id.0, &payload).await
                    {
                        match class_res {
                            SchemaClassificationResult::Existing { schema, confidence } => {
                                if validate_data_against_schema(&schema.json_schema, &payload)
                                    .is_ok()
                                {
                                    tracing::info!(
                                        event_id = %event_id.0,
                                        schema = %schema.qualified_name,
                                        confidence,
                                        "Jev System 1: direct ingestion into user_records"
                                    );
                                    let _ = sqlx::query(
                                                "INSERT INTO user_records (user_id, schema_id, domain, entity_type, title, data, occurred_at, source) \
                                                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                                            )
                                            .bind(user_id.0)
                                            .bind(schema.id)
                                            .bind(&schema.namespace)
                                            .bind(&schema.name)
                                            .bind(format!("{} logged", schema.qualified_name))
                                            .bind(&payload)
                                            .bind(occurred_at)
                                            .bind("jev_system1_ingest")
                                            .execute(self.db.pool())
                                            .await;

                                    sqlx::query("UPDATE events SET processed_at = COALESCE(processed_at, now()) WHERE id = $1")
                                                .bind(event_id.0)
                                                .execute(self.db.pool())
                                                .await?;
                                    return Ok(());
                                }
                            }
                            SchemaClassificationResult::Novel { reason, .. } => {
                                tracing::info!(
                                    event_id = %event_id.0,
                                    reason,
                                    "Jev System 1: novel schema detected, escalating to Gemini (System 2)"
                                );
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "Jev triage evaluation failed, falling back to System 2 planner");
                }
            }
        }

        // 2. System 2 (Gemini Event Planner)
        let actions = self
            .planner
            .plan(EventPlanningPrompt {
                user_id,
                user_context: self.memory.load(user_id).await?,
                event_type,
                occurred_at,
                payload,
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
                "INSERT INTO actions (user_context_id, user_id, event_id, kind, payload, idempotency_key) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (idempotency_key) DO NOTHING RETURNING id",
            )
            .bind(owner.user_context_id.0)
            .bind(owner.user_id.0)
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
