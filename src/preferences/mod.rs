use crate::{db::Db, identity::ResolvedUserContext};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

pub const SENSITIVE_PREFERENCE_KEYS: &[&str] = &[
    "home_address",
    "work_address",
    "allergies",
    "dietary_medical",
    "passport_number",
    "identity_document",
    "payment_method_preference",
];

pub const AUTHORITY_DISCLAIMER: &str = "User preference is advisory context only. It confers no execution authority. Provider currency, timezone, and inventory facts remain strictly authoritative.";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserPreference {
    pub id: Uuid,
    pub user_context_id: Uuid,
    pub category: String,
    pub preference_key: String,
    pub value: Value,
    pub is_sensitive: bool,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub authority_disclaimer: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SetPreferenceRequest {
    pub category: String,
    pub preference_key: String,
    pub value: Value,
    pub is_sensitive: Option<bool>,
    pub confirmed: Option<bool>,
}

#[derive(Clone)]
pub struct PreferenceService {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum PreferenceError {
    #[error("preference request invalid: {0}")]
    Invalid(String),
    #[error("sensitive preference requires explicit confirmation before save or replacement")]
    ConfirmationRequired,
    #[error("preference storage unavailable: {0}")]
    Database(#[from] sqlx::Error),
}

impl PreferenceService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn set_preference(
        &self,
        context: &ResolvedUserContext,
        req: SetPreferenceRequest,
        now: DateTime<Utc>,
    ) -> Result<UserPreference, PreferenceError> {
        let category = req.category.trim();
        let key = req.preference_key.trim();
        if category.is_empty() || category.len() > 128 {
            return Err(PreferenceError::Invalid("invalid category".into()));
        }
        if key.is_empty() || key.len() > 128 {
            return Err(PreferenceError::Invalid("invalid preference key".into()));
        }
        if req.value.is_null() {
            return Err(PreferenceError::Invalid(
                "preference value cannot be null".into(),
            ));
        }

        let is_sensitive =
            req.is_sensitive.unwrap_or(false) || SENSITIVE_PREFERENCE_KEYS.contains(&key);

        let mut tx = self.db.pool().begin().await?;

        // Check if existing preference is already present and whether it was sensitive
        let existing = sqlx::query(
            "SELECT is_sensitive FROM user_preferences WHERE user_context_id = $1 AND preference_key = $2 FOR UPDATE",
        )
        .bind(context.id.0)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;

        let existing_was_sensitive = existing
            .as_ref()
            .map(|r| r.get::<bool, _>("is_sensitive"))
            .unwrap_or(false);

        // Sensitive preferences require confirmation before initial save OR replacement
        if (is_sensitive || existing_was_sensitive) && req.confirmed != Some(true) {
            return Err(PreferenceError::ConfirmationRequired);
        }

        let confirmed_at = if is_sensitive { Some(now) } else { None };

        let row = sqlx::query(
            r#"
            INSERT INTO user_preferences (
                user_context_id, category, preference_key, value, is_sensitive, confirmed_at, created_at, updated_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
            ON CONFLICT (user_context_id, preference_key) DO UPDATE SET
                category = EXCLUDED.category,
                value = EXCLUDED.value,
                is_sensitive = EXCLUDED.is_sensitive,
                confirmed_at = EXCLUDED.confirmed_at,
                updated_at = EXCLUDED.updated_at
            RETURNING id, user_context_id, category, preference_key, value, is_sensitive, confirmed_at, created_at, updated_at
            "#,
        )
        .bind(context.id.0)
        .bind(category)
        .bind(key)
        .bind(&req.value)
        .bind(is_sensitive)
        .bind(confirmed_at)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(UserPreference {
            id: row.get("id"),
            user_context_id: row.get("user_context_id"),
            category: row.get("category"),
            preference_key: row.get("preference_key"),
            value: row.get("value"),
            is_sensitive: row.get("is_sensitive"),
            confirmed_at: row.get("confirmed_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            authority_disclaimer: AUTHORITY_DISCLAIMER.into(),
        })
    }

    pub async fn list_preferences(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<UserPreference>, PreferenceError> {
        let rows = sqlx::query(
            r#"
            SELECT id, user_context_id, category, preference_key, value, is_sensitive, confirmed_at, created_at, updated_at
            FROM user_preferences
            WHERE user_context_id = $1
            ORDER BY category ASC, preference_key ASC
            "#,
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| UserPreference {
                id: r.get("id"),
                user_context_id: r.get("user_context_id"),
                category: r.get("category"),
                preference_key: r.get("preference_key"),
                value: r.get("value"),
                is_sensitive: r.get("is_sensitive"),
                confirmed_at: r.get("confirmed_at"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
                authority_disclaimer: AUTHORITY_DISCLAIMER.into(),
            })
            .collect())
    }

    pub async fn effective_for_agent(
        &self,
        context: &ResolvedUserContext,
        _agent_key: &str,
        categories: &[String],
    ) -> Result<Vec<UserPreference>, PreferenceError> {
        if categories.is_empty() {
            return Ok(Vec::new());
        }

        let rows = sqlx::query(
            r#"
            SELECT id, user_context_id, category, preference_key, value, is_sensitive, confirmed_at, created_at, updated_at
            FROM user_preferences
            WHERE user_context_id = $1 AND category = ANY($2) AND is_sensitive = false
            ORDER BY category ASC, preference_key ASC
            "#,
        )
        .bind(context.id.0)
        .bind(categories)
        .fetch_all(self.db.pool())
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| UserPreference {
                id: r.get("id"),
                user_context_id: r.get("user_context_id"),
                category: r.get("category"),
                preference_key: r.get("preference_key"),
                value: r.get("value"),
                is_sensitive: r.get("is_sensitive"),
                confirmed_at: r.get("confirmed_at"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
                authority_disclaimer: AUTHORITY_DISCLAIMER.into(),
            })
            .collect())
    }

    pub async fn delete_preference(
        &self,
        context: &ResolvedUserContext,
        key: &str,
    ) -> Result<bool, PreferenceError> {
        let rows = sqlx::query(
            "DELETE FROM user_preferences WHERE user_context_id = $1 AND preference_key = $2",
        )
        .bind(context.id.0)
        .bind(key.trim())
        .execute(self.db.pool())
        .await?
        .rows_affected();

        Ok(rows > 0)
    }
}
