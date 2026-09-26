/**
 * Privacy, retention, deletion of platform-controlled history, and portable non-secret export/import.
 *
 * Implements PRD Section 11.19 (Retention and deletion) and 11.20 (Portability), satisfying E43 (vox-core#29):
 * - FR-DAT-001/002/004/005/006/007: Declared retention and backup behavior, deletion of platform task history
 *   with explicit disclosure of external carrier logs, remote operator logs, legal audit hold (up to 365 days),
 *   and disaster recovery backups (up to 30 days). Local deletion is never represented as universal deletion or undo.
 * - FR-PRT-001/002/003/004/005: Separate portable exports for config, preferences, and sensitive task/audit history.
 *   Strictly excludes raw credentials, API secrets, passwords, active proposals, and reusable authority.
 * - FR-PRT-006: Imported connections require renewed authorization on the destination deployment.
 * - Historical action evidence remains fully interpretable after integration removal or deprecation.
 */
use crate::{db::Db, identity::ResolvedUserContext};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

pub const CANONICAL_DELETION_DISCLOSURE: &str = "Platform task entries and conversation turns are removed from active databases. \
    External service records (e.g. Amazon, Expedia, Uber, Twilio carrier receipts), \
    remote operator logs, legal audit hold retention (up to 365 days), and disaster recovery backups \
    (up to 30 days) are beyond immediate platform deletion. Deleting task history does not cancel or \
    refund completed external transactions.";

pub const PREFERENCE_AUTHORITY_DISCLAIMER: &str = "User preference is advisory context only. It confers no execution authority. \
    Provider currency, timezone, and inventory facts remain strictly authoritative.";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeclaredRetentionPolicy {
    pub task_history_retention_days: u32,
    pub audit_retention_days: u32,
    pub backup_window_days: u32,
    pub temporary_context_retention_hours: u32,
    pub external_limitations_disclosure: String,
}

