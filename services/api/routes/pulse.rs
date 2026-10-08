use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;
use vox_core::{
    application::pulse::service::{PulseError, PulseService},
    domain::{identity::Actor, pulse::*, pulse_goals::*},
};

impl IntoResponse for PulseApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            PulseError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            PulseError::Conflict(_) => StatusCode::CONFLICT,
            PulseError::Busy => StatusCode::SERVICE_UNAVAILABLE,
            PulseError::Database(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        if let PulseError::Database(error) = &self.0 {
            tracing::warn!(error=%error,"Pulse request failed");
        }
        (
            status,
            Json(serde_json::json!({"error":self.0.to_string()})),
        )
            .into_response()
    }
}
pub struct PulseApiError(PulseError);
impl From<PulseError> for PulseApiError {
    fn from(error: PulseError) -> Self {
        Self(error)
    }
}
#[derive(Deserialize)]
pub struct CanvasQuery {
    pub timezone: String,
    #[serde(default)]
    pub refresh: bool,
    pub cursor: Option<Uuid>,
}
#[derive(Deserialize)]
pub struct MeasurementsQuery {
    pub timezone: String,
}

#[utoipa::path(get,path="/v1/me/pulse/canvas",tag="pulse",params(("timezone"=String,Query),("refresh"=Option<bool>,Query),("cursor"=Option<Uuid>,Query)),responses((status=200,body=CanvasResponse)))]
pub async fn get_canvas(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Query(input): Query<CanvasQuery>,
) -> Result<Json<CanvasResponse>, PulseApiError> {
    Ok(Json(
        service
            .canvas(&actor, &input.timezone, input.refresh, input.cursor)
            .await?,
    ))
}
#[utoipa::path(get,path="/v1/me/pulse/measurements",tag="pulse",params(("timezone"=String,Query)),responses((status=200,body=Vec<Measurement>)))]
pub async fn list_measurements(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Query(input): Query<MeasurementsQuery>,
) -> Result<Json<Vec<Measurement>>, PulseApiError> {
    Ok(Json(service.measurements(&actor, &input.timezone).await?))
}
#[utoipa::path(post,path="/v1/me/pulse/suggestions",tag="pulse",request_body=DiscoveryInput,responses((status=200,body=DiscoveryResponse)))]
pub async fn discover(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<DiscoveryInput>,
) -> Result<Json<DiscoveryResponse>, PulseApiError> {
    Ok(Json(service.discover(&actor, input).await?))
}
#[utoipa::path(post,path="/v1/me/pulse/preview",tag="pulse",request_body=PulseDefinition,responses((status=200,body=PulseResult)))]
pub async fn preview(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<PulseDefinition>,
) -> Result<Json<PulseResult>, PulseApiError> {
    Ok(Json(service.preview(&actor, input).await?))
}
#[utoipa::path(post,path="/v1/me/pulse/charts",tag="pulse",request_body=SavePulseInput,responses((status=201,body=SavedPulseChart)))]
pub async fn save(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<SavePulseInput>,
) -> Result<impl IntoResponse, PulseApiError> {
    Ok((
        StatusCode::CREATED,
        Json(service.save(&actor, input).await?),
    ))
}
#[utoipa::path(delete,path="/v1/me/pulse/charts/{id}",tag="pulse",params(("id"=Uuid,Path)),responses((status=204),(status=404)))]
pub async fn delete_chart(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, PulseApiError> {
    Ok(if service.delete_chart(&actor, id).await? {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    })
}
#[utoipa::path(post,path="/v1/me/pulse/dismissals",tag="pulse",request_body=PulseDefinition,responses((status=204)))]
pub async fn dismiss(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<PulseDefinition>,
) -> Result<StatusCode, PulseApiError> {
    service.dismiss(&actor, input).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post,path="/v1/me/pulse/compose",tag="pulse",request_body=ComposeInput,responses((status=200,body=ComposeResponse)))]
pub async fn compose(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<ComposeInput>,
) -> Result<Json<ComposeResponse>, PulseApiError> {
    Ok(Json(service.compose(&actor, input).await?))
}
#[derive(Deserialize)]
pub struct GoalsQuery {
    pub timezone: String,
}
#[utoipa::path(get,path="/v1/me/pulse/goals",tag="pulse",params(("timezone"=String,Query)),responses((status=200,body=Vec<GoalView>)))]
pub async fn list_goals(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Query(input): Query<GoalsQuery>,
) -> Result<Json<Vec<GoalView>>, PulseApiError> {
    Ok(Json(service.goals(&actor, &input.timezone).await?))
}
#[utoipa::path(post,path="/v1/me/pulse/goals",tag="pulse",request_body=GoalDraft,responses((status=201,body=GoalView)))]
pub async fn create_goal(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<GoalDraft>,
) -> Result<impl IntoResponse, PulseApiError> {
    Ok((
        StatusCode::CREATED,
        Json(service.create_goal(&actor, input).await?),
    ))
}
#[utoipa::path(delete,path="/v1/me/pulse/goals/{id}",tag="pulse",params(("id"=Uuid,Path)),responses((status=204)))]
pub async fn delete_goal(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<StatusCode, PulseApiError> {
    service.delete_goal(&actor, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post,path="/v1/me/pulse/goals/{id}/entries",tag="pulse",params(("id"=Uuid,Path)),request_body=AddGoalEntryInput,responses((status=200,body=GoalView)))]
pub async fn add_goal_entry(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(input): Json<AddGoalEntryInput>,
) -> Result<Json<GoalView>, PulseApiError> {
    Ok(Json(service.add_goal_entry(&actor, id, input).await?))
}
#[utoipa::path(post,path="/v1/me/pulse/goals/compose",tag="pulse",request_body=GoalComposeInput,responses((status=200,body=GoalComposeResponse)))]
pub async fn compose_goal(
    State(service): State<PulseService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<GoalComposeInput>,
) -> Result<Json<GoalComposeResponse>, PulseApiError> {
    Ok(Json(service.compose_goal(&actor, input).await?))
}
