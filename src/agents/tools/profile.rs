use crate::{db::Db, identity::UserId};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{error::Error as StdError, fmt};

#[derive(Debug)]
pub enum ProfileToolError {
    Database(sqlx::Error),
    InvalidInput(String),
    NotConfigured,
}

impl fmt::Display for ProfileToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(err) => write!(f, "database error: {err}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::NotConfigured => write!(f, "database is not configured"),
        }
    }
}

impl StdError for ProfileToolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for ProfileToolError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GetUserInfoArgs {
    pub include_persona: Option<bool>,
}

#[derive(Clone)]
pub struct GetUserInfo {
    db: Option<Db>,
    user_id: UserId,
}

impl GetUserInfo {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for GetUserInfo {
    const NAME: &'static str = "get_user_info";
    type Args = GetUserInfoArgs;
    type Output = Value;
    type Error = ProfileToolError;

    fn description(&self) -> String {
        "Retrieve the current user profile, biographical facts, preferences, and personality traits."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "include_persona": {
                    "type": "boolean",
                    "description": "Whether to include communication style and persona settings"
                }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(ProfileToolError::NotConfigured)?;
        let row = sqlx::query(
            "SELECT facts, persona, version FROM user_profiles WHERE user_id = $1",
        )
        .bind(self.user_id.0)
        .fetch_optional(db.pool())
        .await?;

        match row {
            Some(r) => {
                let facts: Value = r.get("facts");
                let persona: Value = r.get("persona");
                let version: i64 = r.get("version");
                let mut res = json!({
                    "user_id": self.user_id.0.to_string(),
                    "facts": facts,
                    "version": version
                });
                if args.include_persona.unwrap_or(true) {
                    res["persona"] = persona;
                }
                Ok(res)
            }
            None => Ok(json!({
                "user_id": self.user_id.0.to_string(),
                "facts": {},
                "version": 0
            })),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateUserInfoArgs {
    pub name: Option<String>,
    pub facts: Option<serde_json::Map<String, Value>>,
    pub persona: Option<serde_json::Map<String, Value>>,
}

#[derive(Clone)]
pub struct UpdateUserInfo {
    db: Option<Db>,
    user_id: UserId,
}

impl UpdateUserInfo {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for UpdateUserInfo {
    const NAME: &'static str = "update_user_info";
    type Args = UpdateUserInfoArgs;
    type Output = Value;
    type Error = ProfileToolError;

    fn description(&self) -> String {
        "Update user profile facts (name, preferences, location, habits) or persona settings in the database."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "User's full or preferred name"
                },
                "facts": {
                    "type": "object",
                    "description": "Key-value map of biographical facts, preferences, or notes to merge"
                },
                "persona": {
                    "type": "object",
                    "description": "Persona trait settings: tone, verbosity, proactivity, technical_depth"
                }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(ProfileToolError::NotConfigured)?;
        let mut updates = args.facts.unwrap_or_default();
        if let Some(name) = args.name {
            if !name.trim().is_empty() {
                updates.insert("name".into(), json!(name.trim()));
            }
        }

        let facts_delta = Value::Object(updates);
        let persona_delta = args.persona.map(Value::Object);

        let mut tx = db.pool().begin().await?;

        if let Some(persona_val) = persona_delta {
            sqlx::query(
                "INSERT INTO user_profiles (user_id, facts, persona, version, updated_at) \
                 VALUES ($1, $2, $3, 1, now()) \
                 ON CONFLICT (user_id) DO UPDATE SET \
                 facts = user_profiles.facts || $2, \
                 persona = user_profiles.persona || $3, \
                 version = user_profiles.version + 1, \
                 updated_at = now()",
            )
            .bind(self.user_id.0)
            .bind(&facts_delta)
            .bind(&persona_val)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO user_profiles (user_id, facts, version, updated_at) \
                 VALUES ($1, $2, 1, now()) \
                 ON CONFLICT (user_id) DO UPDATE SET \
                 facts = user_profiles.facts || $2, \
                 version = user_profiles.version + 1, \
                 updated_at = now()",
            )
            .bind(self.user_id.0)
            .bind(&facts_delta)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        let updated_row = sqlx::query(
            "SELECT facts, persona FROM user_profiles WHERE user_id = $1",
        )
        .bind(self.user_id.0)
        .fetch_one(db.pool())
        .await?;

        let current_facts: Value = updated_row.get("facts");
        let current_persona: Value = updated_row.get("persona");

        Ok(json!({
            "status": "success",
            "user_id": self.user_id.0.to_string(),
            "facts": current_facts,
            "persona": current_persona
        }))
    }
}
