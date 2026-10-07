use super::EventId;
use crate::{
    agents::{
        event_agent::{EventAgent, EventContext},
        schema_extractor::{SchemaExtracting, SchemaExtractionPrompt},
        tools::records::validate_data_against_schema,
    },
    db::Db,
    identity::{IdentityService, UserId},
    jev::{
        event_triage::{EventTriageAction, EventTriager},
        schema_classifier::{SchemaClassificationResult, SchemaClassifier, SchemaDescriptor},
    },
};
use chrono::{DateTime, Utc};
use futures_util::{StreamExt, stream};
use serde_json::Value;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

const BATCH_CONCURRENCY: usize = 4;

#[derive(Clone)]
pub struct EventHandler {
    db: Db,
    triager: Option<Arc<EventTriager>>,
    schema_classifier: Option<Arc<SchemaClassifier>>,
    schema_extractor: Option<Arc<dyn SchemaExtracting>>,
    agent: Option<Arc<EventAgent>>,
}

#[derive(Debug, thiserror::Error)]
pub enum EventHandlerError {
    #[error("event storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("event planner unavailable")]
    Agent(#[from] crate::agents::AgentError),
    #[error("event processing hit a transient failure")]
    Transient(String),
}

impl EventHandler {
    pub fn new(db: Db) -> Self {
        Self::with_jev(db, None, None, None)
    }

    pub fn with_jev(
        db: Db,
        triager: Option<Arc<EventTriager>>,
        schema_classifier: Option<Arc<SchemaClassifier>>,
        schema_extractor: Option<Arc<dyn SchemaExtracting>>,
    ) -> Self {
        Self {
            db,
            triager,
            schema_classifier,
            schema_extractor,
            agent: None,
        }
    }

    pub fn with_agent(mut self, agent: Arc<EventAgent>) -> Self {
        self.agent = Some(agent);
        self
    }

    pub async fn handle(&self, event_id: EventId) -> Result<(), EventHandlerError> {
        let Some(row) = sqlx::query(
            "SELECT user_id, source_kind, event_type, occurred_at, payload FROM inbound_events WHERE id = $1",
        )
        .bind(event_id.0)
        .fetch_optional(self.db.pool())
        .await?
        else {
            return Ok(());
        };

        let user_id = UserId(row.get("user_id"));
        let owner = IdentityService::new(self.db.clone())
            .owner_for_user(user_id)
            .await
            .map_err(|error| match error {
                crate::identity::IdentityError::Database(error) => error,
                other => sqlx::Error::Protocol(other.to_string()),
            })?;
        let event_type: String = row.get("event_type");
        let source_kind: String = row.get("source_kind");
        let span_source = if source_kind == "sms" {
            "sms".to_string()
        } else {
            event_type.clone()
        };
        let occurred_at: DateTime<Utc> = row.get("occurred_at");
        let payload: Value = row.get("payload");

        let Some(triager) = &self.triager else {
            self.mark_failed(event_id, "jev_not_configured").await?;
            return Ok(());
        };

        let mut triage = match triager.triage(&event_type, &payload).await {
            Ok(triage) => triage,
            Err(err) => {
                return self
                    .fail_transient(event_id, format!("triage_failed: {err}"))
                    .await;
            }
        };

        // Authorization attempts and payments that already happened are structured
        // transactions, never agent to-dos, whatever the classifier guessed.
        if source_kind == "sms"
            && (payload.get("authorization_only").and_then(Value::as_bool) == Some(true)
                || payload
                    .get("body")
                    .and_then(Value::as_str)
                    .is_some_and(crate::sms_ingestion::looks_like_completed_payment))
        {
            triage.action = EventTriageAction::StoreRecord;
        }

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

        if triage.action == EventTriageAction::PlanAction
            && let Some(agent) = &self.agent
        {
            let context = EventContext {
                event_id: event_id.0,
                owner,
                source_kind: source_kind.clone(),
                event_type: event_type.clone(),
                occurred_at,
                payload: payload.clone(),
            };
            return match agent.run(context).await {
                Ok(outcome) => {
                    self.record_decision(user_id.0, &source_kind, &event_type, &outcome)
                        .await?;
                    self.delete_inbound_event(event_id).await
                }
                Err(error) => {
                    self.fail_transient(event_id, format!("event_agent_failed: {error}"))
                        .await
                }
            };
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
                return self
                    .fail_transient(event_id, format!("classify_failed: {err}"))
                    .await;
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
                    schema_id = %schema.id,
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
                    &span_source,
                )
                .await?;
            }
            SchemaClassificationResult::Novel { .. } => {
                tracing::info!(event_id = %event_id.0, "Jev System 1: novel schema, escalating to System 2");
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
                    &span_source,
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
        span_source: &str,
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
                return self
                    .fail_transient(event_id, format!("system_two_failed: {err}"))
                    .await;
            }
        };

