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
    let Ok(mut lease) = pool.begin().await else { return false; };
    let locked = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("gmail-connection:{connection_id}")).fetch_one(&mut *lease).await;
    if !matches!(locked, Ok(true)) { return false; }
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
    if sync_run_id.is_nil() { return false; }

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

    let Ok(tokens) = connections_service.get_gmail_tokens(user_id, connection_id).await else {
        let _ = sqlx::query("UPDATE connector_sync_runs SET status = 'failed', error = 'token_refresh_failed', completed_at = now() WHERE id = $1")
            .bind(sync_run_id)
            .execute(pool)
            .await;
        return false;
    };

    let gmail_client = vox_connections::providers::gmail::GmailClient::new();
    let history_res = gmail_client.list_history(&tokens.access_token, last_history_id).await;

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
                            .and_then(|s| DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&Utc).timestamp()))
                            .or_else(|| created.map(|c| c.timestamp()))
                    })
                    .unwrap_or_else(|| Utc::now().timestamp());

                let since_ts = match last_synced_at {
                    Some(d) => d.timestamp().max(baseline_ts),
                    None => baseline_ts,
                };

                let recovered_ids = match gmail_client.list_messages_after(&tokens.access_token, since_ts).await {
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
    let mut all_processed_cleanly = true;

    for msg_id in &message_ids {
        records_extracted += 1;
        match ingest_message(pool, &gmail_client, &tokens.access_token, user_id, email_address, msg_id).await {
            Ok(ingested) => records_ingested += i32::from(ingested),
            Err(error) => {
                all_processed_cleanly = false;
                tracing::warn!(%user_id, %connection_id, message_id = %msg_id, error = %error, "gmail ingestion failed; cursor retained");
            }
        }
    }

    if all_processed_cleanly {
        let meta_update = json!({
            "last_history_id": new_cursor,
            "last_synced_at": Utc::now(),
        });
        let finalized: Result<(), sqlx::Error> = async {
            let mut tx = pool.begin().await?;
            sqlx::query("UPDATE vox_connections SET metadata=metadata || $1,last_synced_at=now(),updated_at=now() WHERE id=$2 AND user_id=$3 AND COALESCE((metadata->>'last_history_id')::numeric,0) <= $4::numeric")
                .bind(meta_update).bind(connection_id).bind(user_id).bind(new_cursor.to_string()).execute(&mut *tx).await?;
            sqlx::query("UPDATE connector_sync_runs SET status='completed',records_extracted=$1,records_ingested=$2,cursor_state=$3,completed_at=now() WHERE id=$4")
                .bind(records_extracted).bind(records_ingested).bind(json!({"history_id":new_cursor})).bind(sync_run_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE connector_coverage SET coverage_end=now(),is_healthy=true,last_checked_at=now(),updated_at=now() WHERE user_id=$1 AND connector_id='gmail'")
                .bind(user_id).execute(&mut *tx).await?;
            tx.commit().await
        }.await;
        if let Err(error) = finalized { tracing::error!(%connection_id,%error,"gmail cursor finalization failed"); return false; }

    } else {
        let _ = sqlx::query(
            "UPDATE connector_sync_runs SET status = 'failed', records_extracted = $1, records_ingested = $2, \
             error = 'partial_message_fetch_failure', completed_at = now() WHERE id = $3",
        )
        .bind(records_extracted)
        .bind(records_ingested)
        .bind(sync_run_id)
        .execute(pool)
        .await;
        return false;
    }
    true
}

pub async fn reconcile_all_gmail_connections(pool: &PgPool) {
    let rows = sqlx::query(
        "SELECT c.id, c.user_id, c.metadata, c.account_id, c.account_display_id, c.last_synced_at \
         FROM vox_connections c \
         WHERE c.connector_id = 'gmail' AND c.authorization_state = 'authorized'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for row in rows {
        let conn_id: Uuid = row.get("id");
        let u_id: Uuid = row.get("user_id");
        let metadata: Value = row.get("metadata");
        let email: String = row.get::<Option<String>, _>("account_id")
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
                let _ = svc.renew_gmail_watch(u_id, conn_id).await;
            }
        }

        process_gmail_delta(pool, u_id, conn_id, &email, last_hist_id, 0, last_synced).await;
    }
}

async fn ingest_message(
    pool: &PgPool, gmail: &vox_connections::providers::gmail::GmailClient,
    token: &str, user_id: Uuid, email: &str, message_id: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("gmail:{user_id}:{message_id}")).execute(&mut *tx).await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM source_records WHERE user_id=$1 AND connector_id='gmail' AND source_record_id=$2)")
        .bind(user_id).bind(message_id).fetch_one(&mut *tx).await?;
    if exists { return Ok(false); }
    let msg = gmail.get_message(token, message_id).await?;
    let metadata = json!({"from":msg.from,"to":msg.to,"subject":msg.subject,"date":msg.date,
        "snippet":msg.snippet,"internal_date":msg.internal_date,"attachment_count":msg.attachments.len()});
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&metadata)?));
    let record_id: Uuid = sqlx::query_scalar("INSERT INTO source_records(user_id,connector_id,source_account_id,source_record_id,record_hash,disposition,metadata) VALUES($1,'gmail',$2,$3,$4,'retained',$5) RETURNING id")
        .bind(user_id).bind(email).bind(message_id).bind(hash).bind(&metadata).fetch_one(&mut *tx).await?;
    let mut stored_objects = Vec::new();
    let outcome: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
        for attachment in &msg.attachments {
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
        let facts = msg.body_text.as_deref().and_then(|body| crate::attachments::pdf::extract_document_facts(body, None).ok());
        if let Some(doc) = facts {
            let result = crate::storage::timeline::TimelineRepository::new(pool.clone()).ingest_event_in_transaction(&mut tx,user_id,
                crate::domain::timeline::IngestTimelineEventInput {event_type_id:None,event_type_value:Some(doc.kind),group_id:None,group_value:Some("finance".into()),
                    title:doc.title,summary:Some(doc.summary),occurred_at:doc.occurred_at,ended_at:None,time_precision:"day".into(),source_timezone:None,
                    content:doc.facts.clone(),confidence:0.8,dedupe_key:Some(format!("gmail:{message_id}")),
                    evidence:vec![crate::domain::timeline::NewEvidenceItem {source_record_id:Some(record_id),source_attachment_id:None,
                        source_type:"gmail".into(),source_id:Some(message_id.into()),raw_reference:None,observation_metadata:metadata.clone()}],
                }).await?;
            crate::finance_normalization::dedupe_or_settle_in_transaction(&mut tx,user_id,result.event.id,&doc.facts).await?;
        } else {
            sqlx::query("INSERT INTO updates(user_id,kind,content_version,category,title,summary,content,priority,status,dedupe_key) VALUES($1,'email_notice',1,'email',$2,$3,$4,'low','active',$5)")
                .bind(user_id).bind(msg.subject.as_deref().unwrap_or("Email notification"))
                .bind(&msg.snippet).bind(&metadata).bind(format!("email_notice:{message_id}")).execute(&mut *tx).await?;
        }
        Ok(())
    }.await;
    if let Err(error) = outcome {
        tx.rollback().await?;
        for object in stored_objects { if let Err(error) = crate::storage::object_storage::delete_object(&object).await { tracing::warn!(%error,"gmail rollback object cleanup failed"); } }
        return Err(error);
    }
    tx.commit().await?;
    Ok(true)
}
