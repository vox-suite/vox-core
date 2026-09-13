use super::ActionId;
use crate::{
    bridge_client::{BridgeClientError, BridgeDispatch, OutboundCallRequest},
    db::Db,
    identity::ChannelIdentity,
};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct ActionHandler {
    db: Db,
    bridge: Arc<dyn BridgeDispatch>,
}

#[derive(Debug, thiserror::Error)]
pub enum ActionHandlerError {
    #[error("action storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("bridge dispatch error")]
    Bridge(#[from] BridgeClientError),
    #[error("action not found")]
    NotFound,
    #[error("user phone identity not found")]
    IdentityNotFound,
    #[error("invalid action payload")]
    InvalidPayload,
}

impl ActionHandler {
    pub fn new(db: Db, bridge: Arc<dyn BridgeDispatch>) -> Self {
        Self { db, bridge }
    }

    pub async fn handle(&self, action_id: ActionId) -> Result<(), ActionHandlerError> {
        let action = sqlx::query(
            "SELECT id, user_id, kind, payload, state, idempotency_key, provider_call_id \
             FROM actions WHERE id = $1",
        )
        .bind(action_id.0)
        .fetch_optional(self.db.pool())
        .await?;

        let row = action.ok_or(ActionHandlerError::NotFound)?;
        let state: String = row.get("state");
        if state == "in_progress" || state == "succeeded" {
            return Ok(());
        }

        let user_id: Uuid = row.get("user_id");
        let kind: String = row.get("kind");
        if kind != "outbound_call" {
            return Err(ActionHandlerError::InvalidPayload);
        }

        let payload: serde_json::Value = row.get("payload");
        let reason = payload
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("Vox Notification")
            .to_string();
        let opening_instruction = payload
            .get("opening_instruction")
            .and_then(|v| v.as_str())
            .unwrap_or("Hello from Vox")
            .to_string();

        let phone = sqlx::query_scalar::<_, String>(
            "SELECT external_id FROM user_identities WHERE user_id = $1 AND channel = 'phone' LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(self.db.pool())
        .await?;

        let external_phone = phone.ok_or(ActionHandlerError::IdentityNotFound)?;

        let conversation_row = sqlx::query(
            "INSERT INTO conversations (user_id, channel, external_id) \
             VALUES ($1, 'phone', $2) \
             ON CONFLICT (channel, external_id) DO UPDATE SET external_id = EXCLUDED.external_id \
             RETURNING id",
        )
        .bind(user_id)
        .bind(format!("outbound:{}", action_id.0))
        .fetch_one(self.db.pool())
        .await?;

        let conversation_id: Uuid = conversation_row.get("id");

        let response = self
            .bridge
            .initiate_outbound_call(OutboundCallRequest {
                action_id: action_id.0,
                identity: ChannelIdentity {
                    channel: "phone".into(),
                    external_id: external_phone,
                },
                reason,
                opening_instruction,
                conversation_id,
            })
            .await?;

        let mut tx = self.db.pool().begin().await?;

        let attempt_number = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(attempt_number), 0) + 1 FROM action_attempts WHERE action_id = $1",
        )
        .bind(action_id.0)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO action_attempts (action_id, attempt_number, state, provider_metadata) \
             VALUES ($1, $2, 'accepted', $3)",
        )
        .bind(action_id.0)
        .bind(attempt_number)
        .bind(serde_json::json!({ "provider_call_id": response.provider_call_id }))
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE actions SET state = 'in_progress', provider_call_id = $1, updated_at = now() \
             WHERE id = $2",
        )
        .bind(&response.provider_call_id)
        .bind(action_id.0)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }
}
