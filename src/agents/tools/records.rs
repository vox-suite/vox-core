use crate::{db::Db, identity::UserId};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{error::Error as StdError, fmt};
use uuid::Uuid;

#[derive(Debug)]
pub enum RecordToolError {
    Database(sqlx::Error),
    InvalidInput(String),
    NotConfigured,
}

impl fmt::Display for RecordToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(err) => write!(f, "database error: {err}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::NotConfigured => write!(f, "database is not configured"),
        }
    }
}

impl StdError for RecordToolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for RecordToolError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateUserRecordArgs {
    pub domain: String,
    pub entity_type: String,
    pub title: String,
    pub data: Option<Value>,
}

#[derive(Clone)]
pub struct CreateUserRecord {
    db: Option<Db>,
    user_id: UserId,
}

impl CreateUserRecord {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for CreateUserRecord {
    const NAME: &'static str = "create_user_record";
    type Args = CreateUserRecordArgs;
    type Output = Value;
    type Error = RecordToolError;

    fn description(&self) -> String {
        "Log a personal record across domains: finance (expenses, incomes), health (sleep, workouts, vitals), work (progress, notes), knowledge (reading notes, bookmarks), wishlist (items to buy), or hobbies."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "domain": {
                    "type": "string",
                    "enum": ["finance", "health", "work", "knowledge", "wishlist", "hobbies", "general"],
                    "description": "Domain of the record"
                },
                "entity_type": {
                    "type": "string",
                    "description": "Specific entity type, e.g. transaction, sleep, note, bookmark, item"
                },
                "title": {
                    "type": "string",
                    "description": "Short headline or title of the record"
                },
                "data": {
                    "type": "object",
                    "description": "Structured JSON payload (e.g. amount, currency, category, rating, tags)"
                }
            },
            "required": ["domain", "entity_type", "title"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(RecordToolError::NotConfigured)?;
        let domain = args.domain.trim();
        let entity_type = args.entity_type.trim();
        let title = args.title.trim();

        if domain.is_empty() || entity_type.is_empty() || title.is_empty() {
            return Err(RecordToolError::InvalidInput("domain, entity_type, and title are required".into()));
        }

        let data_val = args.data.unwrap_or_else(|| json!({}));

        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO user_records (user_id, domain, entity_type, title, data, occurred_at, source) \
             VALUES ($1, $2, $3, $4, $5, now(), 'voice_agent') \
             RETURNING id",
        )
        .bind(self.user_id.0)
        .bind(domain)
        .bind(entity_type)
        .bind(title)
        .bind(&data_val)
        .fetch_one(db.pool())
        .await?;

        Ok(json!({
            "status": "created",
            "record_id": id.to_string(),
            "domain": domain,
            "title": title
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListUserRecordsArgs {
    pub domain: Option<String>,
    pub entity_type: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Clone)]
pub struct ListUserRecords {
    db: Option<Db>,
    user_id: UserId,
}

impl ListUserRecords {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for ListUserRecords {
    const NAME: &'static str = "list_user_records";
    type Args = ListUserRecordsArgs;
    type Output = Value;
    type Error = RecordToolError;

    fn description(&self) -> String {
        "Retrieve stored user records (e.g. finance transactions, health data, wishlist items, or knowledge notes)."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "domain": {
                    "type": "string",
                    "enum": ["finance", "health", "work", "knowledge", "wishlist", "hobbies", "all"],
                    "description": "Optional domain filter"
                },
                "entity_type": {
                    "type": "string",
                    "description": "Optional entity type filter"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of records to return; defaults to 15"
                }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(RecordToolError::NotConfigured)?;
        let limit = args.limit.unwrap_or(15).clamp(1, 50);

        let rows = if let Some(domain) = args.domain.filter(|d| d != "all") {
            if let Some(entity_type) = args.entity_type {
                sqlx::query(
                    "SELECT id, domain, entity_type, title, data, occurred_at \
                     FROM user_records \
                     WHERE user_id = $1 AND domain = $2 AND entity_type = $3 \
                     ORDER BY occurred_at DESC LIMIT $4",
                )
                .bind(self.user_id.0)
                .bind(domain)
                .bind(entity_type)
                .bind(limit)
                .fetch_all(db.pool())
                .await?
            } else {
                sqlx::query(
                    "SELECT id, domain, entity_type, title, data, occurred_at \
                     FROM user_records \
                     WHERE user_id = $1 AND domain = $2 \
                     ORDER BY occurred_at DESC LIMIT $3",
                )
                .bind(self.user_id.0)
                .bind(domain)
                .bind(limit)
                .fetch_all(db.pool())
                .await?
            }
        } else {
            sqlx::query(
                "SELECT id, domain, entity_type, title, data, occurred_at \
                 FROM user_records \
                 WHERE user_id = $1 \
                 ORDER BY occurred_at DESC LIMIT $2",
            )
            .bind(self.user_id.0)
            .bind(limit)
            .fetch_all(db.pool())
            .await?
        };

        let records: Vec<Value> = rows
            .into_iter()
            .map(|r| {
                let id: Uuid = r.get("id");
                let dom: String = r.get("domain");
                let ent: String = r.get("entity_type");
                let title: String = r.get("title");
                let data: Value = r.get("data");
                let occurred_at: chrono::DateTime<chrono::Utc> = r.get("occurred_at");
                json!({
                    "id": id.to_string(),
                    "domain": dom,
                    "entity_type": ent,
                    "title": title,
                    "data": data,
                    "occurred_at": occurred_at.to_rfc3339()
                })
            })
            .collect();

        Ok(json!({ "records": records }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ManageUserGoalArgs {
    pub domain: String,
    pub title: String,
    pub description: Option<String>,
    pub target_metric: Option<Value>,
    pub status: Option<String>,
}

#[derive(Clone)]
pub struct ManageUserGoal {
    db: Option<Db>,
    user_id: UserId,
}

impl ManageUserGoal {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for ManageUserGoal {
    const NAME: &'static str = "manage_user_goal";
    type Args = ManageUserGoalArgs;
    type Output = Value;
    type Error = RecordToolError;

    fn description(&self) -> String {
        "Create or update personal targets, budgets (e.g. monthly grocery spend under $400), or fitness goals."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "domain": {
                    "type": "string",
                    "description": "Domain of the goal: finance, health, work, habit"
                },
                "title": {
                    "type": "string",
                    "description": "Name of the goal, e.g. Limit dining spend"
                },
                "description": {
                    "type": "string",
                    "description": "Target description"
                },
                "target_metric": {
                    "type": "object",
                    "description": "Structured target metric, e.g. {\"limit_amount\": 400, \"currency\": \"USD\", \"period\": \"monthly\"}"
                },
                "status": {
                    "type": "string",
                    "enum": ["active", "paused", "completed", "abandoned"],
                    "description": "Status of the goal; defaults to active"
                }
            },
            "required": ["domain", "title"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(RecordToolError::NotConfigured)?;
        let domain = args.domain.trim();
        let title = args.title.trim();
        let desc = args.description.as_deref().unwrap_or("").trim();
        let status = args.status.as_deref().unwrap_or("active");
        let metric = args.target_metric.unwrap_or_else(|| json!({}));

        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO user_goals (user_id, domain, title, description, target_metric, status) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id",
        )
        .bind(self.user_id.0)
        .bind(domain)
        .bind(title)
        .bind(desc)
        .bind(&metric)
        .bind(status)
        .fetch_one(db.pool())
        .await?;

        Ok(json!({
            "status": "created",
            "goal_id": id.to_string(),
            "domain": domain,
            "title": title
        }))
    }
}
