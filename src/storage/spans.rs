/**
* Storage repository for spans and their automatic nesting.
*/
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::domain::{
    ConcurrencyOutcome,
    spans::{ExecutionType, NewSpan, Span, SpanPatch, SpanQuery, SpanStatus},
};

const SPAN_COLUMNS: &str = "s.id, s.user_id, s.parent_id, s.title, s.notes, s.category, s.source, \
    s.source_ref, s.status, s.start_at, s.end_at, s.due_at, s.priority, s.execution_type, \
    s.execution_result, s.data, s.version, s.completed_at, s.created_at, s.updated_at, \
    ARRAY(SELECT cs.collection_id FROM collection_spans cs WHERE cs.span_id = s.id) AS collection_ids";

#[derive(Clone)]
pub struct SpanRepository {
    pool: PgPool,
}

impl SpanRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, user_id: Uuid, input: NewSpan) -> Result<Span, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let id = insert(&mut tx, user_id, &input).await?;
        tx.commit().await?;
        self.get_by_id(user_id, id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)
    }

    pub async fn record(&self, user_id: Uuid, input: NewSpan) -> Result<Uuid, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        if let (Some(source), Some(source_ref)) = (&input.source, &input.source_ref) {
            let existing = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM spans WHERE user_id = $1 AND source = $2 AND source_ref = $3",
            )
            .bind(user_id)
            .bind(source)
            .bind(source_ref)
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(id) = existing {
                return Ok(id);
            }
        }
        let id = insert(&mut tx, user_id, &input).await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn get_by_id(&self, user_id: Uuid, id: Uuid) -> Result<Option<Span>, sqlx::Error> {
        let row = sqlx::query(&format!(
            "SELECT {SPAN_COLUMNS} FROM spans s WHERE s.id = $1 AND s.user_id = $2"
        ))
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(map_span))
    }

    pub async fn list(&self, user_id: Uuid, query: &SpanQuery) -> Result<Vec<Span>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {SPAN_COLUMNS} FROM spans s
             WHERE s.user_id = $1
               AND ($2::timestamptz IS NULL OR COALESCE(s.end_at, s.start_at) >= $2)
               AND ($3::timestamptz IS NULL OR s.start_at < $3)
               AND ($4::uuid IS NULL OR EXISTS (
                    SELECT 1 FROM collection_spans cs WHERE cs.span_id = s.id AND cs.collection_id = $4))
               AND ($5::text IS NULL OR s.status = $5)
               AND (NOT $6 OR s.start_at IS NULL)
             ORDER BY s.start_at NULLS LAST, s.created_at
             LIMIT $7"
        ))
        .bind(user_id)
        .bind(query.from)
        .bind(query.to)
        .bind(query.collection_id)
        .bind(query.status.map(SpanStatus::as_str))
        .bind(query.unscheduled)
        .bind(query.limit.unwrap_or(500).clamp(1, 2000))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(map_span).collect())
    }

    pub async fn update(
        &self,
        user_id: Uuid,
        id: Uuid,
        patch: SpanPatch,
    ) -> Result<ConcurrencyOutcome<Span>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<
            _,
            (
                i32,
                Option<chrono::DateTime<chrono::Utc>>,
                Option<chrono::DateTime<chrono::Utc>>,
            ),
        >(
            "SELECT version, start_at, end_at FROM spans WHERE id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((current, old_start_at, old_end_at)) = row else {
            return Ok(ConcurrencyOutcome::NotFound);
        };
        if patch.expected_version.is_some_and(|v| v != current) {
            return Ok(ConcurrencyOutcome::Conflict);
        }

        let times_changed = patch.start_at.is_some_and(|v| v != old_start_at)
            || patch.end_at.is_some_and(|v| v != old_end_at);
        sqlx::query(
            "UPDATE spans SET
                title = COALESCE($3, title),
                notes = COALESCE($4, notes),
                category = COALESCE($5, category),
                status = COALESCE($6, status),
                start_at = CASE WHEN $7 THEN $8 ELSE start_at END,
                end_at = CASE WHEN $9 THEN $10 ELSE end_at END,
                due_at = CASE WHEN $11 THEN $12 ELSE due_at END,
                priority = COALESCE($13, priority),
                execution_result = COALESCE($14, execution_result),
                data = COALESCE($15, data),
                completed_at = CASE WHEN $6 = 'done' THEN COALESCE(completed_at, now())
                                    WHEN $6 IS NOT NULL THEN NULL ELSE completed_at END,
                version = version + 1,
                updated_at = now()
             WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .bind(patch.title)
        .bind(patch.notes)
        .bind(patch.category)
        .bind(patch.status.map(SpanStatus::as_str))
        .bind(patch.start_at.is_some())
        .bind(patch.start_at.flatten())
        .bind(patch.end_at.is_some())
        .bind(patch.end_at.flatten())
        .bind(patch.due_at.is_some())
        .bind(patch.due_at.flatten())
        .bind(patch.priority)
        .bind(patch.execution_result)
        .bind(patch.data)
        .execute(&mut *tx)
        .await?;

        if times_changed {
            sqlx::query("UPDATE spans SET parent_id = NULL WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            nest(&mut tx, user_id, id).await?;
        }
        tx.commit().await?;

        Ok(match self.get_by_id(user_id, id).await? {
            Some(span) => ConcurrencyOutcome::Success(span),
            None => ConcurrencyOutcome::NotFound,
        })
    }

    pub async fn delete(&self, user_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM spans WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    input: &NewSpan,
) -> Result<Uuid, sqlx::Error> {
    let status = input.status.unwrap_or_default();
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO spans (user_id, parent_id, title, notes, category, source, source_ref, status,
                            start_at, end_at, due_at, priority, execution_type, data, completed_at)
         VALUES ($1, $2, $3, $4, COALESCE($5, 'general'), COALESCE($6, 'user'), $7, $8,
                 $9, $10, $11, COALESCE($12, 0), $13, COALESCE($14, '{}'::jsonb),
                 CASE WHEN $8 = 'done' THEN COALESCE($10, $9, now()) END)
         RETURNING id",
    )
    .bind(user_id)
    .bind(input.parent_id)
    .bind(input.title.trim())
    .bind(input.notes.trim())
    .bind(input.category.as_deref())
    .bind(input.source.as_deref())
    .bind(input.source_ref.as_deref())
    .bind(status.as_str())
    .bind(input.start_at)
    .bind(input.end_at)
    .bind(input.due_at)
    .bind(input.priority)
    .bind(input.execution_type.map(ExecutionType::as_str))
    .bind(input.data.clone())
    .fetch_one(&mut **tx)
    .await?;

    for collection_id in &input.collection_ids {
        sqlx::query(
            "INSERT INTO collection_spans (collection_id, span_id, user_id) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(collection_id)
        .bind(id)
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    }

    if input.parent_id.is_none() {
        nest(tx, user_id, id).await?;
    }
    Ok(id)
}

async fn nest(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "WITH me AS (
             SELECT id, start_at, COALESCE(end_at, start_at) AS end_at,
                    COALESCE(end_at, start_at) - start_at AS dur
             FROM spans WHERE id = $1 AND start_at IS NOT NULL
         ), parent AS (
             SELECT p.id FROM spans p, me
             WHERE p.user_id = $2 AND p.id <> me.id AND p.end_at IS NOT NULL
               AND p.start_at <= me.start_at AND p.end_at >= me.end_at
               AND p.end_at - p.start_at > me.dur
               AND p.end_at - p.start_at <= interval '1 day'
             ORDER BY p.end_at - p.start_at
             LIMIT 1
         )
         UPDATE spans SET parent_id = (SELECT id FROM parent) WHERE id = (SELECT id FROM me)",
    )
    .bind(id)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "WITH me AS (
             SELECT id, parent_id, start_at, end_at, end_at - start_at AS dur
             FROM spans WHERE id = $1 AND start_at IS NOT NULL AND end_at IS NOT NULL
               AND end_at - start_at <= interval '1 day'
         )
         UPDATE spans c SET parent_id = me.id
         FROM me
         WHERE c.user_id = $2 AND c.id <> me.id AND c.start_at IS NOT NULL
           AND c.start_at >= me.start_at AND COALESCE(c.end_at, c.start_at) <= me.end_at
           AND COALESCE(c.end_at, c.start_at) - c.start_at < me.dur
           AND c.parent_id IS NOT DISTINCT FROM me.parent_id",
    )
    .bind(id)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn map_span(row: PgRow) -> Span {
    let status: String = row.get("status");
    let execution_type: Option<String> = row.get("execution_type");
    Span {
        id: row.get("id"),
        user_id: row.get("user_id"),
        parent_id: row.get("parent_id"),
        title: row.get("title"),
        notes: row.get("notes"),
        category: row.get("category"),
        source: row.get("source"),
        source_ref: row.get("source_ref"),
        status: SpanStatus::parse(&status).unwrap_or_default(),
        start_at: row.get("start_at"),
        end_at: row.get("end_at"),
        due_at: row.get("due_at"),
        priority: row.get("priority"),
        execution_type: execution_type.as_deref().and_then(ExecutionType::parse),
        execution_result: row.get("execution_result"),
        data: row.get("data"),
        collection_ids: row.get("collection_ids"),
        version: row.get("version"),
        completed_at: row.get("completed_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
