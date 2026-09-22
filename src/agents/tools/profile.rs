/**
* Agent tools for reading and writing user profile facts and preferences.
*/
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
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.user_id.0,
            include_persona = ?args.include_persona,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(ProfileToolError::NotConfigured)?;
        let row =
            sqlx::query("SELECT profile_facts AS facts, persona, profile_version AS version FROM users WHERE id = $1")
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
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.user_id.0,
            name = ?args.name,
            has_facts = args.facts.is_some(),
            has_persona = args.persona.is_some(),
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(ProfileToolError::NotConfigured)?;
        let mut updates = args.facts.unwrap_or_default();
        if let Some(name) = args.name
            && !name.trim().is_empty()
        {
            updates.insert("name".into(), json!(name.trim()));
        }

        let facts_delta = Value::Object(updates);
        let persona_delta = args.persona.map(Value::Object);

        let mut tx = db.pool().begin().await?;

        let display_name = facts_delta.get("name").and_then(|value| value.as_str());
        if let Some(persona_val) = persona_delta {
            sqlx::query(
                "UPDATE users SET \
                 profile_facts = profile_facts || $2, \
                 persona = persona || $3, \
                 profile_version = profile_version + 1, \
                 display_name = COALESCE($4, display_name), \
                 updated_at = now() \
                 WHERE id = $1",
            )
            .bind(self.user_id.0)
            .bind(&facts_delta)
            .bind(&persona_val)
            .bind(display_name)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(
                "UPDATE users SET \
                 profile_facts = profile_facts || $2, \
                 profile_version = profile_version + 1, \
                 display_name = COALESCE($3, display_name), \
                 updated_at = now() \
                 WHERE id = $1",
            )
            .bind(self.user_id.0)
            .bind(&facts_delta)
            .bind(display_name)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;

        let updated_row =
            sqlx::query("SELECT profile_facts AS facts, persona FROM users WHERE id = $1")
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
