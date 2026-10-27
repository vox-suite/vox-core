use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::storage::object_storage::store_object;

pub struct DummyIngestor;

#[async_trait::async_trait]
impl vox_connections::accounts::TimelineIngestor for DummyIngestor {
    async fn calendar(
        &self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _user_id: Uuid,
        _connection_id: Uuid,
        _account_id: &str,
        _events: &[vox_connections::providers::google_calendar::GoogleCalendarEvent],
        _time_min: DateTime<Utc>,
        _time_max: DateTime<Utc>,
    ) -> Result<usize, vox_connections::accounts::FreshConnectionError> {
        Ok(0)
    }

    async fn gaming(
        &self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _user_id: Uuid,
        _connection_id: Uuid,
        _activities: &[vox_connections::providers::observations::ObservedActivity],
    ) -> Result<usize, vox_connections::accounts::FreshConnectionError> {
        Ok(0)
    }

    async fn game_history(
        &self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _user_id: Uuid,
        _connection_id: Uuid,
        _games: &[vox_connections::providers::playstation::PlayStationGame],
    ) -> Result<usize, vox_connections::accounts::FreshConnectionError> {
        Ok(0)
    }

    async fn food_order(
        &self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _user_id: Uuid,
        _connection_id: Uuid,
        _orders: &[vox_connections::providers::food_delivery::FoodDeliveryOrder],
    ) -> Result<usize, vox_connections::accounts::FreshConnectionError> {
        Ok(0)
    }

    async fn personal_activity(
        &self,
        _tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _user_id: Uuid,
        _connection_id: Uuid,
        _connector: &str,
        _items: &[vox_connections::providers::personal::PersonalActivity],
    ) -> Result<usize, vox_connections::accounts::FreshConnectionError> {
        Ok(0)
    }
}

pub async fn process_gmail_delta(
    pool: &PgPool,
    user_id: Uuid,
    connection_id: Uuid,
    email_address: &str,
    last_history_id: u64,
    incoming_history_id: u64,
    last_synced_at: Option<DateTime<Utc>>,
) -> bool {
    match tokio::time::timeout(
        std::time::Duration::from_secs(120),
        process_gmail_delta_inner(
            pool,
            user_id,
            connection_id,
            email_address,
            last_history_id,
            incoming_history_id,
            last_synced_at,
        ),
    )
    .await
    {
        Ok(completed) => completed,
        Err(_) => {
            tracing::warn!(%connection_id, "Gmail sync exceeded its connection time budget; cursor retained");
            false
        }
    }
}

