/**
* Agent tools for spans: logging past activity, planning future items, and tracking to-dos.
*/
use crate::{
    db::Db,
    domain::spans::{ExecutionType, NewSpan, Span, SpanPatch, SpanQuery, SpanStatus},
    identity::ResourceOwner,
    realtime::UserEventHub,
    storage::{collections::CollectionRepository, spans::SpanRepository},
};
use chrono::{DateTime, Utc};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SpanToolError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Span not found: {0}")]
    NotFound(String),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Database not configured")]
    NotConfigured,
}

const STATUSES: [&str; 6] = [
    "planned",
    "active",
    "waiting_user",
    "done",
    "failed",
    "cancelled",
];

fn parse_time(field: &str, raw: Option<&str>) -> Result<Option<DateTime<Utc>>, SpanToolError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|_| {
                SpanToolError::InvalidInput(format!(
                    "{field} '{raw}' must be ISO 8601 with a timezone offset, e.g. 2026-04-01T15:00:00+05:30"
                ))
            }),
    }
}

fn parse_uuid(field: &str, raw: &str) -> Result<Uuid, SpanToolError> {
    Uuid::parse_str(raw.trim()).map_err(|_| SpanToolError::InvalidInput(format!("Invalid {field}")))
}

fn parse_status(raw: &str) -> Result<SpanStatus, SpanToolError> {
    SpanStatus::parse(raw.trim())
        .ok_or_else(|| SpanToolError::InvalidInput(format!("Unknown status '{raw}'")))
}

