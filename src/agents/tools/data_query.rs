use crate::{
    application::chart_query::compute_chart_data_for_agent,
    db::Db,
    domain::charts::{Aggregation, GroupBy, QuerySpec},
    identity::{ResourceOwner, UserId},
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
    owner: Option<ResourceOwner>,
}

impl FindSchemas {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self {
            db,
            user_id,
            owner: None,
        }
    }
    pub fn with_owner(mut self, owner: ResourceOwner) -> Self {
        self.owner = Some(owner);
        self
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
        let owner = self
            .owner
            .filter(|o| o.user_id == self.user_id)
            .ok_or_else(|| DataQueryToolError::InvalidInput("Verified owner required".into()))?;
        let all = schemas_for_owner(db, owner).await?;
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
    owner: Option<ResourceOwner>,
}

impl QueryUserData {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self {
            db,
            user_id,
            owner: None,
        }
    }
    pub fn with_owner(mut self, owner: ResourceOwner) -> Self {
        self.owner = Some(owner);
        self
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

        let owner = self
            .owner
            .filter(|o| o.user_id == self.user_id)
            .ok_or_else(|| DataQueryToolError::InvalidInput("Verified owner required".into()))?;
        let points =
            compute_chart_data_for_agent(db.pool(), owner, "general", &args.schema_ids, &spec)
                .await?;

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

/// Private schema metadata is discoverable only through records this agent can read.
/// Global declarations remain public, without disclosing another host's private schemas.
pub(crate) async fn schemas_for_owner(
    db: &Db,
    owner: ResourceOwner,
) -> Result<Vec<crate::domain::schemas::DataSchema>, sqlx::Error> {
    let connections =
        crate::storage::span_authority::permitted_connections(db.pool(), owner, Some("general"))
            .await?;
    let connections: Vec<String> = connections.into_iter().map(|id| id.to_string()).collect();
    let allowed = crate::storage::pulse::ALLOWED_SPANS
        .replace(crate::storage::pulse::NATIVE_CONTEXT, "$2::uuid");
    let ids: Vec<Uuid> = sqlx::query_scalar(&format!("SELECT DISTINCT s.schema_id FROM spans s JOIN data_schemas ds ON ds.id=s.schema_id WHERE {allowed} AND (ds.user_context_id=$2 OR ds.user_id IS NULL) AND (NOT(s.data ? 'connection_id') OR s.data->>'connection_id'=ANY($3))"))
        .bind(owner.user_id.0).bind(owner.user_context_id.0).bind(&connections).fetch_all(db.pool()).await?;
    SchemaRepository::new(db.pool().clone())
        .list_visible_to_agent(owner, &ids)
        .await
}
