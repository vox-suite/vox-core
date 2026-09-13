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
        let mut tx = self.db.pool().begin().await?;
        let row = sqlx::query(
            "SELECT user_id, kind, payload, state FROM actions WHERE id = $1 FOR UPDATE",
        )
        .bind(action_id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ActionHandlerError::NotFound)?;
        let state: String = row.get("state");
        if state != "pending" {
            tx.commit().await?;
            return Ok(());
        }
        let kind: String = row.get("kind");
        if kind != "outbound_call" {
            return Err(ActionHandlerError::InvalidPayload);
        }
        let user_id: Uuid = row.get("user_id");
        let payload: serde_json::Value = row.get("payload");
        let reason = required_string(&payload, "reason")?;
        let opening_instruction = required_string(&payload, "opening_instruction")?;
        let external_phone = sqlx::query_scalar::<_, String>(
            "SELECT external_id FROM user_identities WHERE user_id = $1 AND channel = 'phone' LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ActionHandlerError::IdentityNotFound)?;
        let conversation_id: Uuid = sqlx::query_scalar(
            "INSERT INTO conversations (user_id, channel, external_id) VALUES ($1, 'phone', $2) \
             ON CONFLICT (channel, external_id) DO UPDATE SET external_id = EXCLUDED.external_id RETURNING id",
        )
        .bind(user_id)
        .bind(format!("outbound:{}", action_id.0))
        .fetch_one(&mut *tx)
        .await?;
        let attempt_number: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(attempt_number), 0) + 1 FROM action_attempts WHERE action_id = $1",
        )
        .bind(action_id.0)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO action_attempts (action_id, attempt_number, state) VALUES ($1, $2, 'started')",
        )
        .bind(action_id.0)
        .bind(attempt_number)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE actions SET state = 'in_progress', updated_at = now() WHERE id = $1")
            .bind(action_id.0)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        let result = self
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
            .await;
        match result {
            Ok(response) => {
                let mut tx = self.db.pool().begin().await?;
                sqlx::query(
                    "UPDATE action_attempts SET state = 'accepted', provider_metadata = $1 \
                     WHERE action_id = $2 AND attempt_number = $3",
                )
                .bind(serde_json::json!({"provider_call_id": response.provider_call_id}))
                .bind(action_id.0)
                .bind(attempt_number)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE actions SET provider_call_id = $1, updated_at = now() WHERE id = $2",
                )
                .bind(response.provider_call_id)
                .bind(action_id.0)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Ok(())
            }
            Err(error) => {
                let mut tx = self.db.pool().begin().await?;
                sqlx::query(
                    "UPDATE action_attempts SET state = 'failed', error_code = 'bridge_dispatch', completed_at = now() \
                     WHERE action_id = $1 AND attempt_number = $2",
                )
                .bind(action_id.0)
                .bind(attempt_number)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE actions SET state = 'failed', completed_at = now(), updated_at = now() WHERE id = $1",
                )
                .bind(action_id.0)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                Err(error.into())
            }
        }
    }
}

fn required_string(payload: &serde_json::Value, key: &str) -> Result<String, ActionHandlerError> {
    payload
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or(ActionHandlerError::InvalidPayload)
}
