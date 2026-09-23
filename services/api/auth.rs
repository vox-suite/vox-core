/**
* Authentication and authorization middleware for incoming API requests.
*/
use axum::{
    extract::{Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use vox_core::domain::identity::Actor;

pub async fn extract_actor(
    State(pool): State<PgPool>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token = match auth_header.strip_prefix("Bearer ") {
        Some(t) if !t.trim().is_empty() => t.trim(),
        _ => return Err(StatusCode::UNAUTHORIZED),
    };

    if token.split('.').count() == 3 {
        let claims = crate::identity_token::verify_id_token(token).await?;
        let user_id = Uuid::parse_str(&claims.subject).map_err(|_| StatusCode::UNAUTHORIZED)?;
        let display_name = claims.email.as_deref().unwrap_or("Vox User");
        let _ = sqlx::query(
            "INSERT INTO users (id, status, display_name) \
             VALUES ($1, 'active', $2) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(user_id)
        .bind(display_name)
        .execute(&pool)
        .await;

        let actor = Actor::user(user_id);
        req.extensions_mut().insert(actor);
        return Ok(next.run(req).await);
    }

    // 2. Otherwise check session tokens in auth_sessions
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let session_row = sqlx::query(
        "SELECT user_id, device_id FROM auth_sessions \
         WHERE token_hash = $1 \
           AND expires_at > now() \
           AND revoked_at IS NULL",
    )
    .bind(token_hash)
    .fetch_optional(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNAUTHORIZED)?;

    let user_id: Uuid = session_row.get("user_id");
    let device_id: Option<Uuid> = session_row.get("device_id");

    let actor = if let Some(dev_id) = device_id {
        Actor::device(user_id, dev_id)
    } else {
        Actor::user(user_id)
    };
    req.extensions_mut().insert(actor);

    Ok(next.run(req).await)
}
