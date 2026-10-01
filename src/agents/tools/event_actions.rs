use crate::{
    db::Db,
    domain::{
        ConcurrencyOutcome,
        spans::{NewSpan, SpanPatch, SpanStatus},
    },
    storage::spans::SpanRepository,
    user_notifications::{NotifyError, NotifyOutcome, UserNotifier, sanitize},
};
use chrono::{DateTime, Utc};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

const MUTABLE_SOURCES: [&str; 2] = ["sms", "event_agent"];

#[derive(Debug, thiserror::Error)]
pub enum EventActionError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Not allowed: {0}")]
    NotAllowed(String),
    #[error("Not found")]
    NotFound,
}

fn parse_time(field: &str, raw: Option<&str>) -> Result<Option<DateTime<Utc>>, EventActionError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(raw) => DateTime::parse_from_rfc3339(raw)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|_| {
                EventActionError::InvalidInput(format!(
                    "{field} must be ISO 8601 with a timezone offset, e.g. 2026-04-01T15:00:00+05:30"
                ))
            }),
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RecordTaskArgs {
    pub title: String,
    pub notes: Option<String>,
    pub category: Option<String>,
    pub start_at: Option<String>,
    pub due_at: Option<String>,
}

#[derive(Clone)]
pub struct RecordTask {
    db: Db,
    user_id: Uuid,
    event_id: Uuid,
    source_kind: String,
}

impl RecordTask {
    pub fn new(db: Db, user_id: Uuid, event_id: Uuid, source_kind: String) -> Self {
        Self {
            db,
            user_id,
            event_id,
            source_kind,
        }
    }
}

impl Tool for RecordTask {
    const NAME: &'static str = "record_task";
    type Args = RecordTaskArgs;
    type Output = Value;
    type Error = EventActionError;