async fn process_gmail_delta_inner(
    pool: &PgPool,
    user_id: Uuid,
    connection_id: Uuid,
    email_address: &str,
    last_history_id: u64,
    incoming_history_id: u64,
    last_synced_at: Option<DateTime<Utc>>,
) -> bool {
    let Ok(mut lease) = pool.begin().await else {
        return false;
    };
    let locked =
        sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("gmail-connection:{connection_id}"))
            .fetch_one(&mut *lease)
            .await;
    if !matches!(locked, Ok(true)) {
        return false;
    }
    let generation: Option<(Uuid,Uuid)> = match sqlx::query_as("SELECT credential_generation,generation FROM vox_connections WHERE id=$1 AND user_id=$2 AND authorization_state='authorized' AND sync_timeline=true AND (account_id=$3 OR account_display_id=$3)")
        .bind(connection_id).bind(user_id).bind(email_address).fetch_optional(pool).await {
            Ok(value) => value,
            Err(_) => return false,
        };
    let Some((credential_generation, authorization_generation)) = generation else {
        return true;
    };
    let sync_run_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO connector_sync_runs (user_id, connector_id, connection_id, run_type, status, cursor_state, started_at) \
         VALUES ($1, 'gmail', $2, 'live', 'running', $3, now()) RETURNING id",
    )
    .bind(user_id)
    .bind(connection_id)
    .bind(json!({"start_history_id": last_history_id, "target_history_id": incoming_history_id}))
    .fetch_one(pool)
    .await
    .unwrap_or(Uuid::nil());
    if sync_run_id.is_nil() {
        return false;
    }

    let connections_svc = vox_connections::accounts::FreshConnectionsService::new(
        pool.clone(),
        std::env::var("VOX_CREDENTIAL_KEY").ok().as_deref(),
        std::sync::Arc::new(DummyIngestor),
        std::env::var("GOOGLE_CLIENT_ID").ok(),
        std::env::var("GOOGLE_CLIENT_SECRET").ok(),
        std::env::var("VOX_CORE_API_URL").ok(),
    );

    let Ok(connections_service) = connections_svc else {
        let _ = sqlx::query("UPDATE connector_sync_runs SET status='failed',error='credential_configuration_failed',completed_at=now() WHERE id=$1").bind(sync_run_id).execute(pool).await;
        return false;
    };

    let Ok(tokens) = connections_service
        .get_gmail_tokens(user_id, connection_id)
        .await
    else {
        let _ = sqlx::query("UPDATE connector_sync_runs SET status = 'failed', error = 'token_refresh_failed', completed_at = now() WHERE id = $1")
            .bind(sync_run_id)
            .execute(pool)
            .await;
        return false;
    };

    let gmail_client = vox_connections::providers::gmail::GmailClient::new();
    let history_res = gmail_client
        .list_history(&tokens.access_token, last_history_id)
        .await;

    let (message_ids, new_cursor) = match history_res {
        Ok(res) => res,
        Err(vox_connections::providers::gmail::GmailError::Invalid(_)) => {
            if let Ok(profile) = gmail_client.get_profile(&tokens.access_token).await {
                let conn_meta: Option<(Option<DateTime<Utc>>, Option<Value>)> = sqlx::query_as(
                    "SELECT created_at, metadata FROM vox_connections WHERE id = $1",
                )
                .bind(connection_id)
                .fetch_optional(pool)
                .await
                .unwrap_or(None);

                let baseline_ts = conn_meta
                    .as_ref()
                    .and_then(|(created, meta)| {
                        meta.as_ref()
                            .and_then(|m| m.get("baseline_at").and_then(Value::as_str))
                            .and_then(|s| {
                                DateTime::parse_from_rfc3339(s)
                                    .ok()
                                    .map(|dt| dt.with_timezone(&Utc).timestamp())
                            })
                            .or_else(|| created.map(|c| c.timestamp()))
                    })
                    .unwrap_or_else(|| Utc::now().timestamp());

                let since_ts = match last_synced_at {
                    Some(d) => d.timestamp().max(baseline_ts),
                    None => baseline_ts,
                };

                let recovered_ids = match gmail_client
                    .list_messages_after(&tokens.access_token, since_ts)
                    .await
                {
                    Ok(ids) => ids,
                    Err(error) => {
                        tracing::warn!(%connection_id, %error, "gmail history recovery failed; cursor retained");
                        let _ = sqlx::query("UPDATE connector_sync_runs SET status='failed',error='history_recovery_failed',completed_at=now() WHERE id=$1").bind(sync_run_id).execute(pool).await;
                        return false;
                    }
                };
                (recovered_ids, profile.history_id)
            } else {
                let _ = sqlx::query("UPDATE connector_sync_runs SET status = 'failed', error = 'history_expired_profile_failed', completed_at = now() WHERE id = $1")
                    .bind(sync_run_id)
                    .execute(pool)
                    .await;
                return false;
            }
        }
        Err(_) => {
            let _ = sqlx::query("UPDATE connector_sync_runs SET status = 'failed', error = 'history_list_failed', completed_at = now() WHERE id = $1")
                .bind(sync_run_id)
                .execute(pool)
                .await;
            return false;
        }
    };

    let mut records_extracted = 0;
    let mut records_ingested = 0;

    for msg_id in &message_ids {
        records_extracted += 1;
        match ingest_message(
            pool,
            &gmail_client,
            &tokens.access_token,
            user_id,
            email_address,
            msg_id,
            false,
        )
        .await
        {
            Ok(ingested) => records_ingested += i32::from(ingested),
            Err(error) => {
                tracing::warn!(%user_id, %connection_id, message_id = %msg_id, error = %error, "gmail ingestion failed; message skipped");
            }
        }
    }

    let meta_update = json!({
        "last_history_id": new_cursor,
        "last_synced_at": Utc::now(),
    });
    let finalized: Result<(), sqlx::Error> = async {
        let mut tx = pool.begin().await?;
        finalize_cursor(&mut tx, user_id, connection_id, email_address,
            credential_generation, authorization_generation, new_cursor, meta_update).await?;
        sqlx::query("UPDATE connector_sync_runs SET status='completed',records_extracted=$1,records_ingested=$2,cursor_state=$3,completed_at=now() WHERE id=$4")
            .bind(records_extracted).bind(records_ingested).bind(json!({"history_id":new_cursor})).bind(sync_run_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE connector_coverage SET coverage_end=now(),is_healthy=true,last_checked_at=now(),updated_at=now() WHERE user_id=$1 AND connector_id='gmail'")
            .bind(user_id).execute(&mut *tx).await?;
        tx.commit().await
    }.await;
    if let Err(error) = finalized {
        tracing::error!(%connection_id,%error,"gmail cursor finalization failed");
        return false;
    }
    true
}

