/**
* Security audit logging and event tracking for sensitive actions.
*/
use crate::db::Db;
use chrono::{DateTime, Utc};
use hmac::Mac;
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
    pub task_id: Option<Uuid>,
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
    pub async fn deliver_next(&self, worker: &str, now: DateTime<Utc>) -> Result<bool, AuditError> {
        let mut tx = self.db.pool().begin().await?;
        let row=sqlx::query("WITH candidate AS (SELECT id FROM audit_sink_deliveries WHERE (state='pending' AND next_attempt_at <= $1) OR (state='leased' AND lease_expires_at <= $1) ORDER BY next_attempt_at FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE audit_sink_deliveries d SET state='leased',lease_owner=$2,lease_expires_at=$1 + interval '30 seconds',attempts=attempts+1,updated_at=$1 FROM candidate WHERE d.id=candidate.id RETURNING d.id,d.sink_id,d.audit_cursor,d.attempts").bind(now).bind(worker).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(false);
        };
        tx.commit().await?;
        let id: Uuid = row.get("id");
        let sink: Uuid = row.get("sink_id");
        let cursor: i64 = row.get("audit_cursor");
        let attempts: i32 = row.get("attempts");
        let data=sqlx::query("SELECT s.endpoint_origin,s.delivery_secret_key,e.schema_version,e.actor_type,e.event_type,e.aggregate_type,e.aggregate_id,e.occurred_at,e.details FROM audit_sink_definitions s JOIN audit_sink_deliveries d ON d.sink_id=s.id JOIN audit_events e ON e.cursor=d.audit_cursor WHERE s.id=$1 AND d.id=$2 AND s.state='enabled'").bind(sink).bind(id).fetch_optional(self.db.pool()).await?;
        let delivered = if let Some(data) = data {
            match self
                .secrets
                .get(&data.get::<String, _>("delivery_secret_key"))
                .await
            {
                Ok(secret) => {
                    let payload = serde_json::json!({"version":data.get::<i32,_>("schema_version"),"delivery_id":id,"cursor":cursor,"actor_type":data.get::<String,_>("actor_type"),"event_type":data.get::<String,_>("event_type"),"aggregate":{"type":data.get::<String,_>("aggregate_type"),"id":data.get::<Option<Uuid>,_>("aggregate_id")},"occurred_at":data.get::<DateTime<Utc>,_>("occurred_at"),"details":data.get::<Value,_>("details")});
                    let body = serde_json::to_vec(&payload).map_err(|_| AuditError::Invalid)?;
                    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
                        .map_err(|_| AuditError::Invalid)?;
                    mac.update(&body);
                    self.client
                        .post(data.get::<String, _>("endpoint_origin"))
                        .header(
                            "x-vox-audit-signature",
                            hex::encode(mac.finalize().into_bytes()),
                        )
                        .header("x-vox-audit-delivery-id", id.to_string())
                        .body(body)
                        .send()
                        .await
                        .map(|r| r.status().is_success())
                        .unwrap_or(false)
                }
                Err(_) => false,
            }
        } else {
            false
        };
        if delivered {
            sqlx::query("UPDATE audit_sink_deliveries SET state='delivered',delivered_at=$1,lease_owner=NULL,lease_expires_at=NULL,updated_at=$1 WHERE id=$2").bind(now).bind(id).execute(self.db.pool()).await?;
        } else {
            let failed = attempts >= 8;
            let mut tx = self.db.pool().begin().await?;
            sqlx::query("UPDATE audit_sink_deliveries SET state=CASE WHEN $1 THEN 'failed' ELSE 'pending' END,next_attempt_at=$2 + make_interval(secs => LEAST(3600,2 ^ LEAST(attempts,12))::int),lease_owner=NULL,lease_expires_at=NULL,updated_at=$2 WHERE id=$3").bind(failed).bind(now).bind(id).execute(&mut *tx).await?;
            if failed {
                sqlx::query("UPDATE audit_sink_definitions SET state='unhealthy',updated_at=$1 WHERE id=$2 AND state='enabled'").bind(now).bind(sink).execute(&mut *tx).await?;
            }
            tx.commit().await?;
        }
        Ok(true)
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
        let rows = sqlx::query("SELECT cursor,schema_version,user_context_id,actor_type,actor_reference,event_type,aggregate_type,aggregate_id,task_id,task_run_id,proposal_id,approval_id,execution_id,attempt_id,occurred_at,details FROM audit_events WHERE cursor>$1 AND ($2::uuid IS NULL OR user_context_id=$2) AND ($3::uuid IS NULL OR aggregate_id=$3) AND ($4::uuid IS NULL OR execution_id=$4) ORDER BY cursor LIMIT $5")
            .bind(after).bind(query.user_context_id).bind(query.aggregate_id).bind(query.execution_id).bind(limit).fetch_all(self.db.pool()).await?;
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
        sqlx::query("INSERT INTO audit_events (actor_type,actor_reference,event_type,aggregate_type,details) VALUES ('operator',$1,$2,'audit_admin',$3)")
            .bind(actor_reference).bind(event_type).bind(details).execute(self.db.pool()).await?;
        Ok(())
    }
}

fn event(row: sqlx::postgres::PgRow) -> Result<AuditEvent, AuditError> {
    Ok(AuditEvent {
        cursor: row.try_get("cursor")?,
        schema_version: row.try_get("schema_version")?,
        user_context_id: row.try_get("user_context_id")?,
        actor_type: row.try_get("actor_type")?,
        actor_reference: row.try_get("actor_reference")?,
        event_type: row.try_get("event_type")?,
        aggregate_type: row.try_get("aggregate_type")?,
        aggregate_id: row.try_get("aggregate_id")?,
        task_id: row.try_get("task_id")?,
        task_run_id: row.try_get("task_run_id")?,
        proposal_id: row.try_get("proposal_id")?,
        approval_id: row.try_get("approval_id")?,
        execution_id: row.try_get("execution_id")?,
        attempt_id: row.try_get("attempt_id")?,
        occurred_at: row.try_get("occurred_at")?,
        details: row.try_get("details")?,
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

#[cfg(test)]
#[path = "../../tests/unit/audit.rs"]
mod tests;
