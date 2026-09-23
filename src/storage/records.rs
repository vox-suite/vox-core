/**
* Storage repository for dynamic JSONB records and document lookups.
*/
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::records::{Record, RecordKind};

#[derive(Clone)]
pub struct RecordRepository {
    pool: PgPool,
}

impl RecordRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        user_id: Uuid,
        schema_id: Uuid,
        schema_scope: &str,
        kind: RecordKind,
        domain: &str,
        entity_type: &str,
        title: &str,
        data: serde_json::Value,
        source: &str,
        occurred_at: Option<DateTime<Utc>>,
        collection_id: Option<Uuid>,
    ) -> Result<Record, sqlx::Error> {
        let kind_str = match kind {
            RecordKind::Goal => "goal",
            RecordKind::Insight => "insight",
            RecordKind::Fact => "fact",
        };
        let occurred = occurred_at.unwrap_or_else(Utc::now);

        let row = sqlx::query(
            r#"
            INSERT INTO records (
                user_id, schema_id, schema_scope, kind, domain, entity_type,
                title, data, source, occurred_at, collection_id
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING id, user_id, schema_id, schema_scope, kind, domain, entity_type,
                      title, data, occurred_at, source, source_event_id, source_record_ids,
                      collection_id, valid_until, version, created_at, updated_at
            "#,
        )
        .bind(user_id)
        .bind(schema_id)
        .bind(schema_scope)
        .bind(kind_str)
        .bind(domain)
        .bind(entity_type)
        .bind(title)
        .bind(data)
        .bind(source)
        .bind(occurred)
        .bind(collection_id)
        .fetch_one(&self.pool)
        .await?;

        let k_str: String = row.get("kind");

        Ok(Record {
            id: row.get("id"),
            user_id: row.get("user_id"),
            schema_id: row.get("schema_id"),
            schema_scope: row.get("schema_scope"),
            kind: match k_str.as_str() {
                "goal" => RecordKind::Goal,
                "insight" => RecordKind::Insight,
                _ => RecordKind::Fact,
            },
            domain: row.get("domain"),
            entity_type: row.get("entity_type"),
            title: row.get("title"),
            data: row.get("data"),
            occurred_at: row.get("occurred_at"),
            source: row.get("source"),
            source_event_id: row.get("source_event_id"),
            source_record_ids: row.get("source_record_ids"),
            collection_id: row.get("collection_id"),
            valid_until: row.get("valid_until"),
            version: row.get("version"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }

    pub async fn update(
        &self,
        user_id: Uuid,
        id: Uuid,
        expected_version: Option<i32>,
        title: Option<&str>,
        data: Option<serde_json::Value>,
    ) -> Result<crate::domain::ConcurrencyOutcome<Record>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        let current_version = sqlx::query_scalar::<_, i32>(
            "SELECT version FROM records WHERE id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;

        let current_version = match current_version {
            Some(v) => v,
            None => return Ok(crate::domain::ConcurrencyOutcome::NotFound),
        };

        if let Some(expected) = expected_version
            && expected != current_version
        {
            return Ok(crate::domain::ConcurrencyOutcome::Conflict);
        }

        let row = sqlx::query(
            r#"
            UPDATE records SET
                title = COALESCE($3, title),
                data = COALESCE($4, data),
                version = version + 1,
                updated_at = now()
            WHERE id = $1 AND user_id = $2
            RETURNING id, user_id, schema_id, schema_scope, kind, domain, entity_type,
                      title, data, occurred_at, source, source_event_id, source_record_ids,
                      collection_id, valid_until, version, created_at, updated_at
            "#,
        )
        .bind(id)
        .bind(user_id)
        .bind(title)
        .bind(data)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        let k_str: String = row.get("kind");
        Ok(crate::domain::ConcurrencyOutcome::Success(Record {
            id: row.get("id"),
            user_id: row.get("user_id"),
            schema_id: row.get("schema_id"),
            schema_scope: row.get("schema_scope"),
            kind: match k_str.as_str() {
                "goal" => RecordKind::Goal,
                "insight" => RecordKind::Insight,
                _ => RecordKind::Fact,
            },
            domain: row.get("domain"),
            entity_type: row.get("entity_type"),
            title: row.get("title"),
            data: row.get("data"),
            occurred_at: row.get("occurred_at"),
            source: row.get("source"),
            source_event_id: row.get("source_event_id"),
            source_record_ids: row.get("source_record_ids"),
            collection_id: row.get("collection_id"),
            valid_until: row.get("valid_until"),
            version: row.get("version"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }))
    }

    pub async fn get_by_id(&self, user_id: Uuid, id: Uuid) -> Result<Option<Record>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, schema_id, schema_scope, kind, domain, entity_type,
                   title, data, occurred_at, source, source_event_id, source_record_ids,
                   collection_id, valid_until, version, created_at, updated_at
            FROM records
            WHERE id = $1 AND user_id = $2
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            let k_str: String = r.get("kind");
            Record {
                id: r.get("id"),
                user_id: r.get("user_id"),
                schema_id: r.get("schema_id"),
                schema_scope: r.get("schema_scope"),
                kind: match k_str.as_str() {
                    "goal" => RecordKind::Goal,
                    "insight" => RecordKind::Insight,
                    _ => RecordKind::Fact,
                },
                domain: r.get("domain"),
                entity_type: r.get("entity_type"),
                title: r.get("title"),
                data: r.get("data"),
                occurred_at: r.get("occurred_at"),
                source: r.get("source"),
                source_event_id: r.get("source_event_id"),
                source_record_ids: r.get("source_record_ids"),
                collection_id: r.get("collection_id"),
                valid_until: r.get("valid_until"),
                version: r.get("version"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            }
        }))
    }

    pub async fn list(
        &self,
        user_id: Uuid,
        domain: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Record>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, user_id, schema_id, schema_scope, kind, domain, entity_type,
                   title, data, occurred_at, source, source_event_id, source_record_ids,
                   collection_id, valid_until, version, created_at, updated_at
            FROM records
            WHERE user_id = $1 AND ($2::text IS NULL OR domain = $2)
            ORDER BY occurred_at DESC
            LIMIT $3
            "#,
        )
        .bind(user_id)
        .bind(domain)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let k_str: String = r.get("kind");
                Record {
                    id: r.get("id"),
                    user_id: r.get("user_id"),
                    schema_id: r.get("schema_id"),
                    schema_scope: r.get("schema_scope"),
                    kind: match k_str.as_str() {
                        "goal" => RecordKind::Goal,
                        "insight" => RecordKind::Insight,
                        _ => RecordKind::Fact,
                    },
                    domain: r.get("domain"),
                    entity_type: r.get("entity_type"),
                    title: r.get("title"),
                    data: r.get("data"),
                    occurred_at: r.get("occurred_at"),
                    source: r.get("source"),
                    source_event_id: r.get("source_event_id"),
                    source_record_ids: r.get("source_record_ids"),
                    collection_id: r.get("collection_id"),
                    valid_until: r.get("valid_until"),
                    version: r.get("version"),
                    created_at: r.get("created_at"),
                    updated_at: r.get("updated_at"),
                }
            })
            .collect())
    }

    pub async fn delete(&self, user_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM records WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }
}
