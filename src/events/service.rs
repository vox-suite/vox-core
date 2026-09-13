use super::{EventId, IngestEventRequest, IngestEventResponse};
use crate::{db::Db, identity::IdentityService};
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
        let user_id = self.identities.resolve(&request.identity).await?;
        let mut tx = self.db.pool().begin().await?;
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO events (user_id, idempotency_key, event_type, occurred_at, payload) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (idempotency_key) DO NOTHING RETURNING id",
        ).bind(user_id.0).bind(request.idempotency_key.trim()).bind(request.event_type.trim())
            .bind(request.occurred_at).bind(request.payload).fetch_optional(&mut *tx).await?;
        let event_id = if let Some(id) = inserted {
            sqlx::query(
                "INSERT INTO jobs (kind, payload_reference_id) VALUES ('process_event', $1)",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            id
        } else {
            let row = sqlx::query("SELECT id, user_id FROM events WHERE idempotency_key = $1")
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
