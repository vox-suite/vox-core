use crate::db::Db;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct AuditEvent {
    pub cursor: i64,
    pub schema_version: i32,
    pub user_context_id: Option<Uuid>,
    pub actor_type: String,
    pub actor_reference: Option<String>,
    pub event_type: String,
    pub aggregate_type: String,
    pub aggregate_id: Option<Uuid>,
    pub span_id: Option<Uuid>,
    pub task_run_id: Option<Uuid>,
    pub proposal_id: Option<Uuid>,
    pub approval_id: Option<Uuid>,
    pub execution_id: Option<Uuid>,
    pub attempt_id: Option<Uuid>,
    pub occurred_at: DateTime<Utc>,
    pub details: Value,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AuditQuery {
    pub after: Option<i64>,
    pub limit: Option<i64>,
    pub user_context_id: Option<Uuid>,
    pub aggregate_id: Option<Uuid>,
    pub execution_id: Option<Uuid>,
}

#[derive(Clone)]
pub struct AuditService {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("audit request invalid")]
    Invalid,
    #[error("audit storage unavailable")]
    Database(#[from] sqlx::Error),
}

#[async_trait::async_trait]
pub trait AuditSinkSecretStore: Send + Sync {
    async fn get(&self, key: &str) -> Result<String, AuditError>;
}

#[allow(dead_code)]
pub struct AuditDeliveryWorker {
    db: Db,
    secrets: Arc<dyn AuditSinkSecretStore>,
    client: reqwest::Client,
}

impl AuditDeliveryWorker {
    pub fn new(db: Db, secrets: Arc<dyn AuditSinkSecretStore>) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .expect("static client");
        Self {
            db,
            secrets,
            client,
        }
    }

    pub async fn deliver_next(
        &self,
        _worker: &str,
        _now: DateTime<Utc>,
    ) -> Result<bool, AuditError> {
        Ok(false)
    }
}

impl AuditService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn list(&self, query: AuditQuery) -> Result<Vec<AuditEvent>, AuditError> {
        let after = query.after.unwrap_or(0);
        let limit = query.limit.unwrap_or(50);
        if after < 0 || !(1..=100).contains(&limit) {
            return Err(AuditError::Invalid);
        }
        let rows = sqlx::query(
            "SELECT cursor_id, schema_version, user_context_id, actor, event_type, affected_ids, occurred_at, details \
             FROM audit_events \
             WHERE cursor_id > $1 \
             AND ($2::uuid IS NULL OR user_context_id = $2) \
             AND ($3::uuid IS NULL OR affected_ids::text LIKE '%' || $3::text || '%') \
             AND ($4::uuid IS NULL OR affected_ids::text LIKE '%' || $4::text || '%') \
             ORDER BY cursor_id LIMIT $5",
        )
        .bind(after)
        .bind(query.user_context_id)
        .bind(query.aggregate_id)
        .bind(query.execution_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter().map(event).collect()
    }

    pub async fn record_operator_access(
        &self,
        actor_reference: &str,
        event_type: &str,
        details: Value,
    ) -> Result<(), AuditError> {
        if trimmed(actor_reference, 255).is_none()
            || !event_type.starts_with("audit.")
            || !safe_details(&details)
        {
            return Err(AuditError::Invalid);
        }
        sqlx::query(
            "INSERT INTO audit_events (actor, event_type, affected_ids, details) \
             VALUES ($1, $2, '[]'::jsonb, $3)",
        )
        .bind(actor_reference)
        .bind(event_type)
        .bind(details)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }
}

fn event(row: sqlx::postgres::PgRow) -> Result<AuditEvent, AuditError> {
    let affected: Value = row.try_get("affected_ids")?;
    let details: Value = row.try_get("details")?;
    Ok(AuditEvent {
        cursor: row.try_get("cursor_id")?,
        schema_version: row.try_get("schema_version")?,
        user_context_id: row.try_get("user_context_id")?,
        actor_type: row.try_get("actor")?,
        actor_reference: details
            .get("actor_reference")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        event_type: row.try_get("event_type")?,
        aggregate_type: affected
            .get(0)
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned(),
        aggregate_id: affected
            .get(0)
            .and_then(|v| v.get("id"))
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok()),
        span_id: id_from_affected(&affected, "span"),
        task_run_id: id_from_affected(&affected, "task_run"),
        proposal_id: id_from_affected(&affected, "proposal"),
        approval_id: id_from_affected(&affected, "approval"),
        execution_id: id_from_affected(&affected, "execution"),
        attempt_id: id_from_affected(&affected, "attempt"),
        occurred_at: row.try_get("occurred_at")?,
        details,
    })
}

fn id_from_affected(affected: &Value, kind: &str) -> Option<Uuid> {
    affected.as_array()?.iter().find_map(|entry| {
        (entry.get("type")?.as_str()? == kind)
            .then(|| entry.get("id")?.as_str())
            .flatten()
            .and_then(|s| Uuid::parse_str(s).ok())
    })
}

fn trimmed(value: &str, max: usize) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= max).then_some(value)
}

fn safe_details(value: &Value) -> bool {
    value.as_object().is_some_and(|map| {
        map.len() <= 16
            && map.iter().all(|(key, value)| {
                let sensitive = [
                    "secret",
                    "credential",
                    "token",
                    "password",
                    "payload",
                    "reasoning",
                    "content",
                ];
                !sensitive
                    .iter()
                    .any(|needle| key.to_ascii_lowercase().contains(needle))
                    && (value.is_null()
                        || value.is_boolean()
                        || value.is_number()
                        || value.as_str().is_some_and(|x| x.len() <= 255))
            })
    })
}
