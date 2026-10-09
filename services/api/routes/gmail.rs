use axum::{
    Extension, Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[allow(unused_imports)]
pub use vox_core::gmail_sync::reconcile_all_gmail_connections;
use vox_core::{
    domain::identity::Actor,
    gmail_sync::process_gmail_delta,
    storage::object_storage::store_object,
};

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

async fn verify_pubsub_auth(headers: &HeaderMap, query_token: Option<&str>) -> Result<(), StatusCode> {
    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    let bearer_token = auth_header
        .strip_prefix("Bearer ")
        .or_else(|| auth_header.strip_prefix("bearer "))
        .map(str::trim);

    let expected_secret = std::env::var("GMAIL_PUBSUB_SECRET").ok().filter(|s| !s.trim().is_empty());
    let expected_service_account = std::env::var("GMAIL_PUBSUB_SERVICE_ACCOUNT")
        .or_else(|_| std::env::var("GOOGLE_PUBSUB_SERVICE_ACCOUNT"))
        .ok()
        .filter(|s| !s.trim().is_empty());
    let expected_audience = std::env::var("GMAIL_PUBSUB_AUDIENCE")
        .or_else(|_| std::env::var("GMAIL_PUBSUB_PUSH_URL"))
        .or_else(|_| std::env::var("VOX_CORE_API_URL"))
        .ok()
        .filter(|s| !s.trim().is_empty());

    let is_dev = std::env::var("VOX_DEV_STORAGE").map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false)
        || std::env::var("VOX_ENV").map(|v| v.eq_ignore_ascii_case("development") || v.eq_ignore_ascii_case("test")).unwrap_or(false);

    if let Some(token) = bearer_token {
        if is_dev {
            if let Some(ref sec) = expected_secret {
                if token == sec.trim() {
                    return Ok(());
                }
            }
        }

        if let Ok((header, payload, signature)) = crate::identity_token::split_jwt(token) {
            let header_json: Value = serde_json::from_slice(&crate::identity_token::decode_part(header)?)
                .map_err(|_| StatusCode::UNAUTHORIZED)?;
            if header_json.get("alg").and_then(Value::as_str) != Some("RS256") { return Err(StatusCode::UNAUTHORIZED); }
            let kid = header_json
                .get("kid")
                .and_then(|v| v.as_str())
                .ok_or(StatusCode::UNAUTHORIZED)?;

            let jwk = crate::identity_token::google_jwk(kid).await?;
            if !crate::identity_token::rsa_sha256_valid(&jwk, &format!("{header}.{payload}"), signature)? {
                return Err(StatusCode::UNAUTHORIZED);
            }

            let claims_bytes = crate::identity_token::decode_part(payload)?;
            let claims: GoogleOidcClaims = serde_json::from_slice(&claims_bytes)
                .map_err(|_| StatusCode::UNAUTHORIZED)?;

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

    if is_dev {
        if let Some(ref sec) = expected_secret {
            if query_token == Some(sec.trim()) {
                return Ok(());
            }
        }
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

    let notification: GmailNotificationData = serde_json::from_slice(&raw_data)
        .map_err(|_| StatusCode::BAD_REQUEST)?;

    let incoming_history_id = match notification.history_id {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => s.parse::<u64>().unwrap_or(0),
        _ => 0,
    };

    let conn_row = sqlx::query(
        "SELECT id, user_id, metadata, access_ciphertext, refresh_ciphertext, access_expires_at, last_synced_at \
         FROM vox_connections \
         WHERE connector_id = 'gmail' AND (account_id = $1 OR account_display_id = $1) AND authorization_state = 'authorized'",
    )
    .bind(&notification.email_address)
    .fetch_optional(&pool)
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let Some(conn) = conn_row else {
        return Ok(StatusCode::OK);
    };

    let connection_id: Uuid = conn.get("id");
    let user_id: Uuid = conn.get("user_id");
    let metadata: Value = conn.get("metadata");
    let last_synced_at: Option<DateTime<Utc>> = conn.get("last_synced_at");

    let last_history_id = match metadata.get("last_history_id") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.parse::<u64>().unwrap_or(0),
        _ => 0,
    };

    if incoming_history_id > 0 && incoming_history_id <= last_history_id {
        return Ok(StatusCode::OK);
    }

    if incoming_history_id == 0 { return Err(StatusCode::BAD_REQUEST); }
    if !process_gmail_delta(&pool, user_id, connection_id, &notification.email_address, last_history_id, incoming_history_id, last_synced_at).await {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }

    Ok(StatusCode::OK)
}



# [derive(Deserialize, utoipa::ToSchema)]
pub struct DeviceHistoricalImportRequest {
    pub messages: Vec<DeviceHistoricalImportItem>,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[allow(dead_code)]
pub struct DeviceHistoricalImportItem {
    pub message_id: String,
    pub internal_date: Option<i64>,
    pub from: Option<String>,
    pub subject: Option<String>,
    pub date: Option<String>,
    pub snippet: Option<String>,
    pub body_text: Option<String>,
    pub user_reviewed: bool,
    pub uncertainty: bool,
    pub proposed_event: Option<ProposedEventItem>,
    pub attachments: Option<Vec<DeviceHistoricalAttachmentItem>>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ProposedEventItem {
    pub event_type_value: String,
    pub group_value: String,
    pub title: String,
    pub summary: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub content: Value,
}

#[derive(Deserialize, utoipa::ToSchema)]
#[allow(dead_code)]
pub struct DeviceHistoricalAttachmentItem {
    pub filename: String,
    pub mime_type: String,
    pub content_base64: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct DeviceHistoricalImportResponse {
    pub imported_records: usize,
    pub events_created: usize,
    pub attachments_queued: usize,
    pub uncertain_count: usize,
}

#[utoipa::path(post,path="/v1/connectors/gmail/device-historical-import",operation_id="gmail_device_historical_import",request_body=DeviceHistoricalImportRequest,responses((status=200,body=DeviceHistoricalImportResponse),(status=400,description="Invalid or unreviewed import"),(status=409,description="Gmail connection required"),(status=422,description="Invalid timeline event")))]
pub async fn device_historical_import(
    State(pool): State<PgPool>,
    Extension(actor): Extension<Actor>,
    Json(payload): Json<DeviceHistoricalImportRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let user_id = actor.user_id;
    if payload.messages.is_empty() || payload.messages.len() > 25 {
        return Err(StatusCode::BAD_REQUEST);
    }
    for item in &payload.messages {
        if item.uncertainty || !item.user_reviewed || item.message_id.is_empty() || item.message_id.len() > 256
            || item.body_text.as_ref().is_some_and(|body| body.len() > 256 * 1024)
            || (item.proposed_event.is_none() && item.attachments.as_ref().is_none_or(Vec::is_empty)) {
            return Err(StatusCode::BAD_REQUEST);
        }
        if let Some(attachments) = &item.attachments {
            if attachments.len() > 10 { return Err(StatusCode::BAD_REQUEST); }
            for attachment in attachments {
                if attachment.mime_type != "application/pdf" || attachment.content_base64.len() > 35 * 1024 * 1024 {
                    return Err(StatusCode::BAD_REQUEST);
                }
            }
        }
    }
    let connection_id: Uuid = sqlx::query_scalar("SELECT id FROM vox_connections WHERE user_id=$1 AND connector_id='gmail' AND authorization_state='authorized'")
        .bind(user_id).fetch_optional(&pool).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::CONFLICT)?;
    let repo = vox_core::storage::timeline::TimelineRepository::new(pool.clone());
    let mut stored_objects = Vec::new();
    let outcome: Result<DeviceHistoricalImportResponse, StatusCode> = async {
        let mut tx = pool.begin().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let sync_run_id: Uuid = sqlx::query_scalar("INSERT INTO connector_sync_runs(user_id,connector_id,connection_id,run_type,status,started_at) VALUES($1,'gmail',$2,'import','running',now()) RETURNING id")
            .bind(user_id).bind(connection_id).fetch_one(&mut *tx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut imported_records = 0;
        let mut events_created = 0;
        let mut attachments_queued = 0;
        for item in payload.messages {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("gmail:{user_id}:{}",item.message_id)).execute(&mut *tx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let metadata = json!({"from":item.from,"subject":item.subject,"date":item.date,"snippet":item.snippet,"internal_date":item.internal_date,"user_reviewed":true,"historical_import":true});
            let source_record_id: Option<Uuid> = sqlx::query_scalar("INSERT INTO source_records(user_id,connector_id,source_record_id,record_hash,disposition,metadata) VALUES($1,'gmail',$2,$3,'retained',$4) ON CONFLICT(user_id,connector_id,source_record_id) DO NOTHING RETURNING id")
                .bind(user_id).bind(&item.message_id).bind(format!("{:x}",Sha256::digest(serde_json::to_vec(&metadata).map_err(|_| StatusCode::BAD_REQUEST)?)))
                .bind(&metadata).fetch_optional(&mut *tx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let Some(source_record_id) = source_record_id else { continue; };
            imported_records += 1;
            for attachment in item.attachments.unwrap_or_default() {
                let bytes = base64::engine::general_purpose::STANDARD.decode(attachment.content_base64.as_bytes()).map_err(|_| StatusCode::BAD_REQUEST)?;
                if bytes.len() > 25 * 1024 * 1024 || !bytes.starts_with(b"%PDF-") { return Err(StatusCode::BAD_REQUEST); }
                let object_ref = store_object(user_id,"attachments",&attachment.filename,&bytes).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                stored_objects.push(object_ref.clone());
                let attachment_id: Uuid = sqlx::query_scalar("INSERT INTO source_attachments(user_id,source_record_id,object_ref,content_hash,mime_type,size_bytes,parse_state,expires_at) VALUES($1,$2,$3,$4,'application/pdf',$5,'pending',now()+interval '7 days') RETURNING id")
                    .bind(user_id).bind(source_record_id).bind(object_ref).bind(format!("{:x}",Sha256::digest(&bytes))).bind(bytes.len() as i64)
                    .fetch_one(&mut *tx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                sqlx::query("INSERT INTO jobs(user_id,kind,payload_reference_id,state,max_attempts) VALUES($1,'process_attachment',$2,'pending',5)")
                    .bind(user_id).bind(attachment_id).execute(&mut *tx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                attachments_queued += 1;
            }
            if let Some(event) = item.proposed_event {
                let facts=event.content.clone();
                let financial=event.group_value=="finance";
                let result=repo.ingest_event_in_transaction(&mut tx,user_id,vox_core::domain::timeline::IngestTimelineEventInput {
                    event_type_id:None,event_type_value:Some(event.event_type_value),group_id:None,group_value:Some(event.group_value),
                    title:event.title,summary:event.summary,occurred_at:event.occurred_at,ended_at:None,time_precision:"day".into(),source_timezone:None,
                    content:event.content,confidence:0.8,dedupe_key:Some(format!("gmail:{}",item.message_id)),
                    evidence:vec![vox_core::domain::timeline::NewEvidenceItem {source_record_id:Some(source_record_id),source_attachment_id:None,
                        source_type:"gmail".into(),source_id:Some(item.message_id),raw_reference:None,observation_metadata:metadata}],
                }).await.map_err(|error| { tracing::warn!(%user_id,%error,"historical email rejected by timeline contract"); StatusCode::UNPROCESSABLE_ENTITY })?;
                if financial { vox_core::finance_normalization::dedupe_or_settle_in_transaction(&mut tx,user_id,result.event.id,&facts).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?; }
                events_created += 1;
            }
        }
        sqlx::query("UPDATE connector_sync_runs SET status='completed',records_extracted=$1,records_ingested=$2,completed_at=now() WHERE id=$3")
            .bind(imported_records as i32).bind(events_created as i32).bind(sync_run_id).execute(&mut *tx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        sqlx::query("UPDATE connector_coverage SET metadata=metadata || $1,updated_at=now() WHERE user_id=$2 AND connector_id='gmail'")
            .bind(json!({"historical_import":{"records":imported_records,"events":events_created,"coverage":"selected_messages_only","completed_at":Utc::now()}}))
            .bind(user_id).execute(&mut *tx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        tx.commit().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(DeviceHistoricalImportResponse {imported_records,events_created,attachments_queued,uncertain_count:0})
    }.await;
    if outcome.is_err() {
        for object in stored_objects {
            if let Err(error) = vox_core::storage::object_storage::delete_object(&object).await {
                tracing::error!(%user_id,%error,"historical import rollback object cleanup failed");
            }
        }
    }
    Ok((StatusCode::OK,Json(outcome?)))
}

#[derive(Serialize,utoipa::ToSchema)]
pub struct DeviceAccessResponse { pub access_token:String,pub expires_in:i64 }

#[utoipa::path(post,path="/v1/connectors/gmail/device-access",responses((status=200,body=DeviceAccessResponse),(status=409,description="Gmail connection required")))]
pub async fn device_access(
    State(pool): State<PgPool>,
    Extension(actor): Extension<Actor>,
) -> Result<impl IntoResponse, StatusCode> {
    let connection_id: Uuid = sqlx::query_scalar("SELECT id FROM vox_connections WHERE user_id=$1 AND connector_id='gmail' AND authorization_state='authorized'")
        .bind(actor.user_id).fetch_optional(&pool).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.ok_or(StatusCode::CONFLICT)?;
    let key = std::env::var("VOX_CREDENTIAL_KEY").ok();
    let service = vox_connections::accounts::FreshConnectionsService::new(pool,key.as_deref(),std::sync::Arc::new(vox_core::gmail_sync::DummyIngestor),
        std::env::var("GOOGLE_CLIENT_ID").ok(),std::env::var("GOOGLE_CLIENT_SECRET").ok(),std::env::var("VOX_CORE_API_URL").ok())
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let tokens = service.get_gmail_tokens(actor.user_id,connection_id).await.map_err(|_| StatusCode::UNAUTHORIZED)?;
    if tokens.expires_in < 30 { return Err(StatusCode::UNAUTHORIZED); }
    Ok(([ (axum::http::header::CACHE_CONTROL,"no-store"), (axum::http::header::PRAGMA,"no-cache") ],
        Json(DeviceAccessResponse {access_token:tokens.access_token,expires_in:tokens.expires_in})))
}
