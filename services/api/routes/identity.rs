/**
 * HTTP handlers for identity mapping, channels, and caller profiles.
 */

use axum::{
    Extension, Json,
    http::StatusCode,
    response::IntoResponse,
};
use vox_core::domain::identity::Actor;

pub async fn get_me(
    Extension(actor): Extension<Actor>,
) -> Result<impl IntoResponse, StatusCode> {
    Ok(Json(serde_json::json!({
        "user_id": actor.user_id,
        "principal_id": actor.principal_id,
        "grants": actor.grants,
    })))
}
