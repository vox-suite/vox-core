/**
* Outbound call dispatch and phone channel communication.
*/
use crate::{
    bridge_client::{BridgeError, OutboundBridge, OutboundCallRequest},
    db::Db,
    identity::{ChannelIdentity, ResourceOwner, UserContextId, UserId},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutboundCallRecord {
    pub id: Uuid,
    pub user_context_id: Option<UserContextId>,
    pub user_id: UserId,
    pub span_id: Option<Uuid>,
    pub schedule_id: Option<Uuid>,
    pub phone_number: String,
    pub reason: String,
    pub opening_instruction: String,
    pub conversation_id: Uuid,
    pub provider_call_id: Option<String>,
    pub state: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("bridge error: {0}")]
    Bridge(#[from] BridgeError),
    #[error("user has no registered phone number")]
    NoPhoneNumber,
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[derive(Clone)]
pub struct OutboundCallService {
    db: Db,
    bridge: Option<Arc<dyn OutboundBridge>>,
}

impl OutboundCallService {
    pub fn new(db: Db, bridge: Option<Arc<dyn OutboundBridge>>) -> Self {
        Self { db, bridge }
    }

    pub fn with_bridge(mut self, bridge: Arc<dyn OutboundBridge>) -> Self {
        self.bridge = Some(bridge);
        self
    }

    pub fn bridge(&self) -> Option<&Arc<dyn OutboundBridge>> {
        self.bridge.as_ref()
    }

    pub async fn initiate_call_to_phone(
        &self,
        owner: ResourceOwner,
        phone_number: &str,
        reason: &str,
        opening_instruction: &str,
        schedule_id: Option<Uuid>,
        span_id: Option<Uuid>,
    ) -> Result<OutboundCallRecord, OutboundError> {
        let phone_clean = phone_number.trim();
        if phone_clean.is_empty() {
            return Err(OutboundError::InvalidInput(
                "Phone number cannot be empty".into(),
            ));
        }
        let reason_clean = reason.trim();
        if reason_clean.is_empty() {
            return Err(OutboundError::InvalidInput("Reason cannot be empty".into()));
        }
        let opening_clean = opening_instruction.trim();
        if opening_clean.is_empty() {
            return Err(OutboundError::InvalidInput(
                "Opening instruction cannot be empty".into(),
            ));
        }

        let call_id = Uuid::new_v4();
        let conversation_id = Uuid::new_v4();
        let created_at = Utc::now();

        let _ = sqlx::query(
            "INSERT INTO channel_identities (user_id, channel, normalized_external_id) \
             VALUES ($1, 'phone', $2) \
             ON CONFLICT (channel, provider_scope, normalized_external_id) \
             WHERE revoked_at IS NULL DO NOTHING",
        )
        .bind(owner.user_id.0)
        .bind(phone_clean)
        .execute(self.db.pool())
        .await;

        sqlx::query(
            "INSERT INTO conversations (id, user_context_id, user_id, channel, external_conversation_id) \
             VALUES ($1, $2, $3, 'phone', $4) \
             ON CONFLICT (user_context_id, channel, external_conversation_id) DO NOTHING",
        )
        .bind(conversation_id)
        .bind(owner.user_context_id.0)
        .bind(owner.user_id.0)
        .bind(conversation_id.to_string())
        .execute(self.db.pool())
        .await?;

        let checkpoint = serde_json::json!({
            "call_id": call_id,
            "phone_number": phone_clean,
            "reason": reason_clean,
            "opening_instruction": opening_clean,
            "conversation_id": conversation_id,
            "schedule_id": schedule_id,
            "span_id": span_id,
        });
        let dedupe_key = format!("outbound:{}:{}", owner.user_id.0, call_id);
        let job_id: Uuid = sqlx::query_scalar(
            "INSERT INTO jobs (user_id, kind, payload_reference_id, span_id, schedule_id, \
                 checkpoint, state, dedupe_key) \
             VALUES ($1, 'dispatch_action', $2, $3, $4, $5, 'running', $6) \
             RETURNING id",
        )
        .bind(owner.user_id.0)
        .bind(conversation_id)
        .bind(span_id)
        .bind(schedule_id)
        .bind(&checkpoint)
        .bind(&dedupe_key)
        .fetch_one(self.db.pool())
        .await?;

        let provider_call_id = if let Some(bridge) = &self.bridge {
            tracing::info!(
                call_id = %call_id,
                phone = %phone_clean,
                reason = %reason_clean,
                "Dispatching outbound call to vox-bridge"
            );
            let response = bridge
                .initiate_outbound_call(OutboundCallRequest {
                    action_id: call_id,
                    identity: ChannelIdentity {
                        channel: "phone".into(),
                        external_id: phone_clean.to_string(),
                    },
                    reason: reason_clean.to_string(),
                    opening_instruction: opening_clean.to_string(),
                    conversation_id,
                })
                .await?;

            let updated = serde_json::json!({
                "call_id": call_id,
                "phone_number": phone_clean,
                "reason": reason_clean,
                "opening_instruction": opening_clean,
                "conversation_id": conversation_id,
                "schedule_id": schedule_id,
                "span_id": span_id,
                "provider_call_id": response.provider_call_id,
                "state": "in_progress",
            });
            sqlx::query("UPDATE jobs SET checkpoint = $1 WHERE id = $2")
                .bind(&updated)
                .bind(job_id)
                .execute(self.db.pool())
                .await?;

            Some(response.provider_call_id)
        } else {
            tracing::warn!(
                call_id = %call_id,
                "Bridge client not configured; registered outbound call without external telephony dispatch"
            );
            None
        };

        let state = if provider_call_id.is_some() {
            "in_progress".to_string()
        } else {
            "initiated".to_string()
        };

        Ok(OutboundCallRecord {
            id: call_id,
            user_context_id: Some(owner.user_context_id),
            user_id: owner.user_id,
            span_id,
            schedule_id,
            phone_number: phone_clean.to_string(),
            reason: reason_clean.to_string(),
            opening_instruction: opening_clean.to_string(),
            conversation_id,
            provider_call_id,
            state,
            created_at,
        })
    }

    pub async fn initiate_call_for_user(
        &self,
        owner: ResourceOwner,
        reason: &str,
        opening_instruction: &str,
        schedule_id: Option<Uuid>,
        span_id: Option<Uuid>,
    ) -> Result<OutboundCallRecord, OutboundError> {
        let phone: Option<String> = sqlx::query_scalar(
            "SELECT normalized_external_id FROM channel_identities \
             WHERE user_id = $1 AND channel = 'phone' AND revoked_at IS NULL \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(owner.user_id.0)
        .fetch_optional(self.db.pool())
        .await?;

        let phone_number = phone.ok_or(OutboundError::NoPhoneNumber)?;
        self.initiate_call_to_phone(
            owner,
            &phone_number,
            reason,
            opening_instruction,
            schedule_id,
            span_id,
        )
        .await
    }
}