impl Default for DeclaredRetentionPolicy {
    fn default() -> Self {
        Self {
            task_history_retention_days: 90,
            audit_retention_days: 365,
            backup_window_days: 30,
            temporary_context_retention_hours: 24,
            external_limitations_disclosure: CANONICAL_DELETION_DISCLOSURE.to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeleteHistoryResult {
    pub deleted_spans_count: usize,
    pub deleted_conversations_count: usize,
    pub disclosure: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PruneResult {
    pub pruned_spans_count: usize,
    pub pruned_conversations_count: usize,
    pub pruned_jobs_count: usize,
    pub retention_cutoff: DateTime<Utc>,
    pub audit_hold_cutoff: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportedAgentDefinition {
    pub external_key: String,
    pub display_name: String,
    pub description: String,
    pub requested_capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportedIntegrationDeclaration {
    pub external_key: String,
    pub display_name: String,
    pub protocol: String,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportedConnectionDeclaration {
    pub integration_key: String,
    pub account_display_id: Option<String>,
    pub credential_custody: String,
    pub requires_renewed_authorization: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportedConfig {
    pub agents: Vec<ExportedAgentDefinition>,
    pub integrations: Vec<ExportedIntegrationDeclaration>,
    pub connections: Vec<ExportedConnectionDeclaration>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExportedUserPreference {
    pub category: String,
    pub preference_key: String,
    pub value: Value,
    pub is_sensitive: bool,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub authority_disclaimer: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportedCapabilityGrant {
    pub agent_key: String,
    pub integration_key: String,
    pub capability_key: String,
    pub state: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExportedPreferences {
    pub preferences: Vec<ExportedUserPreference>,
    pub capability_grants: Vec<ExportedCapabilityGrant>,
    pub disclaimer: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportedSpan {
    pub id: Uuid,
    pub title: String,
    pub category: String,
    pub status: String,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExportedExecution {
    pub id: Uuid,
    pub capability_key: String,
    pub state: String,
    pub provider_reference: Option<String>,
    pub confirmation_evidence: Option<Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExportedAuditEvent {
    pub cursor_id: i64,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub summary_details: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExportedSpans {
    pub spans: Vec<ExportedSpan>,
    pub executions: Vec<ExportedExecution>,
    pub audit_events: Vec<ExportedAuditEvent>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PortableExportBundle {
    pub export_id: Uuid,
    pub schema_version: String,
    pub generated_at: DateTime<Utc>,
    pub categories: Vec<String>,
    pub disclosure: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ExportedConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferences: Option<ExportedPreferences>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spans: Option<ExportedSpans>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PortableExportResponse {
    pub export_id: Uuid,
    pub download_url: String,
    pub categories: Vec<String>,
    pub generated_at: DateTime<Utc>,
    pub disclosure: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImportResult {
    pub imported_preferences_count: usize,
    pub imported_connections_count: usize,
    pub status: String,
    pub disclosure: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HistoricalActionEvidence {
    pub execution_id: Uuid,
    pub proposal_id: Option<Uuid>,
    pub capability_external_key: String,
    pub proposal_details: Option<Value>,
    pub state: String,
    pub provider_reference: Option<String>,
    pub confirmation_evidence: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub integration_state: String,
}

#[derive(Clone)]
pub struct PrivacyService {
    db: Db,
    policy: DeclaredRetentionPolicy,
}

#[derive(Debug, thiserror::Error)]
pub enum PrivacyError {
    #[error("privacy request invalid: {0}")]
    Invalid(String),
    #[error("export not found")]
    NotFound,
    #[error("unauthorized")]
    Unauthorized,
    #[error("prohibited export data detected: {0}")]
    ProhibitedData(String),
    #[error("privacy storage unavailable: {0}")]
    Database(#[from] sqlx::Error),
}

impl PrivacyService {
    pub fn new(db: Db, policy: Option<DeclaredRetentionPolicy>) -> Self {
        Self {
            db,
            policy: policy.unwrap_or_default(),
        }
    }

    pub fn retention_policy(&self) -> &DeclaredRetentionPolicy {
        &self.policy
    }

    /// Deletes platform-controlled task history and conversation records for the authenticated user context.
    /// Preserves audit records for mandatory regulatory/legal hold.
    /// Discloses external service records, remote operators, audit hold, and 30-day backups.
    pub async fn delete_task_history(
        &self,
        context: &ResolvedUserContext,
        delete_conversations: bool,
    ) -> Result<DeleteHistoryResult, PrivacyError> {
        let mut tx = self.db.pool().begin().await?;

        // 1. Delete associated jobs for spans belonging to user
        let deleted_jobs = sqlx::query("DELETE FROM jobs WHERE user_context_id = $1")
            .bind(context.id.0)
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;

        // 2. Delete spans belonging to user
        let deleted_spans = sqlx::query("DELETE FROM spans WHERE user_context_id = $1")
            .bind(context.id.0)
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;

        let mut deleted_convs = 0;
        if delete_conversations {
            // Delete messages in user's conversations
            sqlx::query(
                "DELETE FROM messages WHERE conversation_id IN (
                    SELECT id FROM conversations WHERE user_context_id = $1
                )",
            )
            .bind(context.id.0)
            .execute(&mut *tx)
            .await?;

            // Delete conversations
            deleted_convs = sqlx::query("DELETE FROM conversations WHERE user_context_id = $1")
                .bind(context.id.0)
                .execute(&mut *tx)
                .await?
                .rows_affected() as usize;
        }

        // Note: audit_events are preserved for the mandatory legal hold window (audit_retention_days).
        // Under FR-AUD-009 & FR-DAT-006, audit events maintain evidence of actor, action, and outcome,
        // while the user's active task entries and conversation turns are removed.

        tx.commit().await?;

        tracing::info!(
            user_id = %context.user_id.0,
            user_context_id = %context.id.0,
            deleted_spans,
            deleted_jobs,
            deleted_convs,
            "Executed platform-controlled task history deletion"
        );

        Ok(DeleteHistoryResult {
            deleted_spans_count: deleted_spans,
            deleted_conversations_count: deleted_convs,
            disclosure: self.policy.external_limitations_disclosure.clone(),
        })
    }

    /// Prune finished actionable spans and jobs older than the declared retention policy cutoff.
    /// Honors mandatory audit retention hold (does not touch audit events younger than audit_retention_days).
    pub async fn apply_retention_prune(
        &self,
        now: DateTime<Utc>,
    ) -> Result<PruneResult, PrivacyError> {
        let task_cutoff = now - Duration::days(self.policy.task_history_retention_days as i64);
        let audit_cutoff = now - Duration::days(self.policy.audit_retention_days as i64);

        let mut tx = self.db.pool().begin().await?;

        // Prune completed/failed/cancelled jobs older than task_cutoff
        let pruned_jobs =
            sqlx::query("DELETE FROM jobs WHERE completed_at IS NOT NULL AND completed_at < $1")
                .bind(task_cutoff)
                .execute(&mut *tx)
                .await?
                .rows_affected() as usize;

        // Prune finished actionable spans older than task_cutoff; lived history stays
        let pruned_spans = sqlx::query(
            "DELETE FROM spans WHERE execution_type IS NOT NULL AND status IN ('done', 'failed', 'cancelled') AND updated_at < $1",
        )
        .bind(task_cutoff)
        .execute(&mut *tx)
        .await?
        .rows_affected() as usize;

        // Prune inactive conversations older than task_cutoff
        let pruned_convs = sqlx::query("DELETE FROM conversations WHERE updated_at < $1")
            .bind(task_cutoff)
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;

        // Prune audit events strictly older than the legal audit hold window
        sqlx::query("DELETE FROM audit_events WHERE occurred_at < $1")
            .bind(audit_cutoff)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;

        Ok(PruneResult {
            pruned_spans_count: pruned_spans,
            pruned_conversations_count: pruned_convs,
            pruned_jobs_count: pruned_jobs,
            retention_cutoff: task_cutoff,
            audit_hold_cutoff: audit_cutoff,
        })
    }

    /// Generates a documented portable export bundle for the selected categories.
    /// Strictly excludes credentials, API tokens, passwords, active action proposals, and active approvals.
    pub async fn generate_portable_export(
        &self,
        context: &ResolvedUserContext,
        categories: &[String],
        now: DateTime<Utc>,
    ) -> Result<PortableExportBundle, PrivacyError> {
        if categories.is_empty() {
            return Err(PrivacyError::Invalid(
                "at least one category required".into(),
            ));
        }

        let export_id = Uuid::new_v4();
        let mut bundle = PortableExportBundle {
            export_id,
            schema_version: "1.0".to_string(),
            generated_at: now,
            categories: categories.to_vec(),
            disclosure: self.policy.external_limitations_disclosure.clone(),
            config: None,
            preferences: None,
            spans: None,
        };

        for cat in categories {
            match cat.as_str() {
                "config" => {
                    let config = self.export_config(context).await?;
                    bundle.config = Some(config);
                }
                "preferences" => {
                    let preferences = self.export_preferences(context).await?;
                    bundle.preferences = Some(preferences);
                }
                "spans" => {
                    let spans = self.export_spans(context).await?;
                    bundle.spans = Some(spans);
                }
                other => {
                    return Err(PrivacyError::Invalid(format!("unknown category: {other}")));
                }
            }
        }

        // Run automated canary scan: ensure no secrets, passwords, tokens, or active approvals leaked
        let serialized = serde_json::to_value(&bundle)
            .map_err(|e| PrivacyError::Invalid(format!("serialization error: {e}")))?;
        Self::scan_for_prohibited_content(&serialized)?;

        // Store export bundle metadata in database so it can be downloaded via get_export
        let bundle_json = serde_json::to_string(&bundle)
            .map_err(|e| PrivacyError::Invalid(format!("export encoding error: {e}")))?;

        let expires_at = now + Duration::hours(24);
        sqlx::query(
            "INSERT INTO portable_exports (id, user_context_id, categories, bundle_data, created_at, expires_at)
             VALUES ($1, $2, $3, $4::jsonb, $5, $6)",
        )
        .bind(export_id)
        .bind(context.id.0)
        .bind(categories)
        .bind(&bundle_json)
        .bind(now)
        .bind(expires_at)
        .execute(self.db.pool())
        .await?;

        Ok(bundle)
    }

    pub async fn get_export(
        &self,
        context: &ResolvedUserContext,
        export_id: Uuid,
    ) -> Result<PortableExportBundle, PrivacyError> {
        let row = sqlx::query(
            "SELECT bundle_data FROM portable_exports WHERE id = $1 AND user_context_id = $2 AND expires_at > now()",
        )
        .bind(export_id)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(PrivacyError::NotFound)?;

        let bundle_data: Value = row.get("bundle_data");
        serde_json::from_value(bundle_data)
            .map_err(|_| PrivacyError::Invalid("invalid export format".into()))
    }

    /// Imports portable preferences and configuration.
    /// FR-PRT-006: All imported connections require fresh authorization on destination deployment.
    /// Credentials and active approvals are strictly prohibited.
    pub async fn import_portable_data(
        &self,
        context: &ResolvedUserContext,
        bundle: &PortableExportBundle,
    ) -> Result<ImportResult, PrivacyError> {
        let raw_value = serde_json::to_value(bundle)
            .map_err(|e| PrivacyError::Invalid(format!("parse error: {e}")))?;
        Self::scan_for_prohibited_content(&raw_value)?;

        let mut tx = self.db.pool().begin().await?;
        let now = Utc::now();
        let mut imported_prefs = 0;
        let mut imported_conns = 0;

        // 1. Import preferences if present
        if let Some(ref prefs) = bundle.preferences {
            for p in &prefs.preferences {
                let is_sensitive = p.is_sensitive;
                let confirmed_at = if is_sensitive { Some(now) } else { None };

                sqlx::query(
                    r#"
                    INSERT INTO user_preferences (
                        user_context_id, category, preference_key, value, is_sensitive, confirmed_at, created_at, updated_at
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
                    ON CONFLICT (user_context_id, preference_key) DO UPDATE SET
                        category = EXCLUDED.category,
                        value = EXCLUDED.value,
                        is_sensitive = EXCLUDED.is_sensitive,
                        confirmed_at = EXCLUDED.confirmed_at,
                        updated_at = EXCLUDED.updated_at
                    "#,
                )
                .bind(context.id.0)
                .bind(&p.category)
                .bind(&p.preference_key)
                .bind(&p.value)
                .bind(is_sensitive)
                .bind(confirmed_at)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                imported_prefs += 1;
            }
        }

        // 2. Import connections if present (FR-PRT-006: force authorization_state = 'pending' / requiring renewed authorization)
        if let Some(ref cfg) = bundle.config {
            for c in &cfg.connections {
                // Find integration definition id by key
                let integration_id = sqlx::query_scalar::<_, Uuid>(
                    "SELECT id FROM integration_definitions WHERE deployment_id=$1 AND external_key = $2 LIMIT 1",
                )
                .bind(context.subject.deployment_id.0)
                .bind(&c.integration_key)
                .fetch_optional(&mut *tx)
                .await?;

                if let Some(int_id) = integration_id {
                    let display_str = c.account_display_id.as_deref().unwrap_or("default");
                    let mut hasher = Sha256::new();
                    hasher.update(display_str.as_bytes());
                    let account_hash = hasher.finalize().to_vec();

                    sqlx::query(
                        r#"
                        INSERT INTO external_connections (
                            user_context_id, integration_id, external_account_hash, account_display_id,
                            credential_custody, authorization_state, authorized_capabilities, created_at, updated_at
                        ) VALUES ($1, $2, $3, $4, $5, 'pending', '{}'::text[], $6, $6)
                        ON CONFLICT (user_context_id, integration_id, external_account_hash) DO NOTHING
                        "#,
                    )
                    .bind(context.id.0)
                    .bind(int_id)
                    .bind(account_hash)
                    .bind(&c.account_display_id)
                    .bind(&c.credential_custody)
                    .bind(now)
                    .execute(&mut *tx)
                    .await?;
                    imported_conns += 1;
                }
            }
        }

        tx.commit().await?;

        Ok(ImportResult {
            imported_preferences_count: imported_prefs,
            imported_connections_count: imported_conns,
            status: "Imported successfully. All imported connections require fresh user authorization before use.".to_string(),
            disclosure: self.policy.external_limitations_disclosure.clone(),
        })
    }

    /// Returns historical action evidence for a completed or past execution,
    /// proving evidence remains completely interpretable after an integration or extension is removed.
    pub async fn get_historical_action_evidence(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
    ) -> Result<HistoricalActionEvidence, PrivacyError> {
        let row = sqlx::query(
            r#"
            SELECT
                e.id AS execution_id,
                e.proposal_id,
                e.state,
                e.provider_reference,
                e.confirmation_evidence,
                e.created_at,
                e.updated_at,
                p.capability,
                p.details AS proposal_details
            FROM executions e
            LEFT JOIN action_proposals p ON p.id = e.proposal_id
            WHERE e.id = $1 AND e.user_context_id = $2
            "#,
        )
        .bind(execution_id)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(PrivacyError::NotFound)?;

        let capability: String = row.get("capability");
        let proposal_details: Option<Value> = row.get("proposal_details");

        // Inspect lifecycle state of integration if known
        let integration_state = if let Some(ref details) = proposal_details {
            if let Some(ext_id_str) = details.get("remote_extension_id").and_then(|v| v.as_str()) {
                if let Ok(ext_id) = Uuid::parse_str(ext_id_str) {
                    sqlx::query_scalar::<_, String>(
                        "SELECT lifecycle_state FROM remote_extensions WHERE id = $1",
                    )
                    .bind(ext_id)
                    .fetch_optional(self.db.pool())
                    .await?
                    .unwrap_or_else(|| "removed".to_string())
                } else {
                    "historical".to_string()
                }
            } else {
                "historical".to_string()
            }
        } else {
            "historical".to_string()
        };

        Ok(HistoricalActionEvidence {
            execution_id,
            proposal_id: row.get("proposal_id"),
            capability_external_key: capability,
            proposal_details,
            state: row.get("state"),
            provider_reference: row.get("provider_reference"),
            confirmation_evidence: row.get("confirmation_evidence"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            integration_state,
        })
    }

    // Helper functions for building partitioned exports
    async fn export_config(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<ExportedConfig, PrivacyError> {
        // 1. Agent definitions
        let agent_rows = sqlx::query(
            "SELECT external_key, purpose, requested_capability_categories FROM agent_definitions WHERE deployment_id=$1 AND state='enabled'",
        )
        .bind(context.subject.deployment_id.0)
        .fetch_all(self.db.pool())
        .await?;

        let agents = agent_rows
            .into_iter()
            .map(|r| ExportedAgentDefinition {
                external_key: r.get("external_key"),
                display_name: r.get("external_key"),
                description: r.get("purpose"),
                requested_capabilities: r.get("requested_capability_categories"),
            })
            .collect();

        // 2. Integration declarations (declarations only - no secrets)
        let int_rows = sqlx::query(
            "SELECT external_key, display_name, protocol FROM integration_definitions WHERE deployment_id=$1 AND state='enabled'",
        )
        .bind(context.subject.deployment_id.0)
        .fetch_all(self.db.pool())
        .await?;

        let integrations = int_rows
            .into_iter()
            .map(|r| ExportedIntegrationDeclaration {
                external_key: r.get("external_key"),
                display_name: r.get("display_name"),
                protocol: r.get("protocol"),
                capabilities: vec![],
            })
            .collect();

        // 3. User connections (account references only - strictly NO credentials)
        let conn_rows = sqlx::query(
            r#"
            SELECT i.external_key, c.account_display_id, c.credential_custody
            FROM external_connections c
            JOIN integration_definitions i ON i.id = c.integration_id
            WHERE c.user_context_id = $1
            "#,
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;

        let connections = conn_rows
            .into_iter()
            .map(|r| ExportedConnectionDeclaration {
                integration_key: r.get("external_key"),
                account_display_id: r.get("account_display_id"),
                credential_custody: r.get("credential_custody"),
                requires_renewed_authorization: true,
            })
            .collect();

        Ok(ExportedConfig {
            agents,
            integrations,
            connections,
        })
    }

    async fn export_preferences(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<ExportedPreferences, PrivacyError> {
        let pref_rows = sqlx::query(
            r#"
            SELECT category, preference_key, value, is_sensitive, confirmed_at
            FROM user_preferences
            WHERE user_context_id = $1
            ORDER BY category ASC, preference_key ASC
            "#,
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;

        let preferences = pref_rows
            .into_iter()
            .map(|r| ExportedUserPreference {
                category: r.get("category"),
                preference_key: r.get("preference_key"),
                value: r.get("value"),
                is_sensitive: r.get("is_sensitive"),
                confirmed_at: r.get("confirmed_at"),
                authority_disclaimer: PREFERENCE_AUTHORITY_DISCLAIMER.to_string(),
            })
            .collect();

        let grant_rows = sqlx::query(
            r#"
            SELECT a.external_key AS agent_key, i.external_key AS integration_key,
                   g.capability_external_key AS capability_key, g.state
            FROM agent_capability_grants g
            JOIN agent_definitions a ON a.id = g.agent_definition_id
            JOIN external_connections conn ON conn.id = g.connection_id
            JOIN integration_definitions i ON i.id = conn.integration_id
            WHERE g.user_context_id = $1
            "#,
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;

        let capability_grants = grant_rows
            .into_iter()
            .map(|r| ExportedCapabilityGrant {
                agent_key: r.get("agent_key"),
                integration_key: r.get("integration_key"),
                capability_key: r.get("capability_key"),
                state: r.get("state"),
            })
            .collect();

        Ok(ExportedPreferences {
            preferences,
            capability_grants,
            disclaimer: PREFERENCE_AUTHORITY_DISCLAIMER.to_string(),
        })
    }

    async fn export_spans(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<ExportedSpans, PrivacyError> {
        let span_rows = sqlx::query(
            "SELECT id, title, category, status, start_at, end_at, created_at FROM spans WHERE user_context_id = $1 ORDER BY created_at DESC",
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;

        let spans = span_rows
            .into_iter()
            .map(|r| ExportedSpan {
                id: r.get("id"),
                title: r.get("title"),
                category: r.get("category"),
                status: r.get("status"),
                start_at: r.get("start_at"),
                end_at: r.get("end_at"),
                created_at: r.get("created_at"),
            })
            .collect();

        let exec_rows = sqlx::query(
            r#"
            SELECT e.id, p.capability, e.state, e.provider_reference, e.confirmation_evidence, e.created_at
            FROM executions e
            LEFT JOIN action_proposals p ON p.id = e.proposal_id
            WHERE e.user_context_id = $1
            ORDER BY e.created_at DESC LIMIT 500
            "#,
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;

        let executions = exec_rows
            .into_iter()
            .map(|r| ExportedExecution {
                id: r.get("id"),
                capability_key: r.get::<Option<String>, _>("capability").unwrap_or_default(),
                state: r.get("state"),
                provider_reference: r.get("provider_reference"),
                confirmation_evidence: r.get("confirmation_evidence"),
                created_at: r.get("created_at"),
            })
            .collect();

        let audit_rows = sqlx::query(
            "SELECT cursor_id, event_type, occurred_at, details FROM audit_events WHERE user_context_id = $1 ORDER BY cursor_id DESC LIMIT 500",
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;

        let audit_events = audit_rows
            .into_iter()
            .map(|r| ExportedAuditEvent {
                cursor_id: r.get("cursor_id"),
                event_type: r.get("event_type"),
                occurred_at: r.get("occurred_at"),
                summary_details: r.get("details"),
            })
            .collect();

        Ok(ExportedSpans {
            spans,
            executions,
            audit_events,
        })
    }

    /// Recursive canary scan preventing credentials, secret tokens, passwords, or active approvals from escaping.
    pub fn scan_for_prohibited_content(value: &Value) -> Result<(), PrivacyError> {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    let lower = k.to_ascii_lowercase();
                    // Prohibited key patterns
                    if lower.contains("secret")
                        || lower.contains("password")
                        || lower.contains("token")
                        || (lower.contains("credential") && k != "credential_custody")
                        || lower.contains("private_key")
                        || lower.contains("session_id")
                        || lower.contains("cvv")
                        || lower.contains("card_number")
                        || lower.contains("pin")
                        || (lower.contains("approval") && !lower.contains("summary"))
                    {
                        return Err(PrivacyError::ProhibitedData(format!(
                            "prohibited key '{k}' detected in export"
                        )));
                    }
                    Self::scan_for_prohibited_content(v)?;
                }
            }
            Value::Array(arr) => {
                for v in arr {
                    Self::scan_for_prohibited_content(v)?;
                }
            }
            Value::String(s) => {
                let lower = s.to_ascii_lowercase();
                if lower.contains("sk_live_")
                    || lower.contains("vox_sk_")
                    || lower.contains("bearer ")
                    || lower.contains("-----begin")
                    || lower.contains("private key-----")
                    || lower
                        .split(|c: char| {
                            c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == ';'
                        })
                        .any(|word| {
                            word.starts_with("sk-")
                                || word.starts_with("eyj")
                                || word.starts_with("key-")
                        })
                {
                    return Err(PrivacyError::ProhibitedData(
                        "credential or token value detected in export payload".into(),
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }
}
