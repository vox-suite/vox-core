/**
* Storage repository for collections and their span membership.
*/
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::domain::collections::{Collection, CollectionKind, CollectionStatus};

const COLLECTION_COLUMNS: &str = "c.id, c.user_id, c.name, c.description, c.kind, c.status, \
    c.starts_at, c.ends_at, c.metadata, c.version, c.created_at, c.updated_at, \
    (SELECT count(*) FROM collection_spans cs WHERE cs.collection_id = c.id) AS span_count";

#[derive(Clone)]
pub struct CollectionRepository {
    pool: PgPool,
}

impl CollectionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        user_id: Uuid,
        name: &str,
        description: &str,
        kind: CollectionKind,
        starts_at: Option<DateTime<Utc>>,
        ends_at: Option<DateTime<Utc>>,
        metadata: serde_json::Value,
    ) -> Result<Collection, sqlx::Error> {
        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO collections (user_id, name, description, kind, starts_at, ends_at, metadata)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        )
        .bind(user_id)
        .bind(name.trim())
        .bind(description.trim())
        .bind(kind.as_str())
        .bind(starts_at)
        .bind(ends_at)
        .bind(metadata)
        .fetch_one(&self.pool)
        .await?;
        self.get_by_id(user_id, id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)
    }

    pub async fn get_by_id(
        &self,
        user_id: Uuid,
        id: Uuid,
    ) -> Result<Option<Collection>, sqlx::Error> {
        let row = sqlx::query(&format!(
            "SELECT {COLLECTION_COLUMNS} FROM collections c WHERE c.id = $1 AND c.user_id = $2"
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(map_collection))
    }

    pub async fn list(&self, user_id: Uuid, limit: i64) -> Result<Vec<Collection>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {COLLECTION_COLUMNS} FROM collections c
             WHERE c.user_id = $1 AND c.status <> 'archived'
             ORDER BY COALESCE(c.starts_at, c.created_at) DESC
             LIMIT $2"
        ))
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(map_collection).collect())
    }

    pub async fn find_by_name(
        &self,
        user_id: Uuid,
        name: &str,
    ) -> Result<Option<Collection>, sqlx::Error> {
        let row = sqlx::query(&format!(
            "SELECT {COLLECTION_COLUMNS} FROM collections c
             WHERE c.user_id = $1 AND LOWER(c.name) = LOWER($2) AND c.status <> 'archived'
             ORDER BY c.created_at LIMIT 1"
        ))
        .bind(user_id)
        .bind(name.trim())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(map_collection))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update(
        &self,
        user_id: Uuid,
        id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        status: Option<CollectionStatus>,
        starts_at: Option<Option<DateTime<Utc>>>,
        ends_at: Option<Option<DateTime<Utc>>>,
    ) -> Result<Option<Collection>, sqlx::Error> {
        let updated = sqlx::query(
            "UPDATE collections SET
                name = COALESCE($3, name),
                description = COALESCE($4, description),
                status = COALESCE($5, status),
                starts_at = CASE WHEN $6 THEN $7 ELSE starts_at END,
                ends_at = CASE WHEN $8 THEN $9 ELSE ends_at END,
                version = version + 1,
                updated_at = now()
             WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .bind(name.map(str::trim))
        .bind(description.map(str::trim))
        .bind(status.map(CollectionStatus::as_str))
        .bind(starts_at.is_some())
        .bind(starts_at.flatten())
        .bind(ends_at.is_some())
        .bind(ends_at.flatten())
        .execute(&self.pool)
        .await?
        .rows_affected();
        if updated == 0 {
            return Ok(None);
        }
        self.get_by_id(user_id, id).await
    }

    pub async fn archive(&self, user_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE collections SET status = 'archived', updated_at = now()
             WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn add_span(
        &self,
        user_id: Uuid,
        collection_id: Uuid,
        span_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO collection_spans (collection_id, span_id, user_id) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(collection_id)
        .bind(span_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_span(
        &self,
        user_id: Uuid,
        collection_id: Uuid,
        span_id: Uuid,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "DELETE FROM collection_spans WHERE collection_id = $1 AND span_id = $2 AND user_id = $3",
        )
        .bind(collection_id)
        .bind(span_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}

fn map_collection(row: PgRow) -> Collection {
    let kind: String = row.get("kind");
    let status: String = row.get("status");
    Collection {
        id: row.get("id"),
        user_id: row.get("user_id"),
        name: row.get("name"),
        description: row.get("description"),
        kind: CollectionKind::parse(&kind),
        status: CollectionStatus::parse(&status),
        starts_at: row.get("starts_at"),
        ends_at: row.get("ends_at"),
        metadata: row.get("metadata"),
        span_count: row.get("span_count"),
        version: row.get("version"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
