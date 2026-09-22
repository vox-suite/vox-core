/**
* Storage repository for user profiles and identity mappings.
*/
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::identity::{User, UserStatus};

#[derive(Clone)]
pub struct UserRepository {
    pool: PgPool,
}

impl UserRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get_by_id(&self, id: Uuid) -> Result<Option<User>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, status, display_name, preferences, profile_facts, persona,
                   profile_version, created_at, updated_at
            FROM users
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            let status_str: String = r.get("status");
            User {
                id: r.get("id"),
                status: match status_str.as_str() {
                    "provisional" => UserStatus::Provisional,
                    "disabled" => UserStatus::Disabled,
                    _ => UserStatus::Active,
                },
                display_name: r.get("display_name"),
                preferences: r.get("preferences"),
                profile_facts: r.get("profile_facts"),
                persona: r.get("persona"),
                profile_version: r.get("profile_version"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            }
        }))
    }
}