#[allow(clippy::too_many_arguments)]
async fn finalize_cursor(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    connection_id: Uuid,
    email_address: &str,
    credential_generation: Uuid,
    authorization_generation: Uuid,
    new_cursor: u64,
    meta_update: Value,
) -> Result<(), sqlx::Error> {
    let finalized = sqlx::query("UPDATE vox_connections SET metadata=metadata || $1,last_synced_at=now(),updated_at=now() WHERE id=$2 AND user_id=$3 AND COALESCE((metadata->>'last_history_id')::numeric,0) <= $4::numeric AND credential_generation=$5 AND generation=$6 AND authorization_state='authorized' AND sync_timeline=true AND (account_id=$7 OR account_display_id=$7)")
                .bind(meta_update).bind(connection_id).bind(user_id).bind(new_cursor.to_string())
                .bind(credential_generation).bind(authorization_generation).bind(email_address)
                .execute(&mut **tx).await?;
    if finalized.rows_affected() != 1 {
        return Err(sqlx::Error::Protocol(
            "Gmail connection changed before cursor finalization".into(),
        ));
    }
    Ok(())
}

pub async fn reconcile_all_gmail_connections(pool: &PgPool) {
    let rows = sqlx::query(
        "SELECT c.id, c.user_id, c.metadata, c.account_id, c.account_display_id, c.last_synced_at \
         FROM vox_connections c \
         WHERE c.connector_id = 'gmail' AND c.authorization_state = 'authorized' AND c.sync_timeline = true",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for row in rows {
        let conn_id: Uuid = row.get("id");
        let u_id: Uuid = row.get("user_id");
        let metadata: Value = row.get("metadata");
        let email: String = row
            .get::<Option<String>, _>("account_id")
            .or_else(|| row.get::<Option<String>, _>("account_display_id"))
            .unwrap_or_default();
        let last_synced: Option<DateTime<Utc>> = row.get("last_synced_at");

        let last_hist_id = match metadata.get("last_history_id") {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(Value::String(s)) => s.parse::<u64>().unwrap_or(0),
            _ => 0,
        };

        let watch_exp = match metadata.get("watch_expiration") {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
            Some(Value::String(s)) => s.parse::<i64>().unwrap_or(0),
            _ => 0,
        };

        let now_ms = Utc::now().timestamp_millis();
        let needs_watch_renewal = watch_exp == 0 || (watch_exp - now_ms) < (24 * 3600 * 1000);

        if needs_watch_renewal {
            let connections_svc = vox_connections::accounts::FreshConnectionsService::new(
                pool.clone(),
                std::env::var("VOX_CREDENTIAL_KEY").ok().as_deref(),
                std::sync::Arc::new(DummyIngestor),
                std::env::var("GOOGLE_CLIENT_ID").ok(),
                std::env::var("GOOGLE_CLIENT_SECRET").ok(),
                std::env::var("VOX_CORE_API_URL").ok(),
            );
            if let Ok(svc) = connections_svc {
                match svc.renew_gmail_watch(u_id,conn_id).await {
                    Ok(()) => {
                        if let Err(error) = sqlx::query("UPDATE updates SET status='resolved',resolved_at=now(),updated_at=now() WHERE user_id=$1 AND dedupe_key=$2 AND status<>'resolved'")
                            .bind(u_id).bind(format!("gmail-watch:{conn_id}")).execute(pool).await {
                            tracing::warn!(%conn_id,%error,"Could not resolve Gmail watch status update");
                        }
                    }
                    Err(_) => {
                        tracing::warn!(%conn_id,"Gmail live watch renewal failed; reconciliation remains enabled");
                        if let Err(error) = sqlx::query("INSERT INTO updates(user_id,kind,category,title,summary,content,dedupe_key) VALUES($1,'connection_status','gmail','Live Gmail delivery needs attention','New mail reconciliation continues while signed push delivery is unavailable.',jsonb_build_object('connection_id',$2::text,'delivery_state','reconciliation_only'),$3) ON CONFLICT(user_id,dedupe_key) WHERE dedupe_key IS NOT NULL DO UPDATE SET status='active',resolved_at=NULL,updated_at=now()")
                            .bind(u_id).bind(conn_id).bind(format!("gmail-watch:{conn_id}")).execute(pool).await {
                            tracing::warn!(%conn_id,%error,"Could not record Gmail watch status update");
                        }
                    }
                }
            }
        }

        process_gmail_delta(pool, u_id, conn_id, &email, last_hist_id, 0, last_synced).await;
    }
}

pub async fn ingest_message(
    pool: &PgPool,
    gmail: &vox_connections::providers::gmail::GmailClient,
    token: &str,
    user_id: Uuid,
    email: &str,
    message_id: &str,
    historical: bool,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM source_records WHERE user_id=$1 AND connector_id='gmail' AND source_record_id=$2)")
        .bind(user_id).bind(message_id).fetch_one(pool).await?;
    if exists {
        return Ok(false);
    }
    let generation: Option<(Uuid,Uuid)> = sqlx::query_as("SELECT credential_generation,generation FROM vox_connections WHERE user_id=$1 AND connector_id='gmail' AND authorization_state='authorized' AND sync_timeline=true AND (account_id=$2 OR account_display_id=$2)")
        .bind(user_id).bind(email).fetch_optional(pool).await?;
    let generation = generation
        .ok_or_else(|| std::io::Error::other("Gmail timeline sync is paused or disconnected"))?;
    let msg = match gmail.get_message(token, message_id).await {
        Ok(message) => message,
        Err(vox_connections::providers::gmail::GmailError::NotFound) => {
            // A message can be deleted after it entered history. Keep its identity so
            // history retries do not repeatedly fetch a permanently missing message.
            sqlx::query("INSERT INTO source_records(user_id,connector_id,source_account_id,source_record_id,record_hash,disposition,metadata) SELECT $1,'gmail',$2,$3,$4,'retained','{\"excluded\":true,\"reason\":\"provider_deleted\"}'::jsonb FROM vox_connections WHERE user_id=$1 AND connector_id='gmail' AND credential_generation=$5 AND generation=$6 AND authorization_state='authorized' AND sync_timeline=true AND (account_id=$2 OR account_display_id=$2) ON CONFLICT(user_id,connector_id,source_record_id) DO NOTHING")
                .bind(user_id).bind(email).bind(message_id).bind(format!("{:x}",Sha256::digest(message_id.as_bytes())))
                .bind(generation.0).bind(generation.1).execute(pool).await?;
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    };
    let extraction = if worth_extracting(&msg).await {
        crate::usage::scope(
            user_id,
            crate::document_extraction::extract(&extraction_source(&msg)),
        )
        .await?
    } else {
        crate::document_extraction::Extraction {
            useful: false,
            uncertainty: false,
            reason: "Filtered before extraction".into(),
            events: Vec::new(),
        }
    };
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("gmail:{user_id}:{message_id}"))
        .execute(&mut *tx)
        .await?;
    let current: Option<(Uuid,Uuid)> = sqlx::query_as("SELECT credential_generation,generation FROM vox_connections WHERE user_id=$1 AND connector_id='gmail' AND authorization_state='authorized' AND sync_timeline=true AND (account_id=$2 OR account_display_id=$2) FOR SHARE")
        .bind(user_id).bind(email).fetch_optional(&mut *tx).await?;
    if current != Some(generation) {
        return Err(std::io::Error::other("Gmail authorization changed during extraction").into());
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM source_records WHERE user_id=$1 AND connector_id='gmail' AND source_record_id=$2)")
        .bind(user_id).bind(message_id).fetch_one(&mut *tx).await?;
    if exists {
        return Ok(false);
    }
    let safe_subject = msg.subject.as_deref().map(redact_email_codes);
    let safe_snippet = msg.snippet.as_deref().map(redact_email_codes);
    let metadata = json!({"from":msg.from,"to":msg.to,"subject":safe_subject,"date":msg.date,
        "snippet":safe_snippet,"internal_date":msg.internal_date,"attachment_count":msg.attachments.len(),"historical_import":historical});
    let metadata = if extraction.useful || extraction.uncertainty {
        metadata
    } else {
        json!({"excluded":true})
    };
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&metadata)?));
    let record_id: Uuid = sqlx::query_scalar("INSERT INTO source_records(user_id,connector_id,source_account_id,source_record_id,record_hash,disposition,metadata) VALUES($1,'gmail',$2,$3,$4,'retained',$5) RETURNING id")
        .bind(user_id).bind(email).bind(message_id).bind(hash).bind(&metadata).fetch_one(&mut *tx).await?;
    let mut stored_objects = Vec::new();
    let outcome: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
        for attachment in msg.attachments.iter().filter(|_| extraction.useful) {
            if attachment.mime_type != "application/pdf" && !attachment.filename.to_ascii_lowercase().ends_with(".pdf") { continue; }
            if attachment.size_bytes > 25 * 1024 * 1024 { return Err(std::io::Error::other("attachment exceeds 25 MiB limit").into()); }
            let bytes = gmail.get_attachment(token, message_id, &attachment.attachment_id).await?;
            if bytes.len() > 25 * 1024 * 1024 { return Err(std::io::Error::other("attachment exceeds 25 MiB limit").into()); }
            let object = store_object(user_id,"attachments",&attachment.filename,&bytes).await?;
            stored_objects.push(object.clone());
            let id: Uuid = sqlx::query_scalar("INSERT INTO source_attachments(user_id,source_record_id,object_ref,content_hash,mime_type,size_bytes,parse_state,expires_at) VALUES($1,$2,$3,$4,'application/pdf',$5,'pending',now()+interval '7 days') RETURNING id")
                .bind(user_id).bind(record_id).bind(object).bind(format!("{:x}",Sha256::digest(&bytes))).bind(bytes.len() as i64).fetch_one(&mut *tx).await?;
            sqlx::query("INSERT INTO jobs(user_id,kind,payload_reference_id,state,max_attempts) VALUES($1,'process_attachment',$2,'pending',5)")
                .bind(user_id).bind(id).execute(&mut *tx).await?;
        }
        for (index, event) in extraction.events.iter().enumerate() {
            let result = crate::storage::timeline::TimelineRepository::new(pool.clone()).ingest_event_in_transaction(&mut tx,user_id,
                crate::domain::timeline::IngestTimelineEventInput {event_type_id:None,event_type_value:Some(event.event_type_value.clone()),group_id:None,group_value:Some(event.group_value.clone()),
                    title:event.title.clone(),summary:event.summary.clone(),occurred_at:event.occurred_at,ended_at:None,time_precision:"day".into(),source_timezone:None,
                    content:event.content.clone(),confidence:0.8,dedupe_key:Some(format!("gmail:{message_id}:{index}")),
                    evidence:vec![crate::domain::timeline::NewEvidenceItem {source_record_id:Some(record_id),source_attachment_id:None,
                        source_type:"gmail".into(),source_id:Some(message_id.into()),raw_reference:None,observation_metadata:metadata.clone()}],
                }).await?;
            if event.group_value == "finance" {
                crate::finance_normalization::dedupe_or_settle_in_transaction(&mut tx,user_id,result.event.id,&event.content).await?;
            }
        }
        if extraction.uncertainty || (extraction.useful && extraction.events.is_empty() && msg.attachments.is_empty()) {
            sqlx::query("INSERT INTO updates(user_id,kind,content_version,category,title,summary,content,priority,status,dedupe_key) VALUES($1,'email_notice',1,'email',$2,$3,$4,'low','active',$5)")
                .bind(user_id).bind(safe_subject.as_deref().unwrap_or("Email needs review"))
                .bind(&extraction.reason).bind(&metadata).bind(format!("email_notice:{message_id}")).execute(&mut *tx).await?;
        }
        Ok(())
    }.await;
    if let Err(error) = outcome {
        tx.rollback().await?;
        for object in stored_objects {
            if let Err(error) = crate::storage::object_storage::delete_object(&object).await {
                tracing::warn!(%error,"gmail rollback object cleanup failed");
            }
        }
        return Err(error);
    }
    tx.commit().await?;
    Ok(true)
}

