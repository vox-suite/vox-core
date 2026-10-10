use axum::{
    Extension, Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use base64::Engine as _;

use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use chrono::Utc;
use vox_core::domain::identity::Actor;
#[allow(unused_imports)]
pub use vox_core::gmail_sync::reconcile_all_gmail_connections;

#[derive(Deserialize)]
pub struct PubSubQuery {
    pub token: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct PubSubPushMessage {
    pub message: PubSubMessageData,
    pub subscription: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct PubSubMessageData {
    pub data: String,
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(rename = "publishTime")]
    pub publish_time: Option<String>,
}

#[derive(Deserialize)]
pub struct GmailNotificationData {
    #[serde(rename = "emailAddress")]
    pub email_address: String,
    #[serde(rename = "historyId")]
    pub history_id: Value,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct GoogleOidcClaims {
    iss: String,
    aud: String,
    exp: u64,
    email: Option<String>,
    #[serde(rename = "email_verified")]
    email_verified: Option<bool>,
}

async fn verify_pubsub_auth(
    headers: &HeaderMap,
    query_token: Option<&str>,
) -> Result<(), StatusCode> {
    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    let bearer_token = auth_header
        .strip_prefix("Bearer ")
        .or_else(|| auth_header.strip_prefix("bearer "))
        .map(str::trim);

    let expected_secret = std::env::var("GMAIL_PUBSUB_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let expected_service_account = std::env::var("GMAIL_PUBSUB_SERVICE_ACCOUNT")
        .or_else(|_| std::env::var("GOOGLE_PUBSUB_SERVICE_ACCOUNT"))
        .ok()
        .filter(|s| !s.trim().is_empty());
    let expected_audience = std::env::var("GMAIL_PUBSUB_AUDIENCE")
        .or_else(|_| std::env::var("GMAIL_PUBSUB_PUSH_URL"))
        .or_else(|_| std::env::var("VOX_CORE_API_URL"))
        .ok()
        .filter(|s| !s.trim().is_empty());

    let is_dev = std::env::var("VOX_DEV_STORAGE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || std::env::var("VOX_ENV")
            .map(|v| v.eq_ignore_ascii_case("development") || v.eq_ignore_ascii_case("test"))
            .unwrap_or(false);

    if let Some(token) = bearer_token {
        if is_dev
            && let Some(ref sec) = expected_secret
            && token == sec.trim()
        {
            return Ok(());
        }

        if let Ok((header, payload, signature)) = crate::identity_token::split_jwt(token) {
            let header_json: Value =
                serde_json::from_slice(&crate::identity_token::decode_part(header)?)
                    .map_err(|_| StatusCode::UNAUTHORIZED)?;
            if header_json.get("alg").and_then(Value::as_str) != Some("RS256") {
                return Err(StatusCode::UNAUTHORIZED);
            }
            let kid = header_json
                .get("kid")
                .and_then(|v| v.as_str())
                .ok_or(StatusCode::UNAUTHORIZED)?;

            let jwk = crate::identity_token::google_jwk(kid).await?;
            if !crate::identity_token::rsa_sha256_valid(
                &jwk,
                &format!("{header}.{payload}"),
                signature,
            )? {
                return Err(StatusCode::UNAUTHORIZED);
            }

            let claims_bytes = crate::identity_token::decode_part(payload)?;
            let claims: GoogleOidcClaims =
                serde_json::from_slice(&claims_bytes).map_err(|_| StatusCode::UNAUTHORIZED)?;

            if claims.iss != "https://accounts.google.com" && claims.iss != "accounts.google.com" {
                return Err(StatusCode::UNAUTHORIZED);
            }

            let now = Utc::now().timestamp() as u64;
            if claims.exp <= now || claims.email_verified != Some(true) {
                return Err(StatusCode::UNAUTHORIZED);
            }

            if let Some(ref expected_aud) = expected_audience {
                if claims.aud != expected_aud.trim() {
                    return Err(StatusCode::UNAUTHORIZED);
                }
            } else if !is_dev {
                return Err(StatusCode::UNAUTHORIZED);
            }

            if let Some(ref expected_sa) = expected_service_account {
                if claims.email.as_deref() != Some(expected_sa.trim()) {
                    return Err(StatusCode::UNAUTHORIZED);
                }
            } else if !is_dev {
                return Err(StatusCode::UNAUTHORIZED);
            }

            return Ok(());
        }
    }

    if is_dev
        && let Some(ref sec) = expected_secret
        && query_token == Some(sec.trim())
    {
        return Ok(());
    }

    Err(StatusCode::UNAUTHORIZED)
}

pub async fn pubsub_webhook(
    State(pool): State<PgPool>,
    Query(query): Query<PubSubQuery>,
    headers: HeaderMap,
    Json(payload): Json<PubSubPushMessage>,
) -> Result<impl IntoResponse, StatusCode> {
    verify_pubsub_auth(&headers, query.token.as_deref()).await?;

    let raw_data = base64::engine::general_purpose::STANDARD
        .decode(payload.message.data.as_bytes())
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let notification: GmailNotificationData =
        serde_json::from_slice(&raw_data).map_err(|_| StatusCode::BAD_REQUEST)?;

    let incoming_history_id = match notification.history_id {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => s.parse::<u64>().unwrap_or(0),
        _ => 0,
    };

    let conn_row = sqlx::query(
        "SELECT id, metadata \
         FROM vox_connections \
         WHERE connector_id = 'gmail' AND (account_id = $1 OR account_display_id = $1) AND authorization_state = 'authorized' AND sync_timeline=true",
    )
    .bind(&notification.email_address)
    .fetch_optional(&pool)
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let Some(conn) = conn_row else {
        return Ok(StatusCode::OK);
    };

    let connection_id: Uuid = conn.get("id");
    let metadata: Value = conn.get("metadata");

    let last_history_id = match metadata.get("last_history_id") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.parse::<u64>().unwrap_or(0),
        _ => 0,
    };

    if incoming_history_id > 0 && incoming_history_id <= last_history_id {
        return Ok(StatusCode::OK);
    }

    if incoming_history_id == 0 {
        return Err(StatusCode::BAD_REQUEST);
    }
    sqlx::query("UPDATE vox_connections SET metadata=jsonb_set(COALESCE(metadata,'{}'::jsonb),'{pending_history_id}',to_jsonb(GREATEST(COALESCE((metadata->>'pending_history_id')::numeric,0),$2::numeric)::text)),updated_at=now() WHERE id=$1 AND authorization_state='authorized' AND sync_timeline=true")
        .bind(connection_id).bind(incoming_history_id.to_string()).execute(&pool).await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    Ok(StatusCode::OK)
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoricalPageRequest {
    pub start_date: String,
    pub end_date: String,
    pub page_token: Option<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct HistoricalCandidate {
    pub message_id: String,
    pub subject: Option<String>,
    pub from: Option<String>,
    pub reason: String,
    pub uncertainty: bool,
    pub events: Vec<vox_core::document_extraction::ExtractedEvent>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct HistoricalPageResponse {
    pub candidates: Vec<HistoricalCandidate>,
    pub excluded: usize,
    pub next_page_token: Option<String>,
}

fn history_permit() -> Result<tokio::sync::SemaphorePermit<'static>, StatusCode> {
    static CAPACITY: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    CAPACITY
        .get_or_init(|| tokio::sync::Semaphore::new(2))
        .try_acquire()
        .map_err(|_| StatusCode::TOO_MANY_REQUESTS)
}

async fn gmail_access(pool: &PgPool, user_id: Uuid) -> Result<(Uuid, String), StatusCode> {
    let connection_id: Uuid = sqlx::query_scalar("SELECT id FROM vox_connections WHERE user_id=$1 AND connector_id='gmail' AND authorization_state='authorized' AND sync_timeline=true")
        .bind(user_id).fetch_optional(pool).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::CONFLICT)?;
    let key = std::env::var("VOX_CREDENTIAL_KEY").ok();
    let service = vox_connections::accounts::FreshConnectionsService::new(
        pool.clone(),
        key.as_deref(),
        std::sync::Arc::new(vox_core::gmail_sync::DummyIngestor),
        std::env::var("GOOGLE_CLIENT_ID").ok(),
        std::env::var("GOOGLE_CLIENT_SECRET").ok(),
        std::env::var("VOX_CORE_API_URL").ok(),
    )
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let tokens = service
        .get_gmail_tokens(user_id, connection_id)
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    Ok((connection_id, tokens.access_token))
}

#[utoipa::path(post,path="/v1/connectors/gmail/history/page",request_body=HistoricalPageRequest,responses((status=200,body=HistoricalPageResponse),(status=400,description="Invalid date range"),(status=409,description="Enable Gmail timeline sync"),(status=503,description="Server extraction unavailable")))]
pub async fn historical_page(
    State(pool): State<PgPool>,
    Extension(actor): Extension<Actor>,
    Json(payload): Json<HistoricalPageRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let _permit = history_permit()?;
    let start = chrono::NaiveDate::parse_from_str(&payload.start_date, "%Y-%m-%d")
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let end = chrono::NaiveDate::parse_from_str(&payload.end_date, "%Y-%m-%d")
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if end <= start
        || (end - start).num_days() > 366
        || payload
            .page_token
            .as_ref()
            .is_some_and(|token| token.len() > 2048)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (_, token) = gmail_access(&pool, actor.user_id).await?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let mut query = vec![
        (
            "q",
            format!(
                "after:{} before:{}",
                start.format("%Y/%m/%d"),
                end.format("%Y/%m/%d")
            ),
        ),
        ("maxResults", "3".into()),
    ];
    if let Some(page) = payload.page_token {
        query.push(("pageToken", page));
    }
    let page: Value = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
        .bearer_auth(&token)
        .query(&query)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?
        .error_for_status()
        .map_err(|_| StatusCode::BAD_GATEWAY)?
        .json()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let provider = vox_connections::providers::gmail::GmailClient::new();
    let mut candidates = Vec::new();
    let mut excluded = 0;
    let ids: Vec<String> = page["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|message| {
            message["id"]
                .as_str()
                .map(str::to_owned)
                .ok_or(StatusCode::BAD_GATEWAY)
        })
        .collect::<Result<_, _>>()?;
    let mut results = stream::iter(ids.into_iter().map(|id| {
        let provider = provider.clone();
        let token = token.clone();
        async move {
            let mail = provider
                .get_message(&token, &id)
                .await
                .map_err(|_| StatusCode::BAD_GATEWAY)?;
            let result = vox_core::document_extraction::extract(
                &vox_core::gmail_sync::extraction_source(&mail),
            )
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
            Ok::<_, StatusCode>((id, mail, result))
        }
    }))
    .buffered(3);
    while let Some(result) = results.next().await {
        let (id, mail, result) = result?;
        if !result.useful && !result.uncertainty {
            excluded += 1;
            continue;
        }
        candidates.push(HistoricalCandidate {
            message_id: id,
            subject: mail.subject,
            from: mail.from,
            reason: result.reason,
            uncertainty: result.uncertainty,
            events: result.events,
        });
    }
    Ok(Json(HistoricalPageResponse {
        candidates,
        excluded,
        next_page_token: page["nextPageToken"].as_str().map(str::to_owned),
    }))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoricalImportRequest {
    pub message_ids: Vec<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct HistoricalImportResponse {
    pub imported_records: usize,
}

#[utoipa::path(post,path="/v1/connectors/gmail/history/import",request_body=HistoricalImportRequest,responses((status=200,body=HistoricalImportResponse),(status=400,description="Invalid message IDs"),(status=409,description="Enable Gmail timeline sync"),(status=503,description="Import failed; safe to retry")))]
pub async fn historical_import(
    State(pool): State<PgPool>,
    Extension(actor): Extension<Actor>,
    Json(payload): Json<HistoricalImportRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let _permit = history_permit()?;
    if payload.message_ids.is_empty()
        || payload.message_ids.len() > 5
        || payload.message_ids.iter().any(|id| {
            id.is_empty() || id.len() > 256 || !id.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let (connection_id, token) = gmail_access(&pool, actor.user_id).await?;
    let provider = vox_connections::providers::gmail::GmailClient::new();
    let email = provider
        .get_profile(&token)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?
        .email_address;
    let sync_run: Uuid = sqlx::query_scalar("INSERT INTO connector_sync_runs(user_id,connector_id,connection_id,run_type,status,started_at) VALUES($1,'gmail',$2,'import','running',now()) RETURNING id")
        .bind(actor.user_id).bind(connection_id).fetch_one(&pool).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut imported = 0;
    for id in payload.message_ids {
        let current_token = match gmail_access(&pool, actor.user_id).await {
            Ok((_, token)) => token,
            Err(status) => {
                sqlx::query("UPDATE connector_sync_runs SET status='failed',error='connection_unavailable',records_ingested=$2,completed_at=now() WHERE id=$1")
                    .bind(sync_run).bind(imported as i32).execute(&pool).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
                return Err(status);
            }
        };
        match vox_core::gmail_sync::ingest_message(
            &pool,
            &provider,
            &current_token,
            actor.user_id,
            &email,
            &id,
            true,
        )
        .await
        {
            Ok(true) => imported += 1,
            Ok(false) => {}
            Err(_) => {
                sqlx::query("UPDATE connector_sync_runs SET status='failed',error='server_import_failed',records_ingested=$2,completed_at=now() WHERE id=$1")
                    .bind(sync_run).bind(imported as i32).execute(&pool).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }
    sqlx::query("UPDATE connector_sync_runs SET status='completed',records_ingested=$2,completed_at=now() WHERE id=$1")
        .bind(sync_run).bind(imported as i32).execute(&pool).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(HistoricalImportResponse {
        imported_records: imported,
    }))
}
