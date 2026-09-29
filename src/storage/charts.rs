use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::charts::{Chart, ChartBoard, ChartType};

#[derive(Clone)]
pub struct ChartRepository {
    pool: PgPool,
}

impl ChartRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create_board(&self, user_id: Uuid, name: &str) -> Result<ChartBoard, sqlx::Error> {
        let row = sqlx::query(
            r#"
            INSERT INTO chart_boards (user_id, name)
            VALUES ($1, $2)
            RETURNING id, user_id, name, created_at, updated_at
            "#,
        )
        .bind(user_id)
        .bind(name)
        .fetch_one(&self.pool)
        .await?;

        Ok(map_board_row(row))
    }

    pub async fn list_boards(&self, user_id: Uuid) -> Result<Vec<ChartBoard>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, user_id, name, created_at, updated_at
            FROM chart_boards
            WHERE user_id = $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(map_board_row).collect())
    }

    pub async fn get_board(
        &self,
        user_id: Uuid,
        board_id: Uuid,
    ) -> Result<Option<ChartBoard>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, name, created_at, updated_at
            FROM chart_boards
            WHERE user_id = $1 AND id = $2
            "#,
        )
        .bind(user_id)
        .bind(board_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(map_board_row))
    }

    pub async fn add_chart(
        &self,
        board_id: Uuid,
        title: &str,
        chart_type: ChartType,
        schema_ids: &[Uuid],
        query_spec: serde_json::Value,
    ) -> Result<Chart, sqlx::Error> {
        let row = sqlx::query(
            r#"
            INSERT INTO charts (board_id, title, chart_type, schema_ids, query_spec)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, board_id, title, chart_type, schema_ids, query_spec, created_at
            "#,
        )
        .bind(board_id)
        .bind(title)
        .bind(chart_type.as_str())
        .bind(schema_ids)
        .bind(query_spec)
        .fetch_one(&self.pool)
        .await?;

        Ok(map_chart_row(row))
    }

    pub async fn list_charts_for_board(&self, board_id: Uuid) -> Result<Vec<Chart>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, board_id, title, chart_type, schema_ids, query_spec, created_at
            FROM charts
            WHERE board_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(board_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(map_chart_row).collect())
    }
}

fn map_board_row(row: sqlx::postgres::PgRow) -> ChartBoard {
    ChartBoard {
        id: row.get("id"),
        user_id: row.get("user_id"),
        name: row.get("name"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn map_chart_row(row: sqlx::postgres::PgRow) -> Chart {
    let chart_type_str: String = row.get("chart_type");
    Chart {
        id: row.get("id"),
        board_id: row.get("board_id"),
        title: row.get("title"),
        chart_type: ChartType::parse(&chart_type_str).unwrap_or(ChartType::Line),
        schema_ids: row.get("schema_ids"),
        query_spec: row.get("query_spec"),
        created_at: row.get("created_at"),
    }
}