async fn worth_extracting(msg: &vox_connections::providers::gmail::GmailMessageItem) -> bool {
    let Some(key) = std::env::var("JEV_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
    else {
        return true;
    };
    static HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let attachments: Vec<&str> = msg
        .attachments
        .iter()
        .map(|a| a.filename.as_str())
        .collect();
    let state = format!(
        "Sender: {}\nSubject: {}\nSnippet: {}\nAttachments: {}",
        msg.from.as_deref().unwrap_or(""),
        msg.subject
            .as_deref()
            .map(redact_email_codes)
            .unwrap_or_default(),
        msg.snippet
            .as_deref()
            .map(redact_email_codes)
            .unwrap_or_default(),
        attachments.join(", ")
    );
    let body = json!({
        "state": state,
        "model": "jev-latest",
        "questions": {"relevant": {
            "type": "noul",
            "instructions": "Could this email record something that happened in the recipient's own life: a payment, bill, refund, order, delivery, booking, trip, appointment, subscription or account statement?",
            "criteria": {
                "true": "A receipt, invoice, bill, statement, order or delivery update, booking or ticket, appointment, or a document about the recipient's money or plans.",
                "false": "Newsletters, promotions, marketing, social notifications, OTP or login codes, generic alerts, or personal conversation."
            }
        }}
    });
    let response = HTTP
        .get_or_init(reqwest::Client::new)
        .post("https://api.typesafe.ai/v1/systemone")
        .bearer_auth(key)
        .timeout(std::time::Duration::from_secs(5))
        .json(&body)
        .send()
        .await;
    let probability = match response {
        Ok(response) if response.status().is_success() => response
            .json::<Value>()
            .await
            .ok()
            .and_then(|parsed| parsed["answers"]["relevant"]["noul"].as_f64()),
        _ => None,
    };
    probability.is_none_or(|probability| probability >= 0.2)
}

pub fn redact_email_codes(text: &str) -> String {
    static FORWARD: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static REVERSE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let forward = FORWARD.get_or_init(||regex::Regex::new(r"(?i)((?:\botp\b|one[- ]time password|verification code|security code|login code)\s*(?:is\s*)?[:=-]?\s*)([0-9]{4,8})\b").unwrap());
    let reverse = REVERSE.get_or_init(||regex::Regex::new(r"(?i)\b([0-9]{4,8})(\s+(?:is\s+)?(?:your\s+)?(?:otp\b|one[- ]time password|verification code|security code|login code))").unwrap());
    let redacted = forward.replace_all(text, "${1}[redacted]");
    reverse
        .replace_all(&redacted, "[redacted]${2}")
        .into_owned()
}

pub fn extraction_source(message: &vox_connections::providers::gmail::GmailMessageItem) -> String {
    format!(
        "Sender: {}\nSubject: {}\nAttachments: {}\nBody:\n{}",
        message.from.as_deref().unwrap_or(""),
        message
            .subject
            .as_deref()
            .map(redact_email_codes)
            .unwrap_or_default(),
        serde_json::to_string(&message.attachments).unwrap_or_default(),
        message
            .body_text
            .as_deref()
            .map(redact_email_codes)
            .unwrap_or_default()
    )
}

pub async fn process_gmail_backfills(pool: &PgPool) {
    let rows = sqlx::query("SELECT id,user_id,COALESCE(account_id,account_display_id) AS email,metadata->'backfill' AS backfill FROM vox_connections WHERE connector_id='gmail' AND authorization_state='authorized' AND sync_timeline=true AND metadata->'backfill'->>'state'='running' LIMIT 5")
        .fetch_all(pool)
        .await;
    let Ok(rows) = rows else {
        tracing::warn!("Cannot read pending Gmail backfills");
        return;
    };
    for row in rows {
        let connection_id: Uuid = row.get("id");
        let budget = std::time::Duration::from_secs(180);
        if tokio::time::timeout(
            budget,
            backfill_page(
                pool,
                connection_id,
                row.get("user_id"),
                row.get("email"),
                row.get("backfill"),
            ),
        )
        .await
        .is_err()
        {
            tracing::warn!(%connection_id, "Gmail backfill page exceeded its time budget");
        }
    }
}

async fn backfill_page(
    pool: &PgPool,
    connection_id: Uuid,
    user_id: Uuid,
    email: Option<String>,
    _backfill: Value,
) {
    let Some(email) = email else { return };
    let Ok(mut lease) = pool.begin().await else {
        return;
    };
    let locked =
        sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("gmail-backfill:{connection_id}"))
            .fetch_one(&mut *lease)
            .await;
    if !matches!(locked, Ok(true)) {
        return;
    }
    // Read the cursor after acquiring the lease: another worker may have advanced it.
    let Ok(Some(backfill)) = sqlx::query_scalar::<_, Value>(
        "SELECT metadata->'backfill' FROM vox_connections WHERE id=$1 AND authorization_state='authorized' AND sync_timeline=true AND metadata->'backfill'->>'state'='running'"
    ).bind(connection_id).fetch_optional(&mut *lease).await else { return; };
    let Ok(service) = vox_connections::accounts::FreshConnectionsService::new(
        pool.clone(),
        std::env::var("VOX_CREDENTIAL_KEY").ok().as_deref(),
        std::sync::Arc::new(DummyIngestor),
        std::env::var("GOOGLE_CLIENT_ID").ok(),
        std::env::var("GOOGLE_CLIENT_SECRET").ok(),
        std::env::var("VOX_CORE_API_URL").ok(),
    ) else {
        return;
    };
    let Ok(tokens) = service.get_gmail_tokens(user_id, connection_id).await else {
        tracing::warn!(%connection_id, "Gmail backfill token refresh failed");
        return;
    };
    let query = format!(
        "after:{} before:{}",
        backfill["after"].as_i64().unwrap_or_default(),
        backfill["before"].as_i64().unwrap_or_default()
    );
    let gmail = vox_connections::providers::gmail::GmailClient::new();
    let (ids, next) = match list_page(
        &tokens.access_token,
        &query,
        backfill["page_token"].as_str(),
    )
    .await
    {
        Ok(page) => page,
        Err(error) => {
            tracing::warn!(%connection_id, %error, "Gmail backfill page listing failed");
            return;
        }
    };
    let offset = backfill["page_offset"].as_u64().unwrap_or_default() as usize;
    let (mut imported, mut skipped) = (0, 0);
    let mut failed = false;
    for id in ids.iter().skip(offset) {
        match ingest_message(
            pool,
            &gmail,
            &tokens.access_token,
            user_id,
            &email,
            id,
            true,
        )
        .await
        {
            Ok(true) => imported += 1,
            Ok(false) => skipped += 1,
            Err(error) => {
                tracing::warn!(%connection_id, message_id = %id, %error, "Gmail backfill message failed; retrying page");
                failed = true;
                break;
            }
        }
    }
    let count = |key: &str| backfill[key].as_i64().unwrap_or_default();
    let mut update = backfill.clone();
    if !failed {
        update["page_token"] = json!(next);
    }
    update["page_offset"] = json!(if failed {
        offset + (imported + skipped) as usize
    } else {
        0
    });
    update["processed"] = json!(count("processed") + imported + skipped);
    update["imported"] = json!(count("imported") + imported);
    update["skipped"] = json!(count("skipped") + skipped);
    if !failed && next.is_none() {
        update["state"] = json!("done");
        update["completed_at"] = json!(Utc::now());
    }
    if let Err(error) = sqlx::query("UPDATE vox_connections SET metadata=metadata || jsonb_build_object('backfill',$2::jsonb) WHERE id=$1 AND metadata->'backfill'->>'started_at'=$3")
        .bind(connection_id)
        .bind(update)
        .bind(backfill["started_at"].as_str())
        .execute(pool)
        .await
    {
        tracing::warn!(%connection_id, %error, "Gmail backfill progress update failed");
    }
}

