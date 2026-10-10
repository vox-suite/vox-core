use crate::state::ApiState;
use axum::{Extension, Json, extract::State, http::StatusCode};
use serde_json::Value;
use vox_core::{
    desktop::actions::{ActionError, ResourceActions, ResourceRequest},
    domain::identity::Actor,
};
#[utoipa::path(post,path="/v1/me/desktop-actions",tag="desktop",request_body=ResourceRequest,responses((status=200,body=Value)))]
pub async fn execute(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<ResourceRequest>,
) -> Result<Json<Value>, StatusCode> {
    let result = ResourceActions::new(state.pool)
        .execute(actor.user_id, input)
        .await
        .map_err(|e| match e {
            ActionError::NotFound => StatusCode::NOT_FOUND,
            ActionError::Conflict => StatusCode::CONFLICT,
            ActionError::Invalid(_) => StatusCode::BAD_REQUEST,
            ActionError::Database(_) => StatusCode::SERVICE_UNAVAILABLE,
        })?;
    state.user_events.notify(
        actor.user_id,
        serde_json::json!({"type":"space_graph_updated","space_id":result["space_id"]}),
    );
    Ok(Json(result))
}
