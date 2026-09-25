use axum::{Extension, Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use vox_core::{
    consent::{ConsentError, ConsentService, DataSource},
    domain::identity::Actor,
    location_ingestion::{LocationIngestionError, LocationIngestionService, LocationSegment},
};

#[derive(Deserialize)]
pub struct SubmitLocationSegmentsRequest {
    pub segments: Vec<LocationSegment>,
}

pub async fn submit_segments(
    State(service): State<LocationIngestionService>,
    Extension(actor): Extension<Actor>,
    Json(body): Json<SubmitLocationSegmentsRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let ids = service
        .submit_segments(actor.user_id, body.segments)
        .await
        .map_err(|err| match err {
            LocationIngestionError::InvalidSegment => StatusCode::BAD_REQUEST,
            LocationIngestionError::ConsentRequired => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;

    Ok((StatusCode::CREATED, Json(serde_json::json!({ "ids": ids }))))
}

pub async fn get_consent(
    State(service): State<ConsentService>,
    Extension(actor): Extension<Actor>,
) -> Result<impl IntoResponse, StatusCode> {
    let status = service
        .status(actor.user_id, DataSource::Location)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(status))
}

#[derive(Deserialize)]
pub struct GrantConsentRequest {
    pub retention_days: Option<i32>,
}

pub async fn grant_consent(
    State(service): State<ConsentService>,
    Extension(actor): Extension<Actor>,
    Json(body): Json<GrantConsentRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let status = service
        .grant(actor.user_id, DataSource::Location, body.retention_days)
        .await
        .map_err(|err| match err {
            ConsentError::InvalidRetention => StatusCode::BAD_REQUEST,
            ConsentError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        })?;
    Ok(Json(status))
}

pub async fn revoke_consent(
    State(service): State<ConsentService>,
    Extension(actor): Extension<Actor>,
) -> Result<impl IntoResponse, StatusCode> {
    service
        .revoke(actor.user_id, DataSource::Location)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}
