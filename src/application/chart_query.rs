use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::charts::{Aggregation, GroupBy, QuerySpec};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartDataPoint {
    pub label: String,
    pub value: f64,
}

pub async fn compute_chart_data(
    pool: &PgPool,
    user_id: Uuid,
    schema_ids: &[Uuid],
    query_spec: &QuerySpec,
) -> Result<Vec<ChartDataPoint>, sqlx::Error> {
    if schema_ids.is_empty() {
        return Ok(Vec::new());
    }

    let agg_expr = match query_spec.aggregation {
        Aggregation::Sum => "COALESCE(SUM(val_num), 0)",
        Aggregation::Count => "COUNT(val_num)",
        Aggregation::Avg => "COALESCE(AVG(val_num), 0)",
        Aggregation::Min => "COALESCE(MIN(val_num), 0)",
        Aggregation::Max => "COALESCE(MAX(val_num), 0)",
    };

    match &query_spec.group_by {
        GroupBy::Day | GroupBy::Week | GroupBy::Month => {
            let (date_part, date_fmt) = match &query_spec.group_by {
                GroupBy::Day => ("day", "YYYY-MM-DD"),
                GroupBy::Week => ("week", "YYYY-MM-DD"),
                GroupBy::Month => ("month", "YYYY-MM"),
                GroupBy::Field(_) => unreachable!(),
            };

            let query_str = format!(
                r#"
                WITH extracted AS (
                    SELECT
                        start_at,
                        created_at,
                        CASE
                            WHEN (data->>$3) ~ '^-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?$'
                            THEN (data->>$3)::numeric
                            ELSE NULL
                        END AS val_num
                    FROM spans
                    WHERE user_id = $1
                      AND schema_id = ANY($2)
                )
                SELECT
                    to_char(date_trunc('{date_part}', COALESCE(start_at, created_at)), '{date_fmt}') AS label,
                    ({agg_expr})::float8 AS value
                FROM extracted
                GROUP BY date_trunc('{date_part}', COALESCE(start_at, created_at))
                ORDER BY date_trunc('{date_part}', COALESCE(start_at, created_at)) ASC
                "#
            );

            let rows = sqlx::query(&query_str)
                .bind(user_id)
                .bind(schema_ids)
                .bind(&query_spec.metric_field)
                .fetch_all(pool)
                .await?;

            let points = rows
                .into_iter()
                .map(|r| ChartDataPoint {
                    label: r.get::<Option<String>, _>("label").unwrap_or_else(|| "unknown".to_string()),
                    value: r.get::<f64, _>("value"),
                })
                .collect();

            Ok(points)
        }
        GroupBy::Field(field_name) => {
            let query_str = format!(
                r#"
                WITH extracted AS (
                    SELECT
                        data,
                        CASE
                            WHEN (data->>$3) ~ '^-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?$'
                            THEN (data->>$3)::numeric
                            ELSE NULL
                        END AS val_num
                    FROM spans
                    WHERE user_id = $1
                      AND schema_id = ANY($2)
                )
                SELECT
                    COALESCE(NULLIF(data->>$4, ''), 'unknown') AS label,
                    ({agg_expr})::float8 AS value
                FROM extracted
                GROUP BY COALESCE(NULLIF(data->>$4, ''), 'unknown')
                ORDER BY value DESC
                LIMIT 100
                "#
            );

            let rows = sqlx::query(&query_str)
                .bind(user_id)
                .bind(schema_ids)
                .bind(&query_spec.metric_field)
                .bind(field_name)
                .fetch_all(pool)
                .await?;

            let mut points: Vec<ChartDataPoint> = rows
                .into_iter()
                .map(|r| ChartDataPoint {
                    label: r.get::<String, _>("label"),
                    value: r.get::<f64, _>("value"),
                })
                .collect();

            if points.len() > 20 {
                let remainder = points.split_off(20);
                let remainder_val = match query_spec.aggregation {
                    Aggregation::Sum | Aggregation::Count => {
                        remainder.iter().map(|p| p.value).sum()
                    }
                    Aggregation::Avg => {
                        let count = remainder.len() as f64;
                        if count > 0.0 {
                            remainder.iter().map(|p| p.value).sum::<f64>() / count
                        } else {
                            0.0
                        }
                    }
                    Aggregation::Min => {
                        remainder
                            .iter()
                            .map(|p| p.value)
                            .fold(f64::INFINITY, f64::min)
                    }
                    Aggregation::Max => {
                        remainder
                            .iter()
                            .map(|p| p.value)
                            .fold(f64::NEG_INFINITY, f64::max)
                    }
                };
                points.push(ChartDataPoint {
                    label: "other".to_string(),
                    value: remainder_val,
                });
            }

            Ok(points)
        }
    }
}
