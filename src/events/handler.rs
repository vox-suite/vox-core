use super::EventId;
use crate::{
    agents::{
        event_planner::EventPlanning,
        schema_extractor::{SchemaExtracting, SchemaExtractionPrompt},
        tools::records::validate_data_against_schema,
    },
    db::Db,
    identity::{IdentityService, UserId},
    jev::{
        event_triage::{EventTriageAction, EventTriager},
        schema_classifier::{SchemaClassificationResult, SchemaClassifier, SchemaDescriptor},
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
    #[allow(dead_code)]
    planner: Arc<dyn EventPlanning>,
    #[allow(dead_code)]
    memory: MemoryService,
    triager: Option<Arc<EventTriager>>,
    schema_classifier: Option<Arc<SchemaClassifier>>,
    schema_extractor: Option<Arc<dyn SchemaExtracting>>,
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
            schema_extractor: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_jev(
        db: Db,
        planner: Arc<dyn EventPlanning>,
        memory: MemoryService,
        triager: Option<Arc<EventTriager>>,
        schema_classifier: Option<Arc<SchemaClassifier>>,
        schema_extractor: Option<Arc<dyn SchemaExtracting>>,
    ) -> Self {
        Self {
            db,
            planner,
            memory,
            triager,
            schema_classifier,
            schema_extractor,
        }
    }

    pub async fn handle(&self, event_id: EventId) -> Result<(), EventHandlerError> {
        let Some(row) = sqlx::query(
            "SELECT user_id, event_type, occurred_at, payload FROM inbound_events WHERE id = $1",
        )
        .bind(event_id.0)
        .fetch_optional(self.db.pool())
        .await?
        else {
            return Ok(());
        };

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

        let Some(triager) = &self.triager else {
            self.mark_failed(event_id, "jev_not_configured").await?;
            return Ok(());
        };

        let triage = match triager.triage(&event_type, &payload).await {
            Ok(triage) => triage,
            Err(err) => {
                self.mark_failed(event_id, &format!("triage_failed: {err}"))
                    .await?;
                return Ok(());
            }
        };

        tracing::info!(
            event_id = %event_id.0,
            action = ?triage.action,
            confidence = triage.confidence,
            is_critical_alert = triage.is_critical_alert,
            "Jev System 1: event triage decision"
        );

        if triage.action == EventTriageAction::Ignore && triage.confidence >= 0.80 {
            tracing::info!(event_id = %event_id.0, "Jev System 1: ignored routine event");
            self.delete_inbound_event(event_id).await?;
            return Ok(());
        }

        if triage.action != EventTriageAction::StoreRecord {
            self.mark_failed(event_id, "plan_action_not_yet_handled")
                .await?;
            return Ok(());
        }

        let Some(classifier) = &self.schema_classifier else {
            self.mark_failed(event_id, "schema_classifier_not_configured")
                .await?;
            return Ok(());
        };

        let classification = match classifier.classify(user_id.0, &payload).await {
            Ok(result) => result,
            Err(err) => {
                self.mark_failed(event_id, &format!("classify_failed: {err}"))
                    .await?;
                return Ok(());
            }
        };

        match classification {
            SchemaClassificationResult::Existing { schema, confidence } => {
                if let Err(err) = validate_data_against_schema(&schema.json_schema, &payload) {
                    self.mark_failed(event_id, &format!("validation_failed: {err}"))
                        .await?;
                    return Ok(());
                }
                tracing::info!(
                    event_id = %event_id.0,
                    schema = %schema.qualified_name,
                    confidence,
                    "Jev System 1: matched existing schema"
                );
                let title = format!("{} logged", schema.qualified_name);
                self.write_span(
                    event_id,
                    user_id.0,
                    schema.id,
                    &title,
                    &payload,
                    occurred_at,
                    &event_type,
                )
                .await?;
            }
            SchemaClassificationResult::Novel { reason, .. } => {
                tracing::info!(event_id = %event_id.0, reason, "Jev System 1: novel schema, escalating to System 2");
                let Some(extractor) = &self.schema_extractor else {
                    self.mark_failed(event_id, "schema_extractor_not_configured")
                        .await?;
                    return Ok(());
                };
                let near_miss = classifier
                    .load_user_schemas(user_id.0)
                    .await
                    .unwrap_or_default();
                self.run_system_two(
                    event_id,
                    user_id.0,
                    &event_type,
                    &payload,
                    occurred_at,
                    extractor.as_ref(),
                    near_miss,
                )
                .await?;
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_system_two(
        &self,
        event_id: EventId,
        user_id: Uuid,
        event_type: &str,
        payload: &Value,
        occurred_at: DateTime<Utc>,
        extractor: &dyn SchemaExtracting,
        near_miss_schemas: Vec<SchemaDescriptor>,
    ) -> Result<(), EventHandlerError> {
        let result = match extractor
            .extract(SchemaExtractionPrompt {
                event_type: event_type.to_string(),
                payload: payload.clone(),
                occurred_at,
                near_miss_schemas,
            })
            .await
        {
            Ok(result) => result,
            Err(err) => {
                self.mark_failed(event_id, &format!("system_two_failed: {err}"))
                    .await?;
                return Ok(());
            }
        };

        if let Err(err) = validate_data_against_schema(&result.json_schema, &result.data) {
            self.mark_failed(event_id, &format!("system_two_validation_failed: {err}"))
                .await?;
            return Ok(());
        }

        let schema_id = match self
            .upsert_schema(
                user_id,
                &result.namespace,
                &result.name,
                &result.description,
                &result.json_schema,
                result.color_token,
                result.icon_token,
            )
            .await
        {
            Ok(id) => id,
            Err(err) => {
                self.mark_failed(event_id, &format!("schema_upsert_failed: {err}"))
                    .await?;
                return Ok(());
            }
        };

        self.write_span(
            event_id,
            user_id,
            schema_id,
            &result.title,
            &result.data,
            occurred_at,
            event_type,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn upsert_schema(
        &self,
        user_id: Uuid,
        namespace: &str,
        name: &str,
        description: &str,
        json_schema: &Value,
        color_token: i32,
        icon_token: i32,
    ) -> Result<Uuid, sqlx::Error> {
        if let Some(existing) = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM data_schemas WHERE user_id = $1 AND namespace = $2 AND name = $3 \
             ORDER BY version DESC LIMIT 1",
        )
        .bind(user_id)
        .bind(namespace)
        .bind(name)
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(existing);
        }

        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO data_schemas (user_id, namespace, name, version, description, json_schema, color_token, icon_token) \
             VALUES ($1, $2, $3, 1, $4, $5, $6, $7) \
             ON CONFLICT (user_id, namespace, name, version) DO UPDATE SET description = data_schemas.description \
             RETURNING id",
        )
        .bind(user_id)
        .bind(namespace)
        .bind(name)
        .bind(description)
        .bind(json_schema)
        .bind(color_token)
        .bind(icon_token)
        .fetch_one(self.db.pool())
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_span(
        &self,
        event_id: EventId,
        user_id: Uuid,
        schema_id: Uuid,
        title: &str,
        data: &Value,
        occurred_at: DateTime<Utc>,
        source_kind: &str,
    ) -> Result<(), EventHandlerError> {
        let span_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO spans (user_id, title, category, source, status, start_at, data, schema_id, source_event_id) \
             SELECT $1, $2, s.name, $3, 'done', $4, $5, s.id, $6 \
             FROM data_schemas s WHERE s.id = $7 \
             RETURNING spans.id",
        )
        .bind(user_id)
        .bind(title)
        .bind(source_kind)
        .bind(occurred_at)
        .bind(data)
        .bind(event_id.0)
        .bind(schema_id)
        .fetch_one(self.db.pool())
        .await?;

        if source_kind == "sms" {
            crate::sms_ingestion::finance::dedupe_or_settle(self.db.pool(), user_id, span_id, data)
                .await?;
        }

        self.delete_inbound_event(event_id).await
    }

    async fn delete_inbound_event(&self, event_id: EventId) -> Result<(), EventHandlerError> {
        sqlx::query("DELETE FROM inbound_events WHERE id = $1")
            .bind(event_id.0)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    async fn mark_failed(&self, event_id: EventId, error: &str) -> Result<(), EventHandlerError> {
        sqlx::query("UPDATE inbound_events SET processing_error = $2 WHERE id = $1")
            .bind(event_id.0)
            .bind(error)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }
}
