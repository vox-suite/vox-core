/**
* Storage repository for dynamic record collection definitions.
*/
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::collections::{Collection, CollectionKind, CollectionStatus};

#[derive(Clone)]
pub struct CollectionRepository {
    pool: PgPool,
}

impl CollectionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        name: &str,
        description: &str,
        kind: CollectionKind,
        metadata: serde_json::Value,
    ) -> Result<Collection, sqlx::Error> {
        let kind_str = match kind {
            CollectionKind::Trip => "trip",
            CollectionKind::Course => "course",
            CollectionKind::Area => "area",
            CollectionKind::Project => "project",
        };

        let row = sqlx::query(
            r#"
            INSERT INTO collections (user_id, name, description, kind, metadata)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, user_id, name, description, kind, status, metadata, version, created_at, updated_at
            "#,
        )
        .bind(user_id)
        .bind(name)
        .bind(description)
        .bind(kind_str)
        .bind(metadata)
        .fetch_one(&self.pool)
        .await?;

        let kind_str: String = row.get("kind");
        let status_str: String = row.get("status");

        Ok(Collection {
            id: row.get("id"),
            user_id: row.get("user_id"),
            name: row.get("name"),
            description: row.get("description"),
            kind: match kind_str.as_str() {
                "trip" => CollectionKind::Trip,
                "course" => CollectionKind::Course,
                "area" => CollectionKind::Area,
                _ => CollectionKind::Project,
            },
            status: match status_str.as_str() {
                "paused" => CollectionStatus::Paused,
                "completed" => CollectionStatus::Completed,
                "archived" => CollectionStatus::Archived,
                _ => CollectionStatus::Active,
            },
            metadata: row.get("metadata"),
            version: row.get("version"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }

    pub async fn get_by_id(
        &self,
        user_id: Uuid,
        id: Uuid,
    ) -> Result<Option<Collection>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, name, description, kind, status, metadata, version, created_at, updated_at
            FROM collections
            WHERE id = $1 AND user_id = $2
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            let kind_str: String = r.get("kind");
            let status_str: String = r.get("status");
            Collection {
                id: r.get("id"),
                user_id: r.get("user_id"),
                name: r.get("name"),
                description: r.get("description"),
                kind: match kind_str.as_str() {
                    "trip" => CollectionKind::Trip,
                    "course" => CollectionKind::Course,
                    "area" => CollectionKind::Area,
                    _ => CollectionKind::Project,
                },
                status: match status_str.as_str() {
                    "paused" => CollectionStatus::Paused,
                    "completed" => CollectionStatus::Completed,
                    "archived" => CollectionStatus::Archived,
                    _ => CollectionStatus::Active,
                },
                metadata: r.get("metadata"),
                version: r.get("version"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            }
        }))
    }

    pub async fn list(&self, user_id: Uuid, limit: i64) -> Result<Vec<Collection>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, user_id, name, description, kind, status, metadata, version, created_at, updated_at
            FROM collections
            WHERE user_id = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
        )
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let kind_str: String = r.get("kind");
                let status_str: String = r.get("status");
                Collection {
                    id: r.get("id"),
                    user_id: r.get("user_id"),
                    name: r.get("name"),
                    description: r.get("description"),
                    kind: match kind_str.as_str() {
                        "trip" => CollectionKind::Trip,
                        "course" => CollectionKind::Course,
                        "area" => CollectionKind::Area,
                        _ => CollectionKind::Project,
                    },
                    status: match status_str.as_str() {
                        "paused" => CollectionStatus::Paused,
                        "completed" => CollectionStatus::Completed,
                        "archived" => CollectionStatus::Archived,
                        _ => CollectionStatus::Active,
                    },
                    metadata: r.get("metadata"),
                    version: r.get("version"),
                    created_at: r.get("created_at"),
                    updated_at: r.get("updated_at"),
                }
            })
            .collect())
    }

    pub async fn archive(&self, user_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE collections
            SET status = 'archived', updated_at = now()
            WHERE id = $1 AND user_id = $2
            "#,
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }
    pub async fn find_by_name(
        &self,
        user_id: Uuid,
        name: &str,
    ) -> Result<Option<Collection>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, name, description, kind, status, metadata, version, created_at, updated_at
            FROM collections
            WHERE user_id =  AND LOWER(name) = LOWER()
            LIMIT 1
            "#,
        )
        .bind(user_id)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            let kind_str: String = r.get("kind");
            let status_str: String = r.get("status");
            Collection {
                id: r.get("id"),
                user_id: r.get("user_id"),
                name: r.get("name"),
                description: r.get("description"),
                kind: match kind_str.as_str() {
                    "trip" => CollectionKind::Trip,
                    "course" => CollectionKind::Course,
                    "area" => CollectionKind::Area,
                    _ => CollectionKind::Project,
                },
                status: match status_str.as_str() {
                    "paused" => CollectionStatus::Paused,
                    "completed" => CollectionStatus::Completed,
                    "archived" => CollectionStatus::Archived,
                    _ => CollectionStatus::Active,
                },
                metadata: r.get("metadata"),
                version: r.get("version"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            }
        }))
    }
}
