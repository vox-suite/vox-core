/**
* HTTP handlers for identity mapping, channels, and caller profiles.
*/
use axum::{Extension, Json, extract::State, http::StatusCode, response::IntoResponse};
use sqlx::PgPool;
use vox_core::domain::identity::Actor;

pub async fn get_me(
    Extension(actor): Extension<Actor>,
    State(pool): State<PgPool>,
) -> Result<impl IntoResponse, StatusCode> {
    let has_phone = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(\
            SELECT 1 FROM channel_identities \
            WHERE user_id = $1 AND channel = 'phone' AND revoked_at IS NULL\
        )",
    )
    .bind(actor.user_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({
        "user_id": actor.user_id,
        "principal_id": actor.principal_id,
        "grants": actor.grants,
        "has_phone": has_phone,
    })))
}
