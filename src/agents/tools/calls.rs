/**
* Agent tools for triggering and scheduling outbound telephone calls.
*/
use crate::{
    db::Db,
    identity::ResourceOwner,
    outbound::{OutboundCallService, OutboundError},
};
use chrono::{DateTime, Duration, Utc};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{error::Error as StdError, fmt, sync::Arc};
use uuid::Uuid;

#[derive(Debug)]
pub enum CallToolError {
    Database(sqlx::Error),
    Outbound(OutboundError),
    InvalidInput(String),
    NoPhoneNumber,
    NotConfigured,
}

impl fmt::Display for CallToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(err) => write!(f, "database error: {err}"),
            Self::Outbound(err) => write!(f, "outbound call error: {err}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::NoPhoneNumber => write!(
                f,
                "The user does not have a registered phone number. Please ask the user for their phone number or provide it in the call arguments."
            ),
            Self::NotConfigured => write!(f, "outbound service or database is not configured"),
        }
    }
}

impl StdError for CallToolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            Self::Outbound(err) => Some(err),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for CallToolError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

impl From<OutboundError> for CallToolError {
    fn from(err: OutboundError) -> Self {
        match err {
            OutboundError::NoPhoneNumber => Self::NoPhoneNumber,
            OutboundError::InvalidInput(msg) => Self::InvalidInput(msg),
            OutboundError::Database(e) => Self::Database(e),
            other => Self::Outbound(other),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ScheduleCallArgs {
    pub delay_seconds: Option<i64>,
    pub delay_minutes: Option<i64>,
    pub run_at: Option<String>,
    pub reason: String,
    pub opening_instruction: String,
    pub phone_number: Option<String>,
}

#[derive(Clone)]
pub struct ScheduleOutboundCall {
    db: Option<Db>,
    #[allow(dead_code)]
    outbound: Option<Arc<OutboundCallService>>,
    owner: ResourceOwner,
}

impl ScheduleOutboundCall {
    pub fn new(
        db: Option<Db>,
        outbound: Option<Arc<OutboundCallService>>,
        owner: ResourceOwner,
    ) -> Self {
        Self {
            db,
            outbound,
            owner,
        }
    }
}

impl Tool for ScheduleOutboundCall {
    const NAME: &'static str = "schedule_outbound_call";
    type Args = ScheduleCallArgs;
    type Output = Value;
    type Error = CallToolError;

    fn description(&self) -> String {
        "Schedule an outbound phone call to the user after a specific delay or at a specific time (e.g. 'call me after 5 min and remind me to clean my room'). This schedules a task and automatically places a phone call to the user when due."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "delay_seconds": {
                    "type": "integer",
                    "description": "Optional number of seconds from now to place the call (e.g. 300 for 5 minutes)"
                },
                "delay_minutes": {
                    "type": "integer",
                    "description": "Optional number of minutes from now to place the call (e.g. 5)"
                },
                "run_at": {
                    "type": "string",
                    "description": "Optional ISO 8601 timestamp for when to place the call (e.g. 2026-09-21T15:30:00Z)"
                },
                "reason": {
                    "type": "string",
                    "description": "Short description / title of the reminder or call reason (e.g. 'Clean room reminder')"
                },
                "opening_instruction": {
                    "type": "string",
                    "description": "What Vox should say as its opening greeting when the user answers the phone call (e.g. 'Remind the user to clean their room as requested')"
                },
                "phone_number": {
                    "type": "string",
                    "description": "Optional phone number override in E.164 format (e.g. +1234567890)"
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
        let db = self.db.as_ref().ok_or(CallToolError::NotConfigured)?;
        let reason = args.reason.trim();
        if reason.is_empty() {
            return Err(CallToolError::InvalidInput("Reason cannot be empty".into()));
        }
        let opening = args.opening_instruction.trim();
        if opening.is_empty() {
            return Err(CallToolError::InvalidInput(
                "Opening instruction cannot be empty".into(),
            ));
        }

        let now = Utc::now();
        let target_time = if let Some(secs) = args.delay_seconds {
            now + Duration::seconds(secs)
        } else if let Some(mins) = args.delay_minutes {
            now + Duration::minutes(mins)
        } else if let Some(ref dt_str) = args.run_at {
            chrono::DateTime::parse_from_rfc3339(dt_str)
                .map(|dt| dt.with_timezone(&Utc))
                .or_else(|_| {
                    chrono::NaiveDateTime::parse_from_str(dt_str, "%Y-%m-%dT%H:%M:%S").map(
                        |naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc),
                    )
                })
                .map_err(|_| {
                    CallToolError::InvalidInput(
                        "Invalid run_at timestamp. Expected ISO 8601 (e.g. 2026-09-21T15:00:00Z)"
                            .into(),
                    )
                })?
        } else {
            now + Duration::minutes(5)
        };

        let phone: Option<String> = if let Some(p) = args.phone_number {
            let p_clean = p.trim().to_string();
            if !p_clean.is_empty() {
                let _ = sqlx::query(
                    "INSERT INTO user_identities (user_id, channel, external_id) \
                     VALUES ($1, 'phone', $2) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(self.owner.user_id.0)
                .bind(&p_clean)
                .execute(db.pool())
                .await;
                Some(p_clean)
            } else {
                None
            }
        } else {
            sqlx::query_scalar(
                "SELECT external_id FROM user_identities \
                 WHERE user_id = $1 AND channel = 'phone' \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(self.owner.user_id.0)
            .fetch_optional(db.pool())
            .await?
        };

        let phone_number = phone.ok_or(CallToolError::NoPhoneNumber)?;

        let schedule_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO scheduled_tasks (user_context_id, user_id, instruction, schedule_kind, timezone, next_run_at, state) \
             VALUES ($1, $2, $3, 'once', 'UTC', $4, 'active') \
             RETURNING id",
        )
        .bind(self.owner.user_context_id.0)
        .bind(self.owner.user_id.0)
        .bind(opening)
        .bind(target_time)
        .fetch_one(db.pool())
        .await?;

        let task_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO tasks (user_context_id, user_id, title, raw_instruction, status, execution_type, due_at) \
             VALUES ($1, $2, $3, $4, 'pending', 'autonomous', $5) \
             RETURNING id",
        )
        .bind(self.owner.user_context_id.0)
        .bind(self.owner.user_id.0)
        .bind(reason)
        .bind(opening)
        .bind(target_time)
        .fetch_one(db.pool())
        .await?;

        tracing::info!(
            schedule_id = %schedule_id,
            task_id = %task_id,
            target_time = %target_time,
            phone_number = %phone_number,
            reason = %reason,
            "Scheduled outbound call created"
        );

        Ok(json!({
            "status": "scheduled",
            "schedule_id": schedule_id.to_string(),
            "task_id": task_id.to_string(),
            "phone_number": phone_number,
            "scheduled_time": target_time.to_rfc3339(),
            "reason": reason,
            "opening_instruction": opening,
            "message": format!("Outbound call scheduled for {} to call {} with reminder: {}", target_time.to_rfc3339(), phone_number, reason)
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TriggerCallArgs {
    pub reason: String,
    pub opening_instruction: String,
    pub phone_number: Option<String>,
}

#[derive(Clone)]
pub struct TriggerOutboundCall {
    db: Option<Db>,
    outbound: Option<Arc<OutboundCallService>>,
    owner: ResourceOwner,
}

impl TriggerOutboundCall {
    pub fn new(
        db: Option<Db>,
        outbound: Option<Arc<OutboundCallService>>,
        owner: ResourceOwner,
    ) -> Self {
        Self {
            db,
            outbound,
            owner,
        }
    }
}

impl Tool for TriggerOutboundCall {
    const NAME: &'static str = "trigger_outbound_call";
    type Args = TriggerCallArgs;
    type Output = Value;
    type Error = CallToolError;

    fn description(&self) -> String {
        "Immediately trigger an outbound phone call to the user to speak with them live right now."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Short description of why the call is being made"
                },
                "opening_instruction": {
                    "type": "string",
                    "description": "What Vox should say as its opening greeting when the user answers the phone call"
                },
                "phone_number": {
                    "type": "string",
                    "description": "Optional phone number override in E.164 format (e.g. +1234567890)"
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
        let outbound = self.outbound.as_ref().ok_or(CallToolError::NotConfigured)?;
        let reason = args.reason.trim();
        if reason.is_empty() {
            return Err(CallToolError::InvalidInput("Reason cannot be empty".into()));
        }
        let opening = args.opening_instruction.trim();
        if opening.is_empty() {
            return Err(CallToolError::InvalidInput(
                "Opening instruction cannot be empty".into(),
            ));
        }

        if let (Some(db), Some(phone)) = (&self.db, &args.phone_number) {
            let p_clean = phone.trim();
            if !p_clean.is_empty() {
                let _ = sqlx::query(
                    "INSERT INTO user_identities (user_id, channel, external_id) \
                     VALUES ($1, 'phone', $2) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(self.owner.user_id.0)
                .bind(p_clean)
                .execute(db.pool())
                .await;
            }
        }

        let record = if let Some(phone) = args
            .phone_number
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            outbound
                .initiate_call_to_phone(self.owner, phone, reason, opening, None, None)
                .await?
        } else {
            outbound
                .initiate_call_for_user(self.owner, reason, opening, None, None)
                .await?
        };

        tracing::info!(
            call_id = %record.id,
            conversation_id = %record.conversation_id,
            phone_number = %record.phone_number,
            reason = %record.reason,
            "Immediate outbound call initiated"
        );

        Ok(json!({
            "status": "call_initiated",
            "call_id": record.id.to_string(),
            "conversation_id": record.conversation_id.to_string(),
            "phone_number": record.phone_number,
            "provider_call_id": record.provider_call_id,
            "reason": record.reason,
            "opening_instruction": record.opening_instruction,
            "message": format!("Outbound call initiated to {} for {}", record.phone_number, record.reason)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{UserContextId, UserId};

    #[test]
    fn schedule_outbound_call_tool_metadata() {
        let owner = ResourceOwner {
            user_context_id: UserContextId(Uuid::new_v4()),
            user_id: UserId(Uuid::new_v4()),
        };
        let tool = ScheduleOutboundCall::new(None, None, owner);
        assert_eq!(ScheduleOutboundCall::NAME, "schedule_outbound_call");
        assert!(tool.description().contains("Schedule an outbound phone call"));
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["delay_minutes"].is_object());
        assert!(params["properties"]["reason"].is_object());
        assert!(params["properties"]["opening_instruction"].is_object());
    }

    #[test]
    fn trigger_outbound_call_tool_metadata() {
        let owner = ResourceOwner {
            user_context_id: UserContextId(Uuid::new_v4()),
            user_id: UserId(Uuid::new_v4()),
        };
        let tool = TriggerOutboundCall::new(None, None, owner);
        assert_eq!(TriggerOutboundCall::NAME, "trigger_outbound_call");
        assert!(tool.description().contains("outbound phone call"));
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["reason"].is_object());
        assert!(params["properties"]["opening_instruction"].is_object());
    }
}