        if let Err(err) = validate_data_against_schema(&result.json_schema, &result.data) {
            return self
                .fail_transient(event_id, format!("system_two_validation_failed: {err}"))
                .await;
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
                return self
                    .fail_transient(event_id, format!("schema_upsert_failed: {err}"))
                    .await;
            }
        };

        self.write_span(
            event_id,
            user_id,
            schema_id,
            &result.title,
            &result.data,
            occurred_at,
            span_source,
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
        // Authorization SMS is an attempt, never proof of a debit. Enforce
        // this independently of the model's extraction choices.
        let authorization_only = if source_kind == "sms" {
            sqlx::query_scalar::<_, bool>(
                "SELECT COALESCE((payload->>'authorization_only')::boolean, false) FROM inbound_events WHERE id = $1",
            )
            .bind(event_id.0)
            .fetch_optional(self.db.pool())
            .await?
            .unwrap_or(false)
        } else {
            false
        };
        let mut data = data.clone();
        if authorization_only && let Some(fields) = data.as_object_mut() {
            fields.insert("direction".into(), serde_json::json!("info"));
            fields.insert(
                "status".into(),
                serde_json::json!("authorization_requested"),
            );
            fields.insert("authorization_only".into(), serde_json::json!(true));
        }
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
        .bind(&data)
        .bind(event_id.0)
        .bind(schema_id)
        .fetch_one(self.db.pool())
        .await?;

        if source_kind == "sms" {
            crate::sms_ingestion::finance::dedupe_or_settle(
                self.db.pool(),
                user_id,
                span_id,
                &data,
            )
            .await?;
        }

        self.delete_inbound_event(event_id).await
    }

    async fn record_decision(
        &self,
        user_id: Uuid,
        source_kind: &str,
        event_type: &str,
        outcome: &str,
    ) -> Result<(), EventHandlerError> {
        sqlx::query(
            "INSERT INTO event_agent_decisions (user_id, source_kind, event_type, outcome) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(user_id)
        .bind(source_kind)
        .bind(event_type)
        .bind(outcome.chars().take(1500).collect::<String>())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    async fn delete_inbound_event(&self, event_id: EventId) -> Result<(), EventHandlerError> {
        sqlx::query("DELETE FROM inbound_events WHERE id = $1")
            .bind(event_id.0)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    async fn mark_failed(&self, event_id: EventId, error: &str) -> Result<(), EventHandlerError> {
        self.record_failure(event_id, error, false).await
    }

    async fn fail_transient(
        &self,
        event_id: EventId,
        error: String,
    ) -> Result<(), EventHandlerError> {
        self.record_failure(event_id, &error, true).await?;
        Err(EventHandlerError::Transient(error))
    }

    async fn record_failure(
        &self,
        event_id: EventId,
        error: &str,
        retryable: bool,
    ) -> Result<(), EventHandlerError> {
        sqlx::query(
            "UPDATE inbound_events SET processing_error = $2, retryable = $3, failed_at = now() WHERE id = $1",
        )
        .bind(event_id.0)
        .bind(error)
        .bind(retryable)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn handle_batch(&self, batch_id: Uuid) -> Result<(), EventHandlerError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM inbound_events \
             WHERE batch_id = $1 AND (processing_error IS NULL OR retryable) \
             ORDER BY occurred_at",
        )
        .bind(batch_id)
        .fetch_all(self.db.pool())
        .await?;
        let results: Vec<Result<(), EventHandlerError>> = stream::iter(ids)
            .map(|id| self.handle(EventId(id)))
            .buffer_unordered(BATCH_CONCURRENCY)
            .collect()
            .await;
        match results.into_iter().find_map(Result::err) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub async fn requeue_failed(&self) -> Result<u64, EventHandlerError> {
        let result = sqlx::query(
            "WITH due AS ( \
                 UPDATE inbound_events e SET requeue_count = requeue_count + 1, failed_at = now() \
                 WHERE e.id IN ( \
                     SELECT id FROM inbound_events \
                     WHERE processing_error IS NOT NULL AND retryable \
                       AND requeue_count < 3 AND failed_at < now() - interval '10 minutes' \
                     LIMIT 200) \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM jobs j \
                     WHERE j.kind IN ('process_event', 'process_event_batch') \
                       AND j.state IN ('pending', 'running') \
                       AND (j.source_event_id = e.id OR j.payload_reference_id = e.id \
                            OR j.payload_reference_id = e.batch_id)) \
                 RETURNING e.id, e.user_id) \
             INSERT INTO jobs (kind, user_id, source_event_id, payload_reference_id) \
             SELECT 'process_event', user_id, id, id FROM due",
        )
        .execute(self.db.pool())
        .await?;
        Ok(result.rows_affected())
    }
}
