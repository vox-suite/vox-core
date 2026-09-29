use axum::{Extension, Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use vox_core::{
    consent::{ConsentError, ConsentService, DataSource},
    domain::identity::Actor,
    sms_ingestion::{SmsIngestionError, SmsIngestionService, SmsMessage},
};

#[derive(Deserialize)]
pub struct SubmitSmsBatchRequest {
    pub messages: Vec<SmsMessage>,
}

pub async fn submit_batch(
    State(service): State<SmsIngestionService>,
    Extension(actor): Extension<Actor>,
    Json(body): Json<SubmitSmsBatchRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let event_ids = service
        .submit_batch(actor.user_id, body.messages)
        .await
        .map_err(|err| match err {
            SmsIngestionError::Empty => StatusCode::BAD_REQUEST,
            SmsIngestionError::ConsentRequired => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "event_ids": event_ids,
            "batch_id": event_ids.first().map(|e| e.0),
        })),
    ))
}

pub async fn get_consent(
    State(service): State<ConsentService>,
    Extension(actor): Extension<Actor>,
) -> Result<impl IntoResponse, StatusCode> {
    let status = service
        .status(actor.user_id, DataSource::Sms)
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
        .grant(actor.user_id, DataSource::Sms, body.retention_days)
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
        .revoke(actor.user_id, DataSource::Sms)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}
