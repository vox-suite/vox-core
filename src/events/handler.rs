/**
* Domain event processing handlers and notification triggers.
*/
use super::EventId;
use crate::{
    agents::{event_planner::EventPlanning, tools::records::validate_data_against_schema},
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

#[derive(Clone)]
pub struct EventHandler {
    db: Db,
    #[allow(dead_code)]
    planner: Arc<dyn EventPlanning>,
    #[allow(dead_code)]
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
            "SELECT user_id, event_type, occurred_at, payload FROM inbound_events WHERE id = $1",
        )
        .bind(event_id.0)
        .fetch_one(self.db.pool())
        .await?;
        let user_id = UserId(row.get("user_id"));
        let _owner = IdentityService::new(self.db.clone())
            .owner_for_user(user_id)
            .await
            .map_err(|error| match error {
                crate::identity::IdentityError::Database(error) => error,
                other => sqlx::Error::Protocol(other.to_string()),
            })?;
        let event_type: String = row.get("event_type");
        let occurred_at: DateTime<Utc> = row.get("occurred_at");
        let payload: Value = row.get("payload");

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

                    if triage.action == EventTriageAction::Ignore && triage.confidence >= 0.80 {
                        tracing::info!(event_id = %event_id.0, "Jev System 1: ignored routine event");
                        sqlx::query("UPDATE inbound_events SET processed_at = COALESCE(processed_at, now()) WHERE id = $1")
                            .bind(event_id.0)
                            .execute(self.db.pool())
                            .await?;
                        return Ok(());
                    }

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
                                                "INSERT INTO records (user_id, schema_id, schema_scope, kind, domain, entity_type, title, data, occurred_at, source) \
                                                 SELECT $1, s.id, s.owner_scope, 'fact', s.namespace, s.name, $2, $3, $4, 'jev_system1_ingest' \
                                                 FROM data_schemas s \
                                                 WHERE s.id = $5 AND (s.user_id IS NULL OR s.user_id = $1)",
                                            )
                                            .bind(user_id.0)
                                            .bind(format!("{} logged", schema.qualified_name))
                                            .bind(&payload)
                                            .bind(occurred_at)
                                            .bind(schema.id)
                                            .execute(self.db.pool())
                                            .await;

                                    sqlx::query("UPDATE inbound_events SET processed_at = COALESCE(processed_at, now()) WHERE id = $1")
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

        sqlx::query(
            "UPDATE inbound_events SET processed_at = COALESCE(processed_at, now()) WHERE id = $1",
        )
        .bind(event_id.0)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }
}
