/**
* Session token exchange endpoint for authenticating clients and devices.
*/
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::identity_token::verify_id_token;

#[derive(Debug, Deserialize)]
pub struct AuthExchangeRequest {
    pub id_token: String,
    pub device_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct AuthExchangeResponse {
    pub token: String,
    pub user_id: Uuid,
    pub expires_at: chrono::DateTime<Utc>,
    pub has_phone: bool,
}

pub async fn exchange_token(
    State(pool): State<PgPool>,
    Json(payload): Json<AuthExchangeRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let identity = verify_id_token(&payload.id_token).await?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let existing = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT user_id, id FROM auth_identities WHERE issuer = $1 AND subject = $2",
    )
    .bind(&identity.issuer)
    .bind(&identity.subject)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let (user_id, identity_id) = if let Some(row) = existing {
        row
    } else {
        let display_name = identity
            .name
            .as_deref()
            .or(identity.email.as_deref())
            .unwrap_or("Vox User");
        let new_uid = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO users (status, display_name) VALUES ('active', $1) RETURNING id",
        )
        .bind(display_name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let context_inserted = sqlx::query(
            "INSERT INTO user_contexts (deployment_id, host_app_id, host_user_id, user_id) \
             SELECT d.id, h.id, $1::text, $2::uuid \
             FROM platform_deployments d \
             JOIN host_apps h ON h.deployment_id = d.id \
             WHERE d.external_key = 'vox.standalone.deployment' \
               AND h.external_key = 'vox.standalone.web'",
        )
        .bind(new_uid.to_string())
        .bind(new_uid)
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if context_inserted.rows_affected() != 1 {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }

        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO auth_identities (user_id, issuer, subject) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (issuer, subject) DO NOTHING \
             RETURNING id",
        )
        .bind(new_uid)
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        if let Some(id) = inserted {
            (new_uid, id)
        } else {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(new_uid)
                .execute(&mut *tx)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            sqlx::query_as::<_, (Uuid, Uuid)>(
                "SELECT user_id, id FROM auth_identities WHERE issuer = $1 AND subject = $2",
            )
            .bind(&identity.issuer)
            .bind(&identity.subject)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        }
    };

    let has_context = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM user_contexts WHERE user_id = $1)",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !has_context {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let has_phone = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(\
            SELECT 1 FROM channel_identities \
            WHERE user_id = $1 AND channel = 'phone' AND revoked_at IS NULL\
        )",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let device_id = if let Some(device_id) = payload.device_id {
        let owns_device = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM devices WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL)",
        )
        .bind(device_id)
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if !owns_device {
            return Err(StatusCode::BAD_REQUEST);
        }
        Some(device_id)
    } else {
        None
    };

    let raw_token = format!(
        "vox_sess_{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );
    let token_hash = hex::encode(Sha256::digest(raw_token.as_bytes()));
    let expires_at = Utc::now() + Duration::days(30);
    let family_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO auth_sessions (user_id, auth_identity_id, device_id, token_hash, family_id, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(user_id)
    .bind(identity_id)
    .bind(device_id)
    .bind(token_hash)
    .bind(family_id)
    .bind(expires_at)
    .execute(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(AuthExchangeResponse {
        token: raw_token,
        user_id,
        expires_at,
        has_phone,
    }))
}