fn summary(span: &Span) -> Value {
    json!({
        "id": span.id,
        "title": span.title,
        "category": span.category,
        "status": span.status,
        "start_at": span.start_at,
        "end_at": span.end_at,
        "due_at": span.due_at,
        "parent_id": span.parent_id,
        "collection_ids": span.collection_ids,
    })
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateSpanArgs {
    pub title: String,
    pub notes: Option<String>,
    pub category: Option<String>,
    pub start_at: Option<String>,
    pub end_at: Option<String>,
    pub due_at: Option<String>,
    pub status: Option<String>,
    pub execution_type: Option<String>,
    pub collection_name: Option<String>,
    pub collection_id: Option<String>,
    pub amount: Option<f64>,
    pub currency: Option<String>,
}

#[derive(Clone)]
pub struct CreateSpan {
    db: Option<Db>,
    owner: ResourceOwner,
    user_events: UserEventHub,
}

impl CreateSpan {
    pub fn new(db: Option<Db>, owner: ResourceOwner, user_events: UserEventHub) -> Self {
        Self {
            db,
            owner,
            user_events,
        }
    }
}

impl Tool for CreateSpan {
    const NAME: &'static str = "create_span";
    type Args = CreateSpanArgs;
    type Output = Value;
    type Error = SpanToolError;

    fn description(&self) -> String {
        "Add an entry to the user's timeline. A span is anything that takes time: something that \
         happened (a bike ride, a meal, a ₹300 payment), something happening now, or something \
         planned (a meeting, a to-do, a task for Vox to execute). Past entries use status 'done'."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "Short title, e.g. 'Bike ride to Nandi Hills'" },
                "notes": { "type": "string", "description": "Details or instructions" },
                "category": { "type": "string", "description": "Free-form kind, e.g. ride, meal, expense, meeting, todo, call" },
                "start_at": { "type": "string", "description": "ISO 8601 with offset. Omit for an unscheduled to-do." },
                "end_at": { "type": "string", "description": "ISO 8601 with offset. Omit for an instant (a payment) or open-ended item." },
                "due_at": { "type": "string", "description": "Deadline, ISO 8601 with offset" },
                "status": { "type": "string", "enum": STATUSES, "description": "Defaults to planned" },
                "execution_type": {
                    "type": "string",
                    "enum": ["autonomous", "interactive", "manual_human"],
                    "description": "Only for actionable items: autonomous (Vox runs it in the background), interactive (needs the user's confirmation), manual_human (user does it). Omit for logged activity."
                },
                "collection_name": { "type": "string", "description": "Name of a collection (e.g. a trip) to add this to; created if missing" },
                "collection_id": { "type": "string", "description": "UUID of an existing collection to add this to" },
                "amount": { "type": "number", "description": "Money spent or received, for expenses" },
                "currency": { "type": "string", "description": "ISO 4217 code, e.g. INR" }
            },
            "required": ["title"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.owner.user_id.0, title = %args.title, "Tool called");
        let db = self.db.as_ref().ok_or(SpanToolError::NotConfigured)?;
        let title = args.title.trim();
        if title.is_empty() {
            return Err(SpanToolError::InvalidInput("title cannot be empty".into()));
        }
        let user_id = self.owner.user_id.0;
        let collections = CollectionRepository::new(db.pool().clone());

        let collection_id = if let Some(raw) = args.collection_id.as_deref() {
            let id = parse_uuid("collection_id", raw)?;
            collections
                .get_by_id(user_id, id)
                .await?
                .ok_or_else(|| SpanToolError::NotFound("collection".into()))?;
            Some(id)
        } else if let Some(name) = args
            .collection_name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            Some(match collections.find_by_name(user_id, name).await? {
                Some(c) => c.id,
                None => {
                    collections
                        .create(user_id, name, "", Default::default(), None, None, json!({}))
                        .await?
                        .id
                }
            })
        } else {
            None
        };

        let execution_type = match args.execution_type.as_deref() {
            None | Some("") => None,
            Some(raw) => Some(ExecutionType::parse(raw).ok_or_else(|| {
                SpanToolError::InvalidInput(format!("Unknown execution_type '{raw}'"))
            })?),
        };
        let status = args.status.as_deref().map(parse_status).transpose()?;
        let data = match args.amount {
            Some(amount) => json!({ "amount": amount, "currency": args.currency }),
            None => json!({}),
        };

        let span = SpanRepository::new(db.pool().clone())
            .create(
                user_id,
                NewSpan {
                    title: title.to_owned(),
                    notes: args.notes.unwrap_or_default(),
                    category: args.category,
                    source: Some("agent".into()),
                    status,
                    start_at: parse_time("start_at", args.start_at.as_deref())?,
                    end_at: parse_time("end_at", args.end_at.as_deref())?,
                    due_at: parse_time("due_at", args.due_at.as_deref())?,
                    execution_type,
                    data: Some(data),
                    collection_ids: collection_id.into_iter().collect(),
                    ..Default::default()
                },
            )
            .await?;

        self.user_events
            .notify(user_id, json!({"type": "span_created", "span_id": span.id}));
        Ok(json!({ "status": "created", "span": summary(&span) }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListSpansArgs {
    pub from: Option<String>,
    pub to: Option<String>,
    pub status: Option<String>,
    pub collection_id: Option<String>,
    pub unscheduled: Option<bool>,
    pub limit: Option<i64>,
}

#[derive(Clone)]
pub struct ListSpans {
    db: Option<Db>,
    owner: ResourceOwner,
}

impl ListSpans {
    pub fn new(db: Option<Db>, owner: ResourceOwner) -> Self {
        Self { db, owner }
    }
}

impl Tool for ListSpans {
    const NAME: &'static str = "list_spans";
    type Args = ListSpansArgs;
    type Output = Value;
    type Error = SpanToolError;

    fn description(&self) -> String {
        "List the user's timeline entries overlapping a time window (what they did, are doing, or \
         have planned), optionally filtered by status or collection. Use unscheduled=true for to-dos \
         without a time."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "Window start, ISO 8601 with offset" },
                "to": { "type": "string", "description": "Window end, ISO 8601 with offset" },
                "status": { "type": "string", "enum": STATUSES },
                "collection_id": { "type": "string", "description": "Only spans in this collection" },
                "unscheduled": { "type": "boolean", "description": "Only spans without a start time" },
                "limit": { "type": "integer", "description": "Defaults to 30" }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.owner.user_id.0, "Tool called");
        let db = self.db.as_ref().ok_or(SpanToolError::NotConfigured)?;
        let query = SpanQuery {
            from: parse_time("from", args.from.as_deref())?,
            to: parse_time("to", args.to.as_deref())?,
            collection_id: args
                .collection_id
                .as_deref()
                .map(|raw| parse_uuid("collection_id", raw))
                .transpose()?,
            status: args.status.as_deref().map(parse_status).transpose()?,
            unscheduled: args.unscheduled.unwrap_or(false),
            limit: Some(args.limit.unwrap_or(30).clamp(1, 100)),
        };
        let spans = SpanRepository::new(db.pool().clone())
            .list(self.owner.user_id.0, &query)
            .await?;
        Ok(json!({ "spans": spans.iter().map(summary).collect::<Vec<_>>() }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GetSpanArgs {
    pub span_id: String,
}

#[derive(Clone)]
pub struct GetSpan {
    db: Option<Db>,
    owner: ResourceOwner,
}

impl GetSpan {
    pub fn new(db: Option<Db>, owner: ResourceOwner) -> Self {
        Self { db, owner }
    }
}

impl Tool for GetSpan {
    const NAME: &'static str = "get_span";
    type Args = GetSpanArgs;
    type Output = Value;
    type Error = SpanToolError;

    fn description(&self) -> String {
        "Get full details of one timeline entry, including notes, data, and execution result."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "span_id": { "type": "string", "description": "UUID of the span" } },
            "required": ["span_id"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.owner.user_id.0, span_id = %args.span_id, "Tool called");
        let db = self.db.as_ref().ok_or(SpanToolError::NotConfigured)?;
        let id = parse_uuid("span_id", &args.span_id)?;
        let span = SpanRepository::new(db.pool().clone())
            .get_by_id(self.owner.user_id.0, id)
            .await?
            .ok_or_else(|| SpanToolError::NotFound(id.to_string()))?;
        Ok(serde_json::to_value(span).unwrap_or(Value::Null))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateSpanArgs {
    pub span_id: String,
    pub title: Option<String>,
    pub notes: Option<String>,
    pub status: Option<String>,
    pub start_at: Option<String>,
    pub end_at: Option<String>,
    pub due_at: Option<String>,
    pub execution_result: Option<Value>,
}

#[derive(Clone)]
pub struct UpdateSpan {
    db: Option<Db>,
    owner: ResourceOwner,
    user_events: UserEventHub,
}

impl UpdateSpan {
    pub fn new(db: Option<Db>, owner: ResourceOwner, user_events: UserEventHub) -> Self {
        Self {
            db,
            owner,
            user_events,
        }
    }
}

impl Tool for UpdateSpan {
    const NAME: &'static str = "update_span";
    type Args = UpdateSpanArgs;
    type Output = Value;
    type Error = SpanToolError;

    fn description(&self) -> String {
        "Update a timeline entry: mark it done or cancelled, reschedule it, rename it, or record an execution result."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "span_id": { "type": "string", "description": "UUID of the span" },
                "title": { "type": "string" },
                "notes": { "type": "string" },
                "status": { "type": "string", "enum": STATUSES },
                "start_at": { "type": "string", "description": "ISO 8601 with offset" },
                "end_at": { "type": "string", "description": "ISO 8601 with offset" },
                "due_at": { "type": "string", "description": "ISO 8601 with offset" },
                "execution_result": { "type": "object", "description": "Outcome details" }
            },
            "required": ["span_id"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.owner.user_id.0, span_id = %args.span_id, status = ?args.status, "Tool called");
        let db = self.db.as_ref().ok_or(SpanToolError::NotConfigured)?;
        let id = parse_uuid("span_id", &args.span_id)?;
        let patch = SpanPatch {
            title: args.title,
            notes: args.notes,
            status: args.status.as_deref().map(parse_status).transpose()?,
            start_at: parse_time("start_at", args.start_at.as_deref())?.map(Some),
            end_at: parse_time("end_at", args.end_at.as_deref())?.map(Some),
            due_at: parse_time("due_at", args.due_at.as_deref())?.map(Some),
            execution_result: args.execution_result,
            ..Default::default()
        };
        match SpanRepository::new(db.pool().clone())
            .update(self.owner.user_id.0, id, patch)
            .await?
        {
            crate::domain::ConcurrencyOutcome::Success(span) => {
                self.user_events.notify(
                    self.owner.user_id.0,
                    json!({"type": "span_updated", "span_id": span.id}),
                );
                Ok(json!({ "status": "updated", "span": summary(&span) }))
            }
            _ => Err(SpanToolError::NotFound(id.to_string())),
        }
    }
}
