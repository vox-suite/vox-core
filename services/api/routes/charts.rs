use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use vox_core::{
    agents::chart_suggester::{
        ChartSuggestion, ChartSuggestionPrompt, SchemaSample, SuggestingCharts,
    },
    application::{
        chart_query::{ChartDataPoint, compute_chart_data},
        schemas::SchemaService,
    },
    domain::{
        charts::{Chart, ChartType, QuerySpec},
        identity::Actor,
    },
    storage::charts::ChartRepository,
};

#[derive(Clone)]
pub struct ChartApiState {
    pub pool: PgPool,
    pub charts: ChartRepository,
    pub schemas: SchemaService,
    pub suggester: Arc<dyn SuggestingCharts>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum SuggestChartsInput {
    List(Vec<Uuid>),
    Object { schema_ids: Vec<Uuid> },
}

impl SuggestChartsInput {
    pub fn into_schema_ids(self) -> Vec<Uuid> {
        match self {
            Self::List(ids) => ids,
            Self::Object { schema_ids } => schema_ids,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateChartBoardInput {
    pub name: String,
    pub charts: Vec<ChartSuggestion>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChartBoardSummary {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub chart_count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChartBoardDetails {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub charts: Vec<Chart>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChartDataResult {
    pub chart_id: Uuid,
    pub data_points: Vec<ChartDataPoint>,
    pub error: Option<String>,
}

pub async fn suggest_charts(
    State(state): State<ChartApiState>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<SuggestChartsInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let schema_ids = input.into_schema_ids();
    if schema_ids.is_empty() || schema_ids.len() > 8 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let user_schemas = state
        .schemas
        .get_by_ids(&actor, &schema_ids)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if user_schemas.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut schema_samples = Vec::new();
    for s in user_schemas {
        let sample_rows = sqlx::query(
            r#"
            SELECT data
            FROM spans
            WHERE user_id = $1 AND schema_id = $2
            ORDER BY COALESCE(start_at, created_at) DESC
            LIMIT 5
            "#,
        )
        .bind(actor.user_id)
        .bind(s.id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();

        let sample_data = sample_rows
            .into_iter()
            .map(|r| r.get::<serde_json::Value, _>("data"))
            .collect();

        schema_samples.push(SchemaSample {
            id: s.id,
            namespace: s.namespace,
            name: s.name,
            description: s.description,
            json_schema: s.json_schema,
            sample_data,
        });
    }

    let suggestions = state
        .suggester
        .suggest(ChartSuggestionPrompt {
            schemas: schema_samples,
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(suggestions))
}

pub async fn create_chart_board(
    State(state): State<ChartApiState>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<CreateChartBoardInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let trimmed_name = input.name.trim();
    if trimmed_name.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let board = state
        .charts
        .create_board(actor.user_id, trimmed_name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut created_charts = Vec::new();
    for suggestion in input.charts {
        let chart_type = ChartType::parse(&suggestion.chart_type).unwrap_or(ChartType::Line);

        let valid_schemas = state
            .schemas
            .get_by_ids(&actor, &suggestion.schema_ids)
            .await
            .unwrap_or_default();
        let valid_ids: Vec<Uuid> = valid_schemas.into_iter().map(|s| s.id).collect();
        if valid_ids.is_empty() {
            continue;
        }

        let query_spec_json = serde_json::to_value(&suggestion.query_spec)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let chart = state
            .charts
            .add_chart(
                board.id,
                &suggestion.title,
                chart_type,
                &valid_ids,
                query_spec_json,
            )
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        created_charts.push(chart);
    }

    let response = ChartBoardDetails {
        id: board.id,
        user_id: board.user_id,
        name: board.name,
        created_at: board.created_at,
        updated_at: board.updated_at,
        charts: created_charts,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

pub async fn list_chart_boards(
    State(state): State<ChartApiState>,
    Extension(actor): Extension<Actor>,
) -> Result<impl IntoResponse, StatusCode> {
    let rows = sqlx::query(
        r#"
        SELECT b.id, b.user_id, b.name, b.created_at, b.updated_at, COUNT(c.id) AS chart_count
        FROM chart_boards b
        LEFT JOIN charts c ON c.board_id = b.id
        WHERE b.user_id = $1
        GROUP BY b.id
        ORDER BY b.created_at DESC
        "#,
    )
    .bind(actor.user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let boards: Vec<ChartBoardSummary> = rows
        .into_iter()
        .map(|r| ChartBoardSummary {
            id: r.get("id"),
            user_id: r.get("user_id"),
            name: r.get("name"),
            created_at: r.get("created_at"),
            updated_at: r.get("updated_at"),
            chart_count: r.get("chart_count"),
        })
        .collect();

    Ok(Json(boards))
}

pub async fn get_chart_board(
    State(state): State<ChartApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let board = state
        .charts
        .get_board(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let board = match board {
        Some(b) => b,
        None => return Err(StatusCode::NOT_FOUND),
    };

    let charts = state
        .charts
        .list_charts_for_board(board.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let details = ChartBoardDetails {
        id: board.id,
        user_id: board.user_id,
        name: board.name,
        created_at: board.created_at,
        updated_at: board.updated_at,
        charts,
    };

    Ok(Json(details))
}

pub async fn get_chart_board_data(
    State(state): State<ChartApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let board = state
        .charts
        .get_board(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let board = match board {
        Some(b) => b,
        None => return Err(StatusCode::NOT_FOUND),
    };

    let charts = state
        .charts
        .list_charts_for_board(board.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut results = Vec::new();
    for chart in charts {
        let query_spec: Result<QuerySpec, _> = serde_json::from_value(chart.query_spec.clone());
        let query_spec = match query_spec {
            Ok(qs) => qs,
            Err(e) => {
                results.push(ChartDataResult {
                    chart_id: chart.id,
                    data_points: Vec::new(),
                    error: Some(format!("Invalid query spec: {e}")),
                });
                continue;
            }
        };

        let valid_schemas = state
            .schemas
            .get_by_ids(&actor, &chart.schema_ids)
            .await
            .unwrap_or_default();
        let valid_ids: Vec<Uuid> = valid_schemas.into_iter().map(|s| s.id).collect();

        if valid_ids.is_empty() {
            results.push(ChartDataResult {
                chart_id: chart.id,
                data_points: Vec::new(),
                error: Some("Category not found or deleted".to_string()),
            });
            continue;
        }

        match compute_chart_data(&state.pool, actor.user_id, &valid_ids, &query_spec).await {
            Ok(data_points) => {
                results.push(ChartDataResult {
                    chart_id: chart.id,
                    data_points,
                    error: None,
                });
            }
            Err(e) => {
                results.push(ChartDataResult {
                    chart_id: chart.id,
                    data_points: Vec::new(),
                    error: Some(e.to_string()),
                });
            }
        }
    }

    Ok(Json(results))
}
