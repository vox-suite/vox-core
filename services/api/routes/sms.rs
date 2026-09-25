use axum::{Extension, Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use vox_core::{
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
    let batch_id = service
        .submit_batch(actor.user_id, body.messages)
        .await
        .map_err(|err| match err {
            SmsIngestionError::Empty => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "batch_id": batch_id })),
    ))
}
