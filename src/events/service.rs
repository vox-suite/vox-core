/**
* Event subscription and delivery coordination service.
*/
use super::{EventId, IngestEventRequest, IngestEventResponse};
use crate::{
    db::Db,
    identity::{IdentityError, ResolvedUserContext},
};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone)]
pub struct EventService {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum EventError {
    #[error("invalid event request")]
    Invalid,
    #[error("event storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("event identity unavailable")]
    Identity(#[from] IdentityError),
}

impl EventService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn ingest(
        &self,
        context: ResolvedUserContext,
        request: IngestEventRequest,
    ) -> Result<IngestEventResponse, EventError> {
        if request.idempotency_key.trim().is_empty()
            || request.event_type.trim().is_empty()
            || request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
        {
            return Err(EventError::Invalid);
        }
        let owner = context.owner();
        let source_id = format!(
            "{}:{}",
            request.identity.channel.trim(),
            request.identity.external_id.trim()
        );
        let event_id = self
            .ingest_for_user(
                owner.user_id.0,
                "channel",
                &source_id,
                request.idempotency_key.trim(),
                request.event_type.trim(),
                request.occurred_at,
                &request.payload,
            )
            .await?;
        Ok(IngestEventResponse { event_id })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn ingest_for_user(
        &self,
        user_id: Uuid,
        source_kind: &str,
        source_id: &str,
        external_event_id: &str,
        event_type: &str,
        occurred_at: chrono::DateTime<chrono::Utc>,
        payload: &serde_json::Value,
    ) -> Result<EventId, EventError> {
        let mut tx = self.db.pool().begin().await?;
        let payload_hash = hex::encode(Sha256::digest(
            serde_json::to_vec(payload).unwrap_or_default(),
        ));
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO inbound_events (user_id, source_kind, source_id, external_event_id, payload_hash, event_type, occurred_at, payload) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (source_kind, source_id, external_event_id) DO NOTHING \
             RETURNING id",
        )
        .bind(user_id)
        .bind(source_kind)
        .bind(source_id)
        .bind(external_event_id)
        .bind(payload_hash)
        .bind(event_type)
        .bind(occurred_at)
        .bind(payload)
        .fetch_optional(&mut *tx)
        .await?;
        let event_id = if let Some(id) = inserted {
            sqlx::query(
                "INSERT INTO jobs (kind, user_id, source_event_id, payload_reference_id) \
                 VALUES ('process_event', $1, $2, $2)",
            )
            .bind(user_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            id
        } else {
            let row = sqlx::query(
                "SELECT id, user_id FROM inbound_events \
                 WHERE source_kind = $1 AND source_id = $2 AND external_event_id = $3",
            )
            .bind(source_kind)
            .bind(source_id)
            .bind(external_event_id)
            .fetch_one(&mut *tx)
            .await?;
            let stored_user: Uuid = row.get("user_id");
            if stored_user != user_id {
                return Err(EventError::Invalid);
            }
            row.get("id")
        };
        tx.commit().await?;
        Ok(EventId(event_id))
    }
}
