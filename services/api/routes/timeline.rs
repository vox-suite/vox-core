use axum::{
    Extension, Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use vox_core::{
    domain::identity::Actor,
    timeline::{TimelineError, TimelineService, TimelineSource},
};

#[derive(Deserialize)]
pub struct GetTimelineQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub types: Option<String>,
}

pub async fn get_timeline(
    State(service): State<TimelineService>,
    Extension(actor): Extension<Actor>,
    Query(query): Query<GetTimelineQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let types = query
        .types
        .as_deref()
        .map(|raw| raw.split(',').filter_map(parse_source).collect::<Vec<_>>());

    let entries = service
        .list(actor.user_id, query.from, query.to, types.as_deref())
        .await
        .map_err(|e| match e {
            TimelineError::Invalid(_) => StatusCode::BAD_REQUEST,
            TimelineError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        })?;

    Ok(Json(entries))
}

fn parse_source(s: &str) -> Option<TimelineSource> {
    match s.trim() {
        "task" => Some(TimelineSource::Task),
        "schedule" => Some(TimelineSource::Schedule),
        "reminder" => Some(TimelineSource::Reminder),
        _ => None,
    }
}
