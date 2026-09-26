/**
* Agent tools for collections: groupings of spans such as a trip or an event.
*/
use crate::{
    db::Db,
    domain::{
        collections::{Collection, CollectionKind, CollectionStatus},
        spans::SpanQuery,
    },
    identity::UserId,
    storage::{collections::CollectionRepository, spans::SpanRepository},
};
use chrono::{DateTime, Utc};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum CollectionToolError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Collection not found")]
    NotFound,
    #[error("Database not configured")]
    NotConfigured,
}

fn parse_time(raw: Option<&str>) -> Result<Option<DateTime<Utc>>, CollectionToolError> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(raw) => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|_| {
                CollectionToolError::InvalidInput(format!("'{raw}' must be ISO 8601 with offset"))
            }),
    }
}

fn summary(c: &Collection) -> Value {
    json!({
        "id": c.id,
        "name": c.name,
        "description": c.description,
        "kind": c.kind,
        "status": c.status,
        "starts_at": c.starts_at,
        "ends_at": c.ends_at,
        "span_count": c.span_count,
    })
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateCollectionArgs {
    pub name: String,
    pub description: Option<String>,
    pub kind: Option<CollectionKind>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
}

#[derive(Clone)]
pub struct CreateCollection {
    db: Option<Db>,
    user_id: UserId,
}

impl CreateCollection {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for CreateCollection {
    const NAME: &'static str = "create_collection";
    type Args = CreateCollectionArgs;
    type Output = Value;
    type Error = CollectionToolError;

    fn description(&self) -> String {
        "Create a collection that groups timeline entries, e.g. a trip, an event, a course, or a life area."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "e.g. 'Goa trip'" },
                "description": { "type": "string" },
                "kind": { "type": "string", "enum": ["trip", "event", "course", "area", "custom"] },
                "starts_at": { "type": "string", "description": "ISO 8601 with offset" },
                "ends_at": { "type": "string", "description": "ISO 8601 with offset" }
            },
            "required": ["name"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.user_id.0, name = %args.name, "Tool called");
        let db = self.db.as_ref().ok_or(CollectionToolError::NotConfigured)?;
        if args.name.trim().is_empty() {
            return Err(CollectionToolError::InvalidInput(
                "name cannot be empty".into(),
            ));
        }
        let collection = CollectionRepository::new(db.pool().clone())
            .create(
                self.user_id.0,
                &args.name,
                args.description.as_deref().unwrap_or(""),
                args.kind.unwrap_or_default(),
                parse_time(args.starts_at.as_deref())?,
                parse_time(args.ends_at.as_deref())?,
                json!({}),
            )
            .await?;
        Ok(json!({ "status": "created", "collection": summary(&collection) }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListCollectionsArgs {}

#[derive(Clone)]
pub struct ListCollections {
    db: Option<Db>,
    user_id: UserId,
}

impl ListCollections {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for ListCollections {
    const NAME: &'static str = "list_collections";
    type Args = ListCollectionsArgs;
    type Output = Value;
    type Error = CollectionToolError;

    fn description(&self) -> String {
        "List the user's collections (trips, events, areas) that are not archived.".to_owned()
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.user_id.0, "Tool called");
        let db = self.db.as_ref().ok_or(CollectionToolError::NotConfigured)?;
        let collections = CollectionRepository::new(db.pool().clone())
            .list(self.user_id.0, 50)
            .await?;
        Ok(json!({ "collections": collections.iter().map(summary).collect::<Vec<_>>() }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GetCollectionArgs {
    pub collection_id: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone)]
pub struct GetCollection {
    db: Option<Db>,
    user_id: UserId,
}

impl GetCollection {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for GetCollection {
    const NAME: &'static str = "get_collection";
    type Args = GetCollectionArgs;
    type Output = Value;
    type Error = CollectionToolError;

    fn description(&self) -> String {
        "Get a collection by id or name with its timeline entries and total spend.".to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "collection_id": { "type": "string" },
                "name": { "type": "string" }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.user_id.0, "Tool called");
        let db = self.db.as_ref().ok_or(CollectionToolError::NotConfigured)?;
        let repo = CollectionRepository::new(db.pool().clone());
        let collection = if let Some(raw) = args.collection_id.as_deref() {
            let id = Uuid::parse_str(raw.trim())
                .map_err(|_| CollectionToolError::InvalidInput("Invalid collection_id".into()))?;
            repo.get_by_id(self.user_id.0, id).await?
        } else if let Some(name) = args.name.as_deref() {
            repo.find_by_name(self.user_id.0, name).await?
        } else {
            return Err(CollectionToolError::InvalidInput(
                "Provide collection_id or name".into(),
            ));
        }
        .ok_or(CollectionToolError::NotFound)?;

        let spans = SpanRepository::new(db.pool().clone())
            .list(
                self.user_id.0,
                &SpanQuery {
                    collection_id: Some(collection.id),
                    limit: Some(200),
                    ..Default::default()
                },
            )
            .await?;
        let spent: f64 = spans
            .iter()
            .filter_map(|s| s.data.get("amount").and_then(Value::as_f64))
            .sum();

        Ok(json!({
            "collection": summary(&collection),
            "total_amount": spent,
            "spans": spans.iter().map(|s| json!({
                "id": s.id,
                "title": s.title,
                "category": s.category,
                "status": s.status,
                "start_at": s.start_at,
                "end_at": s.end_at,
                "amount": s.data.get("amount"),
            })).collect::<Vec<_>>(),
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateCollectionArgs {
    pub collection_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub status: Option<CollectionStatus>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub add_span_ids: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct UpdateCollection {
    db: Option<Db>,
    user_id: UserId,
}

impl UpdateCollection {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for UpdateCollection {
    const NAME: &'static str = "update_collection";
    type Args = UpdateCollectionArgs;
    type Output = Value;
    type Error = CollectionToolError;

    fn description(&self) -> String {
        "Rename a collection, change its dates or status (active, paused, completed, archived), or add existing timeline entries to it."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "collection_id": { "type": "string" },
                "name": { "type": "string" },
                "description": { "type": "string" },
                "status": { "type": "string", "enum": ["active", "paused", "completed", "archived"] },
                "starts_at": { "type": "string", "description": "ISO 8601 with offset" },
                "ends_at": { "type": "string", "description": "ISO 8601 with offset" },
                "add_span_ids": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["collection_id"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(tool = Self::NAME, user_id = %self.user_id.0, collection_id = %args.collection_id, "Tool called");
        let db = self.db.as_ref().ok_or(CollectionToolError::NotConfigured)?;
        let id = Uuid::parse_str(args.collection_id.trim())
            .map_err(|_| CollectionToolError::InvalidInput("Invalid collection_id".into()))?;
        let repo = CollectionRepository::new(db.pool().clone());
        for raw in args.add_span_ids.unwrap_or_default() {
            let span_id = Uuid::parse_str(raw.trim())
                .map_err(|_| CollectionToolError::InvalidInput(format!("Invalid span id {raw}")))?;
            repo.add_span(self.user_id.0, id, span_id).await?;
        }
        let collection = repo
            .update(
                self.user_id.0,
                id,
                args.name.as_deref(),
                args.description.as_deref(),
                args.status,
                parse_time(args.starts_at.as_deref())?.map(Some),
                parse_time(args.ends_at.as_deref())?.map(Some),
            )
            .await?
            .ok_or(CollectionToolError::NotFound)?;
        Ok(json!({ "status": "updated", "collection": summary(&collection) }))
    }
}
