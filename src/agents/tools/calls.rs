use crate::{db::Db, identity::UserId};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io;
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize)]
pub struct TriggerCallArgs {
    pub reason: String,
    pub opening_instruction: String,
}

#[derive(Clone)]
pub struct TriggerOutboundCall {
    db: Option<Db>,
    user_id: UserId,
}

impl TriggerOutboundCall {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for TriggerOutboundCall {
    const NAME: &'static str = "trigger_outbound_call";
    type Args = TriggerCallArgs;
    type Output = Value;
    type Error = io::Error;

    fn description(&self) -> String {
        "Trigger an outbound phone call to the user to speak with them live, deliver an urgent update, or inform them of completed tasks."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Internal reason for placing the call (e.g. Flight search complete, Urgent reminder)"
                },
                "opening_instruction": {
                    "type": "string",
                    "description": "The exact opening message or instruction the voice agent will speak to the user as soon as they answer the phone"
                }
            },
            "required": ["reason", "opening_instruction"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.user_id.0,
            reason = %args.reason,
            "Tool called"
        );
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| io::Error::other("Database unavailable"))?;

        let reason = args.reason.trim();
        let opening = args.opening_instruction.trim();

        if reason.is_empty() || opening.is_empty() {
            return Err(io::Error::other(
                "reason and opening_instruction are required",
            ));
        }

        let idempotency_key = format!("manual_call:{}:{}", self.user_id.0, Uuid::new_v4());

        let payload = json!({
            "reason": reason,
            "opening_instruction": opening
        });

        let mut tx = db
            .pool()
            .begin()
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;

        let action_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO actions (user_id, kind, payload, state, idempotency_key) \
             VALUES ($1, 'outbound_call', $2, 'pending', $3) \
             RETURNING id",
        )
        .bind(self.user_id.0)
        .bind(payload)
        .bind(idempotency_key)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| io::Error::other(e.to_string()))?;

        sqlx::query(
            "INSERT INTO jobs (kind, payload_reference_id) \
             VALUES ('dispatch_action', $1)",
        )
        .bind(action_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| io::Error::other(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;

        Ok(json!({
            "status": "call_queued",
            "action_id": action_id.to_string(),
            "reason": reason
        }))
    }
}
