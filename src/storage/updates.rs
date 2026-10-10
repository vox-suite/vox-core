use chrono::Utc;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::updates::{
    JobActionResponse, JobInputRequest, JobRetryRequest, UpdateItem, UpdatesQuery,
};

#[derive(Debug, thiserror::Error)]
pub enum UpdatesStorageError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("update not found")]
    NotFound,
    #[error("job not found or unauthorized")]
    JobNotFound,
    #[error("bounded attempts exceeded: job has reached maximum attempts")]
    BoundedAttemptsExceeded,
    #[error("invalid job state for retry")]
    InvalidJobState,
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[derive(Clone)]
pub struct UpdatesRepository {
    pool: PgPool,
}

impl UpdatesRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn list_updates(
        &self,
        user_id: Uuid,
        query: UpdatesQuery,
    ) -> Result<Vec<UpdateItem>, UpdatesStorageError> {
        if query.before.is_some() != query.before_id.is_some() {
            return Err(UpdatesStorageError::InvalidInput(
                "pagination requires date and ID".into(),
            ));
        }
        let fetch_limit = query.limit.unwrap_or(50).clamp(1, 100);
        let status_filter = query.status.unwrap_or_else(|| "active".to_string());

        let rows = sqlx::query(
            "SELECT u.id, u.user_id, u.kind, u.content_version, u.category, u.title, u.summary, \
                    u.content, u.ui_hint, u.priority, u.status, u.read_at, u.source_job_id, \
                    u.dedupe_key, u.published_at, u.expires_at, u.resolved_at, u.created_at, u.updated_at, \
                    j.state AS job_state, j.attempt_count, j.max_attempts, j.wait_reason \
             FROM updates u \
             LEFT JOIN jobs j ON j.id = u.source_job_id AND j.user_id = u.user_id \
             WHERE u.user_id = $1 \
               AND ($2::text = 'all' OR u.status = $2) \
               AND ($3::text IS NULL OR u.kind = $3) \
               AND ($4::text IS NULL OR u.category = $4) AND (u.expires_at IS NULL OR u.expires_at>now()) \
               AND ($6::timestamptz IS NULL OR (u.published_at,u.id)<($6,$7::uuid)) \
             ORDER BY u.published_at DESC,u.id DESC \
             LIMIT $5",
        )
        .bind(user_id)
        .bind(status_filter)
        .bind(query.kind)
        .bind(query.category)
        .bind(fetch_limit)
        .bind(query.before)
        .bind(query.before_id)
        .fetch_all(&self.pool)
        .await?;

        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let status: String = row.get("status");
            let read_at: Option<chrono::DateTime<Utc>> = row.get("read_at");
            let job_state: Option<String> = row.get("job_state");
            let wait_reason: Option<String> = row.get("wait_reason");
            let attempt_count: Option<i32> = row.get("attempt_count");
            let max_attempts: Option<i32> = row.get("max_attempts");

            let mut actions = Vec::new();
            if status == "active" {
                if read_at.is_none() {
                    actions.push("read".to_string());
                }
                actions.push("dismiss".to_string());
                if job_state.as_deref().is_none_or(|s| s == "completed") {
                    actions.push("resolve".to_string());
                }

                if let Some(ref jstate) = job_state {
                    let attempts = attempt_count.unwrap_or(0);
                    let max = max_attempts.unwrap_or(5);
                    if wait_reason.as_deref() == Some("waiting_user") {
                        actions.push("provide_input".to_string());
                    } else if (jstate == "failed" || jstate == "cancelled") && attempts < max {
                        actions.push("retry".to_string());
                    }
                }
            }

            items.push(UpdateItem {
                id: row.get("id"),
                user_id: row.get("user_id"),
                kind: row.get("kind"),
                content_version: row.get("content_version"),
                category: row.get("category"),
                title: row.get("title"),
                summary: row.get("summary"),
                content: row.get("content"),
                ui_hint: row.get("ui_hint"),
                priority: row.get("priority"),
                status,
                read_at,
                source_job_id: row.get("source_job_id"),
                dedupe_key: row.get("dedupe_key"),
                published_at: row.get("published_at"),
                expires_at: row.get("expires_at"),
                resolved_at: row.get("resolved_at"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
                available_actions: actions,
            });
        }

        Ok(items)
    }

    pub async fn get_update(
        &self,
        user_id: Uuid,
        id: Uuid,
    ) -> Result<UpdateItem, UpdatesStorageError> {
        let row = sqlx::query(
            "SELECT u.id, u.user_id, u.kind, u.content_version, u.category, u.title, u.summary, \
                    u.content, u.ui_hint, u.priority, u.status, u.read_at, u.source_job_id, \
                    u.dedupe_key, u.published_at, u.expires_at, u.resolved_at, u.created_at, u.updated_at, \
                    j.state AS job_state, j.attempt_count, j.max_attempts, j.wait_reason \
             FROM updates u \
             LEFT JOIN jobs j ON j.id = u.source_job_id AND j.user_id = u.user_id \
             WHERE u.id = $1 AND u.user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(UpdatesStorageError::NotFound)?;

        let status: String = row.get("status");
        let read_at: Option<chrono::DateTime<Utc>> = row.get("read_at");
        let job_state: Option<String> = row.get("job_state");
        let wait_reason: Option<String> = row.get("wait_reason");
        let attempt_count: Option<i32> = row.get("attempt_count");
        let max_attempts: Option<i32> = row.get("max_attempts");

        let mut actions = Vec::new();
        if status == "active" {
            if read_at.is_none() {
                actions.push("read".to_string());
            }
            actions.push("dismiss".to_string());
            actions.push("resolve".to_string());

            if let Some(ref jstate) = job_state {
                let attempts = attempt_count.unwrap_or(0);
                let max = max_attempts.unwrap_or(5);
                if wait_reason.as_deref() == Some("waiting_user") {
                    actions.push("provide_input".to_string());
                    if attempts < max {
                        actions.push("retry".to_string());
                    }
                } else if (jstate == "failed" || jstate == "cancelled") && attempts < max {
                    actions.push("retry".to_string());
                }
            }
        }

        Ok(UpdateItem {
            id: row.get("id"),
            user_id: row.get("user_id"),
            kind: row.get("kind"),
            content_version: row.get("content_version"),
            category: row.get("category"),
            title: row.get("title"),
            summary: row.get("summary"),
            content: row.get("content"),
            ui_hint: row.get("ui_hint"),
            priority: row.get("priority"),
            status,
            read_at,
            source_job_id: row.get("source_job_id"),
            dedupe_key: row.get("dedupe_key"),
            published_at: row.get("published_at"),
            expires_at: row.get("expires_at"),
            resolved_at: row.get("resolved_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            available_actions: actions,
        })
    }

    pub async fn mark_read(
        &self,
        user_id: Uuid,
        id: Uuid,
    ) -> Result<UpdateItem, UpdatesStorageError> {
        let rows = sqlx::query(
            "UPDATE updates SET read_at = now(), updated_at = now() \
             WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        if rows.rows_affected() == 0 {
            return Err(UpdatesStorageError::NotFound);
        }
        self.get_update(user_id, id).await
    }

    pub async fn dismiss(
        &self,
        user_id: Uuid,
        id: Uuid,
    ) -> Result<UpdateItem, UpdatesStorageError> {
        let rows = sqlx::query(
            "UPDATE updates SET status = 'dismissed', updated_at = now() \
             WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        if rows.rows_affected() == 0 {
            return Err(UpdatesStorageError::NotFound);
        }
        self.get_update(user_id, id).await
    }

    pub async fn resolve(
        &self,
        user_id: Uuid,
        id: Uuid,
    ) -> Result<UpdateItem, UpdatesStorageError> {
        let update = self.get_update(user_id, id).await?;
        if let Some(job_id) = update.source_job_id {
            let job_row = sqlx::query(
                "SELECT state, wait_reason FROM jobs WHERE id = $1 AND user_id = $2 AND (source_job_id IS NULL OR EXISTS(SELECT 1 FROM jobs j WHERE j.id=updates.source_job_id AND j.user_id=updates.user_id AND j.state='completed' AND j.wait_reason IS NULL))",
            )
            .bind(job_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;

            if let Some(jr) = job_row {
                let j_state: String = jr.get("state");
                let wait_reason: Option<String> = jr.get("wait_reason");
                if j_state != "completed" || wait_reason.is_some() {
                    return Err(UpdatesStorageError::InvalidJobState);
                }
            }
        }

        let rows = sqlx::query(
            "UPDATE updates SET status = 'resolved', resolved_at = now(), updated_at = now() \
             WHERE id = $1 AND user_id = $2 AND (source_job_id IS NULL OR EXISTS(SELECT 1 FROM jobs j WHERE j.id=updates.source_job_id AND j.user_id=updates.user_id AND j.state='completed' AND j.wait_reason IS NULL))",
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        if rows.rows_affected() == 0 {
            return Err(UpdatesStorageError::InvalidJobState);
        }
        self.get_update(user_id, id).await
    }

    pub async fn retry_job(
        &self,
        user_id: Uuid,
        job_id: Uuid,
        request: JobRetryRequest,
    ) -> Result<JobActionResponse, UpdatesStorageError> {
        let mut tx = self.pool.begin().await?;

        let job_row = sqlx::query(
            "SELECT id, state, attempt_count, max_attempts, action_idempotency_keys FROM jobs WHERE id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(job_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(UpdatesStorageError::JobNotFound)?;

        let keys: serde_json::Value = job_row.get("action_idempotency_keys");
        if request.idempotency_key.as_ref().is_some_and(|key| {
            keys.as_array()
                .is_some_and(|keys| keys.iter().any(|v| v.as_str() == Some(key)))
        }) {
            return Ok(JobActionResponse {
                job_id,
                status: "queued".into(),
                message: "Retry already accepted".into(),
            });
        }
        if request
            .idempotency_key
            .as_ref()
            .is_some_and(|k| Uuid::parse_str(k).is_err())
        {
            return Err(UpdatesStorageError::InvalidInput(
                "idempotency key must be a UUID".into(),
            ));
        }
        let state: String = job_row.get("state");
        let attempt_count: i32 = job_row.get("attempt_count");
        let max_attempts: i32 = job_row.get("max_attempts");

        if attempt_count >= max_attempts {
            return Err(UpdatesStorageError::BoundedAttemptsExceeded);
        }

        if state != "failed" && state != "cancelled" {
            return Err(UpdatesStorageError::InvalidJobState);
        }

        sqlx::query(
            "UPDATE jobs SET state = 'pending', wait_reason = NULL, lease_owner = NULL, \
                            lease_expires_at = NULL, available_at = now(), \
                            action_idempotency_keys = CASE WHEN $3::text IS NULL THEN action_idempotency_keys ELSE action_idempotency_keys || jsonb_build_array($3::text) END \
             WHERE id = $1 AND user_id = $2",
        )
        .bind(job_id)
        .bind(user_id)
        .bind(request.idempotency_key.as_deref())
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE updates SET status = 'active', updated_at = now() \
             WHERE source_job_id = $1 AND user_id = $2 AND status <> 'resolved'",
        )
        .bind(job_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(JobActionResponse {
            job_id,
            status: "queued".to_string(),
            message: "Job queued for retry".to_string(),
        })
    }

    pub async fn provide_job_input(
        &self,
        user_id: Uuid,
        job_id: Uuid,
        input: JobInputRequest,
    ) -> Result<JobActionResponse, UpdatesStorageError> {
        let mut tx = self.pool.begin().await?;

        let job_row = sqlx::query(
            "SELECT id, state, wait_reason, attempt_count, max_attempts FROM jobs WHERE id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(job_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(UpdatesStorageError::JobNotFound)?;

        let state: String = job_row.get("state");
        let wait: Option<String> = job_row.get("wait_reason");
        if state == "running" || state == "completed" || (state == "pending" && wait.is_none()) {
            return Err(UpdatesStorageError::InvalidJobState);
        }
        if job_row.get::<i32, _>("attempt_count") >= job_row.get::<i32, _>("max_attempts") {
            return Err(UpdatesStorageError::BoundedAttemptsExceeded);
        }
        let input_type = input.input_type.trim();
        if input_type.is_empty() {
            return Err(UpdatesStorageError::InvalidInput(
                "missing input_type".into(),
            ));
        }

        match input_type {
            "password" | "attachment_password" => {
                let password = input.data.get("password").and_then(|v| v.as_str());
                let Some(pw) = password else {
                    return Err(UpdatesStorageError::InvalidInput(
                        "password field required".into(),
                    ));
                };
                if pw.trim().is_empty() || pw.len() > 1024 {
                    return Err(UpdatesStorageError::InvalidInput(
                        "password cannot be empty".into(),
                    ));
                }

                let att_row = sqlx::query(
                    "SELECT id FROM source_attachments WHERE user_id=$1 AND id=(SELECT payload_reference_id FROM jobs WHERE id=$2 AND user_id=$1 AND kind='process_attachment') AND raw_deleted_at IS NULL FOR UPDATE",
                )
                .bind(user_id)
                .bind(job_id)
                .fetch_optional(&mut *tx)
                .await?;

                let Some(att) = att_row else {
                    return Err(UpdatesStorageError::InvalidInput(
                        "no attachment pending input for this job".into(),
                    ));
                };
                let attachment_id: Uuid = att.get("id");

                let key = crate::attachments::crypto::attachment_master_key().map_err(|_| {
                    UpdatesStorageError::InvalidInput(
                        "attachment encryption key is not configured".into(),
                    )
                })?;
                let (ciphertext, nonce) = crate::attachments::crypto::encrypt_attachment_secret(
                    &key,
                    job_id,
                    attachment_id,
                    pw,
                )
                .map_err(|e| {
                    UpdatesStorageError::InvalidInput(format!("encryption failed: {e}"))
                })?;

                let ttl_expiry = Utc::now() + chrono::Duration::minutes(15);
                let encrypted_meta = serde_json::json!({
                    "encrypted_secret": ciphertext,
                    "nonce": nonce,
                    "secret_expires_at": ttl_expiry.to_rfc3339(),
                    "secret_hint": "user_provided"
                });

                sqlx::query(
                    "UPDATE source_attachments SET parse_state = 'processing', \
                            encryption_metadata = encryption_metadata || $1, \
                            error = NULL, \
                            updated_at = now() \
                     WHERE id = $2 AND user_id = $3",
                )
                .bind(encrypted_meta)
                .bind(attachment_id)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;

                sqlx::query(
                    "UPDATE jobs SET state = 'pending', wait_reason = NULL, lease_owner = NULL, \
                                    lease_expires_at = NULL, available_at = now(), \
                                    payload_reference_id = COALESCE(payload_reference_id, $1) \
                     WHERE id = $2 AND user_id = $3",
                )
                .bind(attachment_id)
                .bind(job_id)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;

                sqlx::query(
                    "UPDATE updates SET status = 'active', summary = 'Retrying attachment extraction with provided credentials', updated_at = now() \
                     WHERE source_job_id = $1 AND user_id = $2",
                )
                .bind(job_id)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
            }
            "profile_field" => {
                let field_name = input.data.get("field").and_then(|v| v.as_str());
                let field_value = input.data.get("value").and_then(|v| v.as_str());
                let (Some(f_name), Some(f_val)) = (field_name, field_value) else {
                    return Err(UpdatesStorageError::InvalidInput(
                        "field and value required".into(),
                    ));
                };
                if ![
                    "name",
                    "full_name",
                    "dob",
                    "date_of_birth",
                    "bank_phone",
                    "pan",
                ]
                .contains(&f_name)
                    || f_val.trim().is_empty()
                    || f_val.len() > 120
                {
                    return Err(UpdatesStorageError::InvalidInput(
                        "field name and value cannot be empty".into(),
                    ));
                }

                sqlx::query(
                    "UPDATE users SET profile_facts = profile_facts || jsonb_build_object($1::text, $2::text), updated_at = now() \
                     WHERE id = $3",
                )
                .bind(f_name)
                .bind(f_val)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;

                sqlx::query(
                    "UPDATE jobs SET state = 'pending', wait_reason = NULL, lease_owner = NULL, \
                                    lease_expires_at = NULL, available_at = now() \
                     WHERE id = $1 AND user_id = $2",
                )
                .bind(job_id)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;

                sqlx::query(
                    "UPDATE updates SET status = 'active', summary = 'Profile field received; job resumed', updated_at = now() \
                     WHERE source_job_id = $1 AND user_id = $2",
                )
                .bind(job_id)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
            }
            other => {
                return Err(UpdatesStorageError::InvalidInput(format!(
                    "unrecognized input_type: {other}"
                )));
            }
        }

        tx.commit().await?;

        Ok(JobActionResponse {
            job_id,
            status: "processing".to_string(),
            message: "User input applied successfully".to_string(),
        })
    }

    pub async fn create_update(
        &self,
        user_id: Uuid,
        kind: &str,
        content_version: i32,
        category: &str,
        title: &str,
        summary: Option<&str>,
        content: &serde_json::Value,
        ui_hint: &serde_json::Value,
        priority: &str,
        source_job_id: Option<Uuid>,
        dedupe_key: Option<&str>,
        expires_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<UpdateItem, UpdatesStorageError> {
        validate_update_content(kind, content_version, content)?;

        let row = sqlx::query(
            "INSERT INTO updates (user_id, kind, content_version, category, title, summary, content, ui_hint, priority, status, source_job_id, dedupe_key, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'active', $10, $11, $12) \
             ON CONFLICT (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL \
             DO UPDATE SET \
                 kind = EXCLUDED.kind, \
                 content_version = EXCLUDED.content_version, \
                 category = EXCLUDED.category, \
                 title = EXCLUDED.title, \
                 summary = EXCLUDED.summary, \
                 content = EXCLUDED.content, \
                 ui_hint = EXCLUDED.ui_hint, \
                 priority = EXCLUDED.priority, \
                 source_job_id = EXCLUDED.source_job_id, \
                 expires_at = EXCLUDED.expires_at, \
                 updated_at = now() \
             RETURNING id",
        )
        .bind(user_id)
        .bind(kind)
        .bind(content_version)
        .bind(category)
        .bind(title)
        .bind(summary)
        .bind(content)
        .bind(ui_hint)
        .bind(priority)
        .bind(source_job_id)
        .bind(dedupe_key)
        .bind(expires_at)
        .fetch_one(&self.pool)
        .await?;

        let id: Uuid = row.get("id");
        self.get_update(user_id, id).await
    }
}

pub fn validate_update_content(
    kind: &str,
    version: i32,
    content: &serde_json::Value,
) -> Result<(), UpdatesStorageError> {
    if !content.is_object() {
        return Err(UpdatesStorageError::InvalidInput(
            "content must be a json object".into(),
        ));
    }
    match kind {
        "briefing" => {
            let has_field = content.get("items").is_some()
                || content.get("sections").is_some()
                || content.get("summary_points").is_some()
                || content.get("headline").is_some()
                || content.get("summary").is_some()
                || content.get("body").is_some();
            if !has_field {
                return Err(UpdatesStorageError::InvalidInput(format!(
                    "briefing content v{version} requires at least one of: items, sections, summary_points, headline, summary, body"
                )));
            }
        }
        "email_notice" => {
            let has_field = content.get("sender").is_some()
                || content.get("subject").is_some()
                || content.get("message_id").is_some()
                || content.get("summary").is_some();
            if !has_field {
                return Err(UpdatesStorageError::InvalidInput(format!(
                    "email_notice content v{version} requires at least one of: sender, subject, message_id, summary"
                )));
            }
        }
        "processing_issue" => {
            let has_field = content.get("error").is_some()
                || content.get("reason").is_some()
                || content.get("attachment_id").is_some()
                || content.get("issue_type").is_some();
            if !has_field {
                return Err(UpdatesStorageError::InvalidInput(format!(
                    "processing_issue content v{version} requires at least one of: error, reason, attachment_id, issue_type"
                )));
            }
        }
        "connection_status" => {
            let has_field = content.get("connector_id").is_some()
                || content.get("status").is_some()
                || content.get("connection_id").is_some()
                || content.get("error").is_some();
            if !has_field {
                return Err(UpdatesStorageError::InvalidInput(format!(
                    "connection_status content v{version} requires at least one of: connector_id, status, connection_id, error"
                )));
            }
        }
        "daily_plan" => {
            let has_field = content.get("date").is_some()
                || content.get("items").is_some()
                || content.get("tasks").is_some()
                || content.get("events").is_some()
                || content.get("schedule").is_some();
            if !has_field {
                return Err(UpdatesStorageError::InvalidInput(format!(
                    "daily_plan content v{version} requires at least one of: date, items, tasks, events, schedule"
                )));
            }
        }
        other => {
            return Err(UpdatesStorageError::InvalidInput(format!(
                "unrecognized update kind: {other}"
            )));
        }
    }
    Ok(())
}
