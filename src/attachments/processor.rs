use chrono::Utc;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::attachments::{
    crypto::decrypt_attachment_secret,
    password_derivation::{PasswordDerivationOutcome, derive_provider_password},
    pdf::{PdfError, parse_pdf},
};

#[derive(Debug)]
pub enum AttachmentOutcome {
    Success,
    WaitingUser(String),
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("attachment not found")]
    NotFound,
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("processing failed: {0}")]
    Failed(String),
}

pub async fn process_attachment(
    pool: &PgPool,
    key: &[u8; 32],
    user_id: Uuid,
    attachment_id: Uuid,
    job_id: Uuid,
) -> Result<AttachmentOutcome, AttachmentError> {
    let mut tx = pool.begin().await?;

    let row = sqlx::query(
        "SELECT a.id, a.user_id, a.storage_owner_id, a.source_record_id, a.object_ref, a.encryption_metadata, \
                a.parse_state, j.attempt_count, j.max_attempts \
         FROM source_attachments a \
         JOIN jobs j ON j.id = $3 AND j.user_id = a.user_id \
         WHERE a.id = $1 AND a.user_id = $2 AND j.payload_reference_id=a.id AND j.kind='process_attachment' AND j.state='running' FOR UPDATE OF a, j",
    )
    .bind(attachment_id)
    .bind(user_id)
    .bind(job_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AttachmentError::NotFound)?;

    let source_record_id: Uuid = row.get("source_record_id");
    let object_ref: String = row.get("object_ref");
    let storage_owner_id: Uuid = row.get("storage_owner_id");
    let encryption_metadata: serde_json::Value = row.get("encryption_metadata");
    let attempt_count: i32 = row.get("attempt_count");
    let max_attempts: i32 = row.get("max_attempts");

    let read_result = if object_ref.starts_with(&format!("vox-obj://{storage_owner_id}/")) {
        crate::storage::object_storage::read_object(&object_ref).await.map_err(|e| e.to_string())
    } else { Err("attachment reference does not belong to this user".into()) };

    let bytes = match read_result {
        Ok(data) => data,
        Err(err) => {
            let error_msg = format!("Attachment file unreadable at {}: {}", object_ref, err);
            let next_attempts = attempt_count + 1;
            let will_retry = next_attempts < max_attempts;

            let (job_state, parse_state) = if will_retry {
                ("pending", "pending")
            } else {
                ("failed", "failed")
            };

            sqlx::query(
                "UPDATE jobs SET attempt_count = $1, state = $2, wait_reason = NULL, available_at=now()+interval '30 seconds', \
                                lease_owner = NULL, lease_expires_at = NULL, last_error_code = 'object_read_failed', \
                                completed_at = CASE WHEN $3 THEN NULL ELSE now() END \
                 WHERE id = $4 AND user_id = $5",
            )
            .bind(next_attempts)
            .bind(job_state)
            .bind(will_retry)
            .bind(job_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE source_attachments SET parse_state = $1, error = $2, \
                        expires_at = CASE WHEN $3 THEN LEAST(COALESCE(expires_at,now()+interval '7 days'),now()+interval '7 days') ELSE now()+interval '7 days' END, \
                        updated_at = now() \
                 WHERE id = $4 AND user_id = $5",
            )
            .bind(parse_state)
            .bind(&error_msg)
            .bind(will_retry)
            .bind(attachment_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "INSERT INTO updates (user_id, kind, content_version, category, title, summary, content, ui_hint, priority, status, source_job_id, dedupe_key) \
                 VALUES ($1, 'processing_issue', 1, 'storage', 'Attachment file unreadable', \
                         $2, jsonb_build_object('attachment_id', $3::text, 'object_ref', $4, 'error', $2), \
                         '{}'::jsonb, 'high', 'active', $5, $6) \
                 ON CONFLICT (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL \
                 DO UPDATE SET status = 'active', summary = EXCLUDED.summary, content = EXCLUDED.content, updated_at = now()",
            )
            .bind(user_id)
            .bind(&error_msg)
            .bind(attachment_id)
            .bind(&object_ref)
            .bind(job_id)
            .bind(format!("attachment_read_failure:{}", attachment_id))
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            return Ok(AttachmentOutcome::Failed(error_msg));
        }
    };

    let initial = parse_pdf_off_runtime(&bytes, None).await;
    let parse_result = if !matches!(&initial, Err(PdfError::PasswordRequired)) {
        initial
    } else {
        let encrypted_secret = encryption_metadata
            .get("encrypted_secret")
            .and_then(|v| v.as_str());
        let nonce = encryption_metadata.get("nonce").and_then(|v| v.as_str());
        let secret_expires_at = encryption_metadata
            .get("secret_expires_at")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let now = Utc::now();
        let is_expired = secret_expires_at.is_none_or(|exp| exp <= now);

        let provided_pw = if encrypted_secret.is_some() && nonce.is_some() && !is_expired {
            decrypt_attachment_secret(
                key,
                job_id,
                attachment_id,
                encrypted_secret.unwrap(),
                nonce.unwrap(),
            )
            .ok()
        } else {
            None
        };

        if let Some(ref pw) = provided_pw {
            parse_pdf_off_runtime(&bytes, Some(pw)).await
        } else {
            let user_facts: serde_json::Value = sqlx::query_scalar(
                "SELECT profile_facts FROM users WHERE id = $1",
            )
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or_else(|| serde_json::json!({}));

            let record_meta: serde_json::Value = sqlx::query_scalar(
                "SELECT metadata FROM source_records WHERE id = $1 AND user_id = $2",
            )
            .bind(source_record_id)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?
            .unwrap_or_else(|| serde_json::json!({}));

            let provider_hint = record_meta
                .get("subject")
                .and_then(|v| v.as_str())
                .or_else(|| record_meta.get("from").and_then(|v| v.as_str()));

            match derive_provider_password(provider_hint, &user_facts) {
                PasswordDerivationOutcome::CandidatePasswords(candidates) => {
                    let mut matched = None;
                    for cand in candidates {
                        if let Ok(doc) = parse_pdf_off_runtime(&bytes, Some(&cand)).await {
                            matched = Some(doc);
                            break;
                        }
                    }
                    if let Some(doc) = matched {
                        Ok(doc)
                    } else {
                        Err(PdfError::WrongPassword)
                    }
                }
                PasswordDerivationOutcome::MissingFact(fact) => {
                    let wait_msg = format!("Missing profile fact '{}' required for provider password derivation", fact);
                    sqlx::query(
                        "UPDATE source_attachments SET parse_state = 'waiting_user', \
                                error = $3, updated_at = now() \
                         WHERE id = $1 AND user_id = $2",
                    )
                    .bind(attachment_id)
                    .bind(user_id)
                    .bind(&wait_msg)
                    .execute(&mut *tx)
                    .await?;

                    sqlx::query(
                        "UPDATE jobs SET wait_reason = 'waiting_user', state = 'pending', \
                                        lease_owner = NULL, lease_expires_at = NULL \
                         WHERE id = $1 AND user_id = $2",
                    )
                    .bind(job_id)
                    .bind(user_id)
                    .execute(&mut *tx)
                    .await?;

                    sqlx::query(
                        "INSERT INTO updates (user_id, kind, content_version, category, title, summary, content, ui_hint, priority, status, source_job_id, dedupe_key) \
                         VALUES ($1, 'processing_issue', 1, 'security', 'Profile detail required to unlock attachment', \
                                 $2, jsonb_build_object('attachment_id', $3::text, 'reason', 'missing_profile_fact', 'missing_fact', $4), \
                                 '{}'::jsonb, 'high', 'active', $5, $6) \
                         ON CONFLICT (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL \
                         DO UPDATE SET status = 'active', summary = EXCLUDED.summary, content = EXCLUDED.content, updated_at = now()",
                    )
                    .bind(user_id)
                    .bind(&wait_msg)
                    .bind(attachment_id)
                    .bind(fact)
                    .bind(job_id)
                    .bind(format!("attachment_missing_fact:{}:{}", attachment_id, fact))
                    .execute(&mut *tx)
                    .await?;

                    tx.commit().await?;
                    return Ok(AttachmentOutcome::WaitingUser(wait_msg));
                }
                PasswordDerivationOutcome::NoProviderRule => {
                    Err(PdfError::PasswordRequired)
                }
            }
        }
    };

    match parse_result {
        Err(PdfError::WrongPassword) => {
            let next_attempts = attempt_count + 1;
            if next_attempts < max_attempts {
                sqlx::query(
                    "UPDATE jobs SET attempt_count = $1, wait_reason = 'waiting_user', \
                                    state = 'pending', lease_owner = NULL, lease_expires_at = NULL \
                     WHERE id = $2 AND user_id = $3",
                )
                .bind(next_attempts)
                .bind(job_id)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;

                sqlx::query(
                    "UPDATE source_attachments SET parse_state = 'waiting_user', \
                            error = 'Incorrect password', updated_at = now() \
                     WHERE id = $1 AND user_id = $2",
                )
                .bind(attachment_id)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;

                record_issue(&mut tx,user_id,attachment_id,job_id,"Incorrect attachment password","Password incorrect. Enter the correct password.","encrypted_pdf").await?;

                tx.commit().await?;
                Ok(AttachmentOutcome::WaitingUser("Incorrect password".into()))
            } else {
                sqlx::query(
                    "UPDATE jobs SET attempt_count = $1, state = 'failed', wait_reason = NULL, \
                                    completed_at = now() \
                     WHERE id = $2 AND user_id = $3",
                )
                .bind(next_attempts)
                .bind(job_id)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;

                sqlx::query(
                    "UPDATE source_attachments SET parse_state = 'failed', \
                            error = 'Maximum password attempts exceeded', \
                            expires_at = now() + interval '7 days', updated_at = now() \
                     WHERE id = $1 AND user_id = $2",
                )
                .bind(attachment_id)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;

                record_issue(&mut tx,user_id,attachment_id,job_id,"Attachment could not be unlocked","Maximum password attempts exceeded.","attempts_exhausted").await?;

                tx.commit().await?;
                Ok(AttachmentOutcome::Failed("Maximum attempts exceeded".into()))
            }
        }
        Err(PdfError::PasswordRequired) => {
            sqlx::query(
                "UPDATE source_attachments SET parse_state = 'waiting_user', \
                        error = 'password_required', updated_at = now() \
                 WHERE id = $1 AND user_id = $2",
            )
            .bind(attachment_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE jobs SET wait_reason = 'waiting_user', state = 'pending', \
                                lease_owner = NULL, lease_expires_at = NULL \
                 WHERE id = $1 AND user_id = $2",
            )
            .bind(job_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "INSERT INTO updates (user_id, kind, content_version, category, title, summary, content, ui_hint, priority, status, source_job_id, dedupe_key) \
                 VALUES ($1, 'processing_issue', 1, 'security', 'Password required for attachment', \
                         'Password required to parse attachment', jsonb_build_object('attachment_id', $2::text, 'reason', 'encrypted_pdf'), \
                         '{}'::jsonb, 'high', 'active', $3, $4) \
                 ON CONFLICT (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL \
                 DO UPDATE SET status = 'active', summary = EXCLUDED.summary, updated_at = now()",
            )
            .bind(user_id)
            .bind(attachment_id)
            .bind(job_id)
            .bind(format!("attachment_password:{}", attachment_id))
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            Ok(AttachmentOutcome::WaitingUser("Password required".into()))
        }
        Err(err) => {
            let next_attempts = attempt_count + 1;
            let will_retry = next_attempts < max_attempts && matches!(&err,PdfError::ExtractionFailed(message) if message.contains("time limit") || message.contains("unavailable") || message.contains("unexpectedly"));
            let (job_state, parse_state) = if will_retry {
                ("pending", "processing")
            } else {
                ("failed", "failed")
            };

            sqlx::query(
                "UPDATE jobs SET attempt_count = $1, state = $2, lease_owner = NULL, lease_expires_at = NULL, \
                                last_error_code = 'parse_error', available_at=now()+interval '30 seconds', \
                                completed_at = CASE WHEN $3 THEN NULL ELSE now() END \
                 WHERE id = $4 AND user_id = $5",
            )
            .bind(next_attempts)
            .bind(job_state)
            .bind(will_retry)
            .bind(job_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE source_attachments SET parse_state = $1, error = $2, \
                        expires_at = CASE WHEN $3 THEN LEAST(COALESCE(expires_at,now()+interval '7 days'),now()+interval '7 days') ELSE now()+interval '7 days' END, \
                        updated_at = now() \
                 WHERE id = $4 AND user_id = $5",
            )
            .bind(parse_state)
            .bind(err.to_string())
            .bind(will_retry)
            .bind(attachment_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            record_issue(&mut tx,user_id,attachment_id,job_id,"Attachment parsing failed",&err.to_string(),"parse_error").await?;
            tx.commit().await?;
            Ok(AttachmentOutcome::Failed(err.to_string()))
        }
        Ok(doc) => {
            let result = crate::storage::timeline::TimelineRepository::new(pool.clone()).ingest_event_in_transaction(&mut tx,user_id,
                crate::domain::timeline::IngestTimelineEventInput {
                    event_type_id:None,event_type_value:Some(doc.kind.clone()),group_id:None,group_value:Some("finance".into()),
                    title:doc.title.clone(),summary:Some(doc.summary.clone()),occurred_at:doc.occurred_at,ended_at:doc.ended_at,
                    time_precision:"day".into(),source_timezone:None,content:doc.facts.clone(),confidence:0.8,
                    dedupe_key:Some(format!("attachment:{attachment_id}")),
                    evidence:vec![crate::domain::timeline::NewEvidenceItem {source_record_id:Some(source_record_id),source_attachment_id:Some(attachment_id),
                        source_type:"source_attachment".into(),source_id:Some(attachment_id.to_string()),raw_reference:None,observation_metadata:doc.facts.clone()}],
                }).await.map_err(|error| AttachmentError::Failed(error.to_string()))?;
            crate::finance_normalization::dedupe_or_settle_in_transaction(&mut tx,user_id,result.event.id,&doc.facts).await?;

            sqlx::query(
                "UPDATE source_records SET temporary_content_ref = NULL \
                 WHERE id = $1 AND user_id = $2",
            )
            .bind(source_record_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE source_attachments SET parse_state = 'parsed', \
                        encryption_metadata = encryption_metadata - 'encrypted_secret' - 'nonce' - 'secret_expires_at', expires_at=now(), \
                        error = NULL, updated_at = now() \
                 WHERE id = $1 AND user_id = $2",
            )
            .bind(attachment_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE updates SET status = 'resolved', resolved_at = now(), \
                        summary = 'Attachment processed successfully', updated_at = now() \
                 WHERE source_job_id = $1 AND user_id = $2",
            )
            .bind(job_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE jobs SET state = 'completed', completed_at = now(), lease_owner = NULL, lease_expires_at = NULL \
                 WHERE id = $1 AND user_id = $2",
            )
            .bind(job_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;

            if let Err(error) = crate::storage::object_storage::delete_object(&object_ref).await {
                tracing::warn!(%attachment_id, %error, "parsed attachment deletion will retry through retention");
            }

            Ok(AttachmentOutcome::Success)
        }
    }
}

async fn record_issue(tx: &mut sqlx::Transaction<'_,sqlx::Postgres>,user_id:Uuid,attachment_id:Uuid,job_id:Uuid,title:&str,summary:&str,reason:&str) -> Result<(),sqlx::Error> {
    sqlx::query("INSERT INTO updates(user_id,kind,category,title,summary,content,priority,source_job_id,dedupe_key) VALUES($1,'processing_issue','attachments',$2,$3,jsonb_build_object('attachment_id',$4::text,'reason',$5::text),'high',$6,$7) ON CONFLICT(user_id,dedupe_key) WHERE dedupe_key IS NOT NULL DO UPDATE SET status='active',title=EXCLUDED.title,summary=EXCLUDED.summary,content=EXCLUDED.content,read_at=NULL,updated_at=now()")
        .bind(user_id).bind(title).bind(summary).bind(attachment_id).bind(reason).bind(job_id).bind(format!("attachment_issue:{attachment_id}"))
        .execute(&mut **tx).await?;
    Ok(())
}

async fn parse_pdf_off_runtime(bytes: &[u8], password: Option<&str>) -> Result<crate::attachments::pdf::ExtractedFinancialDocument, PdfError> {
    static PARSERS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    let permit = tokio::time::timeout(std::time::Duration::from_secs(10),PARSERS.get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2))).clone().acquire_owned()).await
        .map_err(|_| PdfError::ExtractionFailed("PDF parser unavailable: capacity wait exceeded".into()))?
        .map_err(|_| PdfError::ExtractionFailed("PDF parser unavailable".into()))?;
    let bytes = bytes.to_vec();
    let password = password.map(str::to_owned);
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        parse_pdf(&bytes, password.as_deref())
    });
    tokio::time::timeout(std::time::Duration::from_secs(45), task).await
        .map_err(|_| PdfError::ExtractionFailed("PDF extraction exceeded its time limit".into()))?
        .map_err(|_| PdfError::ExtractionFailed("PDF parser stopped unexpectedly".into()))?
}
