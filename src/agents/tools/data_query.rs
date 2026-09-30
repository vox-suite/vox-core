use crate::{
    application::chart_query::compute_chart_data,
    db::Db,
    domain::charts::{Aggregation, GroupBy, QuerySpec},
    identity::UserId,
    storage::schemas::SchemaRepository,
};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum DataQueryToolError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Database not configured")]
    NotConfigured,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FindSchemasArgs {
    pub query: String,
}

#[derive(Clone)]
pub struct FindSchemas {
    db: Option<Db>,
    user_id: UserId,
}

impl FindSchemas {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for FindSchemas {
    const NAME: &'static str = "find_schemas";
    type Args = FindSchemasArgs;
    type Output = Value;
    type Error = DataQueryToolError;

    fn description(&self) -> String {
        "Find relevant data schemas and categories in the user's timeline (e.g. expenses, trips, goals, workouts).".to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search term or concept, e.g. 'expenses', 'travel', 'budget'" }
            },
            "required": ["query"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(DataQueryToolError::NotConfigured)?;
        let repo = SchemaRepository::new(db.pool().clone());

        let all = repo.list_for_user(Some(self.user_id.0)).await?;
        let q = args.query.to_lowercase();

        let filtered: Vec<_> = all
            .into_iter()
            .filter(|s| {
                s.name.to_lowercase().contains(&q)
                    || s.namespace.to_lowercase().contains(&q)
                    || s.description.to_lowercase().contains(&q)
            })
            .map(|s| {
                let properties = s
                    .json_schema
                    .get("properties")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                json!({
                    "id": s.id,
                    "namespace": s.namespace,
                    "name": s.name,
                    "description": s.description,
                    "fields": properties
                })
            })
            .collect();

        Ok(json!({ "schemas": filtered }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct QueryUserDataArgs {
    pub schema_ids: Vec<Uuid>,
    pub metric_field: String,
    pub aggregation: String,
    pub group_by: String,
}

#[derive(Clone)]
pub struct QueryUserData {
    db: Option<Db>,
    user_id: UserId,
}

impl QueryUserData {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for QueryUserData {
    const NAME: &'static str = "query_user_data";
    type Args = QueryUserDataArgs;
    type Output = Value;
    type Error = DataQueryToolError;

    fn description(&self) -> String {
        "Query aggregated numerical metrics over the user's timeline spans for specific schemas."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "schema_ids": { "type": "array", "items": { "type": "string" }, "description": "List of schema UUIDs to query" },
                "metric_field": { "type": "string", "description": "Numeric field in spans.data, e.g. 'amount', 'duration'" },
                "aggregation": { "type": "string", "enum": ["sum", "avg", "count", "min", "max"] },
                "group_by": { "type": "string", "description": "Group by bucket ('day', 'week', 'month') or category field" }
            },
            "required": ["schema_ids", "metric_field", "aggregation", "group_by"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(DataQueryToolError::NotConfigured)?;

        let agg = Aggregation::parse(&args.aggregation).ok_or_else(|| {
            DataQueryToolError::InvalidInput(format!("Invalid aggregation: {}", args.aggregation))
        })?;

        let group_by = GroupBy::parse(&args.group_by);

        let spec = QuerySpec {
            metric_field: args.metric_field.clone(),
            aggregation: agg,
            group_by,
        };

        let points = compute_chart_data(db.pool(), self.user_id.0, &args.schema_ids, &spec).await?;

        let sum: f64 = points.iter().map(|p| p.value).sum();
        let count = points.len();
        let avg = if count > 0 { sum / count as f64 } else { 0.0 };

        Ok(json!({
            "metric_field": args.metric_field,
            "aggregation": args.aggregation,
            "points": points,
            "summary": {
                "total": sum,
                "count": count,
                "avg": avg
            }
        }))
    }
}