async fn list_page(
    token: &str,
    query: &str,
    page_token: Option<&str>,
) -> Result<(Vec<String>, Option<String>), reqwest::Error> {
    static HTTP: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let mut params = vec![("q", query), ("maxResults", "25")];
    if let Some(page_token) = page_token {
        params.push(("pageToken", page_token));
    }
    let page: Value = HTTP
        .get_or_init(reqwest::Client::new)
        .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
        .bearer_auth(token)
        .query(&params)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let ids = page["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["id"].as_str().map(str::to_owned))
        .collect();
    Ok((ids, page["nextPageToken"].as_str().map(str::to_owned)))
}

pub async fn process_pending_gmail(pool: &PgPool) {
    let rows = sqlx::query("SELECT id,user_id,account_id,account_display_id,metadata,last_synced_at FROM vox_connections WHERE connector_id='gmail' AND authorization_state='authorized' AND sync_timeline=true AND COALESCE((metadata->>'pending_history_id')::numeric,0)>COALESCE((metadata->>'last_history_id')::numeric,0) AND (metadata->>'gmail_retry_after' IS NULL OR (metadata->>'gmail_retry_after')::timestamptz<=now()) LIMIT 20")
        .fetch_all(pool).await;
    let Ok(rows) = rows else {
        tracing::warn!("Cannot read pending Gmail work");
        return;
    };
    for row in rows {
        let metadata: Value = row.get("metadata");
        let cursor = |key: &str| {
            metadata[key]
                .as_u64()
                .or_else(|| metadata[key].as_str().and_then(|value| value.parse().ok()))
                .unwrap_or(0)
        };
        let email: Option<String> = row
            .get::<Option<String>, _>("account_id")
            .or_else(|| row.get("account_display_id"));
        if let Some(email) = email {
            let completed = process_gmail_delta(
                pool,
                row.get("user_id"),
                row.get("id"),
                &email,
                cursor("last_history_id"),
                cursor("pending_history_id"),
                row.get("last_synced_at"),
            )
            .await;
            let retry_after = if completed {
                Utc::now()
            } else {
                Utc::now() + chrono::Duration::minutes(5)
            };
            if sqlx::query("UPDATE vox_connections SET metadata=metadata || $2 WHERE id=$1")
                .bind(row.get::<Uuid, _>("id"))
                .bind(json!({"gmail_retry_after":retry_after}))
                .execute(pool)
                .await
                .is_err()
            {
                tracing::warn!("Cannot update Gmail retry backoff");
            }
        }
    }
}

#[cfg(test)]
mod cursor_regression_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
    async fn stale_gmail_generation_cannot_finalize_cursor() {
        let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let (connection, credential, generation): (Uuid,Uuid,Uuid) = sqlx::query_as("INSERT INTO vox_connections(user_id,connector_id,account_id,authorization_state,sync_timeline,metadata) VALUES($1,'gmail','new@example.test','authorized',true,'{\"last_history_id\":7}') RETURNING id,credential_generation,generation").bind(user).fetch_one(&mut *tx).await.unwrap();
        assert!(
            finalize_cursor(
                &mut tx,
                user,
                connection,
                "new@example.test",
                credential,
                Uuid::new_v4(),
                999,
                json!({"last_history_id":999})
            )
            .await
            .is_err()
        );
        let cursor: Value = sqlx::query_scalar(
            "SELECT metadata->'last_history_id' FROM vox_connections WHERE id=$1",
        )
        .bind(connection)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(cursor, json!(7));
        finalize_cursor(
            &mut tx,
            user,
            connection,
            "new@example.test",
            credential,
            generation,
            8,
            json!({"last_history_id":8}),
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();
    }
}
