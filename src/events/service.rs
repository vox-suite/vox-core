/**
* Event subscription and delivery coordination service.
*/
use super::{EventId, IngestEventRequest, IngestEventResponse};
use crate::{
    db::Db,
    identity::{IdentityError, IdentityService},
};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone)]
pub struct EventService {
    db: Db,
    identities: IdentityService,
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
        Self {
            identities: IdentityService::new(db.clone()),
            db,
        }
    }

    pub async fn ingest(
        &self,
        request: IngestEventRequest,
    ) -> Result<IngestEventResponse, EventError> {
        if request.idempotency_key.trim().is_empty()
            || request.event_type.trim().is_empty()
            || request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
        {
            return Err(EventError::Invalid);
        }
        let owner = self
            .identities
            .resolve_legacy_owner(&request.identity)
            .await?;
        let user_id = owner.user_id;
        let mut tx = self.db.pool().begin().await?;
        let source_id = format!(
            "{}:{}",
            request.identity.channel.trim(),
            request.identity.external_id.trim()
        );
        let payload_hash = hex::encode(Sha256::digest(
            serde_json::to_vec(&request.payload).unwrap_or_default(),
        ));
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO inbound_events (user_id, source_kind, source_id, external_event_id, payload_hash, event_type, occurred_at, payload) \
             VALUES ($1, 'channel', $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (source_kind, source_id, external_event_id) DO NOTHING \
             RETURNING id",
        )
        .bind(user_id.0)
        .bind(&source_id)
        .bind(request.idempotency_key.trim())
        .bind(payload_hash)
        .bind(request.event_type.trim())
        .bind(request.occurred_at)
        .bind(&request.payload)
        .fetch_optional(&mut *tx)
        .await?;
        let event_id = if let Some(id) = inserted {
            sqlx::query(
                "INSERT INTO jobs (kind, user_id, source_event_id, payload_reference_id) \
                 VALUES ('process_event', $1, $2, $2)",
            )
            .bind(user_id.0)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            id
        } else {
            let row = sqlx::query(
                "SELECT id, user_id FROM inbound_events \
                 WHERE source_kind = 'channel' AND source_id = $1 AND external_event_id = $2",
            )
            .bind(&source_id)
            .bind(request.idempotency_key.trim())
            .fetch_one(&mut *tx)
            .await?;
            let stored_user: Uuid = row.get("user_id");
            if stored_user != user_id.0 {
                return Err(EventError::Invalid);
            }
            row.get("id")
        };
        tx.commit().await?;
        Ok(IngestEventResponse {
            event_id: EventId(event_id),
        })
    }
}