    fn description(&self) -> String {
        "Add a planned to-do or reminder item to the user's timeline, for example a bill due date \
         or a delivery to expect. Always created as 'planned'. Check list_spans first to avoid duplicates."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Short title in your own words" },
                "notes": { "type": "string", "description": "Brief details in your own words" },
                "category": { "type": "string", "description": "e.g. bill, delivery, appointment, todo" },
                "start_at": { "type": "string", "description": "ISO 8601 with offset" },
                "due_at": { "type": "string", "description": "Deadline, ISO 8601 with offset" }
            },
            "required": ["title"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.user_id, event_id = %self.event_id, "Tool called");
        let title = sanitize(&args.title, 200);
        if title.is_empty() {
            return Err(EventActionError::InvalidInput(
                "title cannot be empty".into(),
            ));
        }
        let notes = sanitize(args.notes.as_deref().unwrap_or(""), 1000);
        let category = args
            .category
            .as_deref()
            .map(|value| sanitize(value, 40))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "task".to_string());
        let digest = hex::encode(Sha256::digest(title.to_lowercase()));
        let span = NewSpan {
            title,
            notes,
            category: Some(category),
            source: Some("event_agent".to_string()),
            source_ref: Some(format!("{}:{}", self.event_id, &digest[..12])),
            status: Some(SpanStatus::Planned),
            start_at: parse_time("start_at", args.start_at.as_deref())?,
            due_at: parse_time("due_at", args.due_at.as_deref())?,
            data: Some(json!({ "origin": "event_agent", "event_source": self.source_kind })),
            ..Default::default()
        };
        let id = SpanRepository::new(self.db.pool().clone())
            .record(self.user_id, span)
            .await?;
        Ok(json!({ "status": "recorded", "span_id": id }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AdjustSpanArgs {
    pub span_id: String,
    pub status: Option<String>,
    pub due_at: Option<String>,
    pub start_at: Option<String>,
    pub notes: Option<String>,
}

#[derive(Clone)]
pub struct AdjustSpan {
    db: Db,
    user_id: Uuid,
}

impl AdjustSpan {
    pub fn new(db: Db, user_id: Uuid) -> Self {
        Self { db, user_id }
    }
}

impl Tool for AdjustSpan {
    const NAME: &'static str = "adjust_span";
    type Args = AdjustSpanArgs;
    type Output = Value;
    type Error = EventActionError;

    fn description(&self) -> String {
        "Update a timeline item that came from the user's messages or an earlier event: mark it \
         done or cancelled, or change its dates. Items the user created themselves cannot be changed."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "span_id": { "type": "string", "description": "UUID of the span" },
                "status": { "type": "string", "enum": ["planned", "done", "cancelled"] },
                "due_at": { "type": "string", "description": "ISO 8601 with offset" },
                "start_at": { "type": "string", "description": "ISO 8601 with offset" },
                "notes": { "type": "string", "description": "Brief update in your own words" }
            },
            "required": ["span_id"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.user_id, "Tool called");
        let id = Uuid::parse_str(args.span_id.trim())
            .map_err(|_| EventActionError::InvalidInput("Invalid span_id".into()))?;
        let status = match args.status.as_deref().map(str::trim) {
            None | Some("") => None,
            Some("planned") => Some(SpanStatus::Planned),
            Some("done") => Some(SpanStatus::Done),
            Some("cancelled") => Some(SpanStatus::Cancelled),
            Some(other) => {
                return Err(EventActionError::InvalidInput(format!(
                    "status '{other}' is not allowed"
                )));
            }
        };
        let repo = SpanRepository::new(self.db.pool().clone());
        let span = repo
            .get_by_id(self.user_id, id)
            .await?
            .ok_or(EventActionError::NotFound)?;
        if !MUTABLE_SOURCES.contains(&span.source.as_str()) {
            return Err(EventActionError::NotAllowed(
                "only items created from messages or by the event agent can be changed".into(),
            ));
        }
        let patch = SpanPatch {
            status,
            due_at: parse_time("due_at", args.due_at.as_deref())?.map(Some),
            start_at: parse_time("start_at", args.start_at.as_deref())?.map(Some),
            notes: args.notes.as_deref().map(|notes| sanitize(notes, 1000)),
            ..Default::default()
        };
        match repo.update(self.user_id, id, patch).await? {
            ConcurrencyOutcome::Success(updated) => {
                Ok(json!({ "status": "updated", "span_id": updated.id }))
            }
            _ => Err(EventActionError::NotFound),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NotifyUserArgs {
    pub subject: String,
    pub message: String,
}

#[derive(Clone)]
pub struct NotifyUser {
    notifier: Option<Arc<UserNotifier>>,
    user_id: Uuid,
    event_id: Uuid,
}

impl NotifyUser {
    pub fn new(notifier: Option<Arc<UserNotifier>>, user_id: Uuid, event_id: Uuid) -> Self {
        Self {
            notifier,
            user_id,
            event_id,
        }
    }
}

impl Tool for NotifyUser {
    const NAME: &'static str = "notify_user";
    type Args = NotifyUserArgs;
    type Output = Value;
    type Error = EventActionError;

    fn description(&self) -> String {
        "Send the user one email about this event. Only for something time-sensitive or important. \
         It always goes to the user's own verified address; links and phone numbers are removed. \
         At most one notification per event."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subject": { "type": "string", "description": "Short subject" },
                "message": { "type": "string", "description": "Plain-text message in your own words" }
            },
            "required": ["subject", "message"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.user_id, event_id = %self.event_id, "Tool called");
        let notifier = self
            .notifier
            .as_ref()
            .ok_or_else(|| EventActionError::NotAllowed("email is not configured".into()))?;
        let key = format!("event:{}", self.event_id);
        match notifier
            .notify(self.user_id, &key, &args.subject, &args.message)
            .await
        {
            Ok(NotifyOutcome::Sent) => Ok(json!({ "status": "sent" })),
            Ok(NotifyOutcome::Duplicate) => Ok(json!({ "status": "already_notified" })),
            Err(NotifyError::Database(error)) => Err(EventActionError::Database(error)),
            Err(error) => Err(EventActionError::NotAllowed(error.to_string())),
        }
    }
}
