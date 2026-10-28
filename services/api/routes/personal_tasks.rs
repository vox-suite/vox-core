use axum::{Extension, Json, extract::State, http::StatusCode};
use vox_core::{
    application::personal_tasks::{
        CreatePersonalTask, PersonalTaskError, PersonalTaskResult, PersonalTaskService,
    },
    domain::identity::Actor,
};
#[utoipa::path(post,path="/v1/personal-tasks",tag="tasks",request_body=CreatePersonalTask,
 responses((status=200,body=PersonalTaskResult),(status=201,body=PersonalTaskResult),(status=409,description="Request ID reused for different content"),(status=410,description="Original task was deleted")))]
pub async fn create(
    State(service): State<PersonalTaskService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<CreatePersonalTask>,
) -> Result<(StatusCode, Json<PersonalTaskResult>), (StatusCode, Json<serde_json::Value>)> {
    let result = service.create(&actor, input).await.map_err(|error| {
        let status = match &error {
            PersonalTaskError::Invalid(_) => StatusCode::BAD_REQUEST,
            PersonalTaskError::Conflict => StatusCode::CONFLICT,
            PersonalTaskError::Deleted => StatusCode::GONE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(serde_json::json!({"error":error.to_string()})))
    })?;
    Ok((
        if result.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(result),
    ))
}
