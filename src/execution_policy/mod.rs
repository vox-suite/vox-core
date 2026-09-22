/**
 * Policy rules dictating auto-approval vs human-in-the-loop execution.
 */

use crate::{db::Db, identity::ResolvedUserContext};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

const MAX_KEY_BYTES: usize = 511;
const CURRENCY_BYTES: usize = 3;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionIdentity {
    pub provider_external_key: String,
    pub model_identifier: String,
    pub account_reference: String,
    pub connection_id: Uuid,
    pub price_amount_minor: i64,
    pub price_currency: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionRequest {
    pub approval_id: Uuid,
    pub attempt_id: Uuid,
    pub execution: ExecutionIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpendingPolicyRequest {
    pub capability_external_key: String,
    pub provider_external_key: Option<String>,
    pub currency: String,
    pub max_amount_minor: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationalQuotaRequest {
    pub provider_external_key: String,
    pub model_identifier: String,
    pub account_reference: String,
    pub connection_id: Uuid,
    pub max_attempts: i32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PolicyDecision {
    pub policy_snapshot: Value,
}

#[derive(Clone)]
pub struct ExecutionPolicyService {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionPolicyError {
    #[error("execution policy request is invalid")]
    Invalid,
    #[error("an approved proposal is required before policy evaluation")]
    ApprovalRequired,
    #[error("the execution differs from the approved proposal and needs a fresh proposal")]
    FreshProposalRequired,
    #[error("a spending policy blocks this execution")]
    SpendingPolicyExceeded,
    #[error("the exact operational quota is exhausted")]
    QuotaExhausted,
    #[error("execution policy storage is unavailable")]
    Database(#[from] sqlx::Error),
}

impl ExecutionPolicyService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn set_spending_policy(
        &self,
        context: &ResolvedUserContext,
        request: SpendingPolicyRequest,
    ) -> Result<(), ExecutionPolicyError> {
        let capability = key(&request.capability_external_key)?;
        let provider = request
            .provider_external_key
            .as_deref()
            .map(key)
            .transpose()?;
        let currency = currency(&request.currency)?;
        if request.max_amount_minor < 0 {
            return Err(ExecutionPolicyError::Invalid);
        }
        sqlx::query(
            "INSERT INTO spending_policies (user_context_id, capability_external_key, provider_external_key, currency, max_amount_minor) \
             VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (user_context_id, capability_external_key, provider_external_key, currency) \
             DO UPDATE SET max_amount_minor=EXCLUDED.max_amount_minor,policy_version=spending_policies.policy_version+1,updated_at=now()",
        )
        .bind(context.id.0)
        .bind(capability)
        .bind(provider)
        .bind(currency)
        .bind(request.max_amount_minor)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn set_operational_quota(
        &self,
        context: &ResolvedUserContext,
        request: OperationalQuotaRequest,
    ) -> Result<(), ExecutionPolicyError> {
        let identity = quota_identity(&request)?;
        let owns_connection = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM external_connections WHERE id=$1 AND user_context_id=$2)",
        )
        .bind(identity.connection_id)
        .bind(context.id.0)
        .fetch_one(self.db.pool())
        .await?;
        if !owns_connection {
            return Err(ExecutionPolicyError::Invalid);
        }
        sqlx::query(
            "INSERT INTO operational_quotas (user_context_id,provider_external_key,model_identifier,external_account_hash,connection_id,max_attempts) \
             VALUES ($1,$2,$3,$4,$5,$6) \
             ON CONFLICT (user_context_id,provider_external_key,model_identifier,external_account_hash,connection_id) \
             DO UPDATE SET max_attempts=EXCLUDED.max_attempts, reserved_attempts=LEAST(operational_quotas.reserved_attempts, EXCLUDED.max_attempts), policy_version=operational_quotas.policy_version+1, updated_at=now()",
        )
        .bind(context.id.0)
        .bind(identity.provider_external_key)
        .bind(identity.model_identifier)
        .bind(identity.account_hash)
        .bind(identity.connection_id)
        .bind(request.max_attempts)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn evaluate(
        &self,
        context: &ResolvedUserContext,
        request: ExecutionRequest,
        now: DateTime<Utc>,
    ) -> Result<PolicyDecision, ExecutionPolicyError> {
        let identity = execution_identity(&request.execution)?;
        let mut tx = self.db.pool().begin().await?;
        let proposal = sqlx::query(
            "SELECT a.proposal_id,a.consumed_attempt_id,a.proposal_hash,p.details,p.details_hash,p.expires_at,p.state,p.capability_external_key \
             FROM action_approvals a JOIN action_proposals p ON p.id=a.proposal_id \
             WHERE a.id=$1 AND a.user_context_id=$2 FOR UPDATE OF a,p",
        )
        .bind(request.approval_id)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ExecutionPolicyError::ApprovalRequired)?;
        if proposal
            .get::<Option<Uuid>, _>("consumed_attempt_id")
            .is_some()
            || proposal.get::<String, _>("state") != "approved"
            || proposal.get::<DateTime<Utc>, _>("expires_at") <= now
            || hash(&proposal.get::<Value, _>("details"))?
                != proposal.get::<Vec<u8>, _>("proposal_hash")
            || proposal.get::<Vec<u8>, _>("proposal_hash")
                != proposal.get::<Vec<u8>, _>("details_hash")
        {
            return Err(ExecutionPolicyError::ApprovalRequired);
        }
        let approved = proposal_execution(&proposal.get::<Value, _>("details"))?;
        if approved != identity {
            record_decision(
                &mut tx,
                context,
                request.approval_id,
                request.attempt_id,
                "fresh_proposal_required",
                serde_json::json!({"spending":[],"quota":null}),
            )
            .await?;
            tx.commit().await?;
            return Err(ExecutionPolicyError::FreshProposalRequired);
        }
        let capability: String = proposal.get("capability_external_key");
        let spending = sqlx::query(
            "SELECT id,policy_version,max_amount_minor,provider_external_key FROM spending_policies \
             WHERE user_context_id=$1 AND capability_external_key=$2 AND currency=$3 \
             AND (provider_external_key IS NULL OR provider_external_key=$4) FOR SHARE",
        )
        .bind(context.id.0)
        .bind(capability)
        .bind(&identity.price_currency)
        .bind(&identity.provider_external_key)
        .fetch_all(&mut *tx)
        .await?;
        let ceiling = spending
            .iter()
            .map(|row| row.get::<i64, _>("max_amount_minor"))
            .min();
        let spending_snapshot = Value::Array(
            spending
                .iter()
                .map(|row| serde_json::json!({
                    "id": row.get::<Uuid, _>("id"),
                    "version": row.get::<i64, _>("policy_version"),
                    "max_amount_minor": row.get::<i64, _>("max_amount_minor"),
                    "provider_external_key": row.get::<Option<String>, _>("provider_external_key"),
                }))
                .collect(),
        );
        if ceiling.is_some_and(|maximum| identity.price_amount_minor > maximum) {
            record_decision(
                &mut tx,
                context,
                request.approval_id,
                request.attempt_id,
                "spending_policy_exceeded",
                serde_json::json!({"spending":spending_snapshot,"quota":null}),
            )
            .await?;
            tx.commit().await?;
            return Err(ExecutionPolicyError::SpendingPolicyExceeded);
        }
        let quota = sqlx::query(
            "SELECT id,max_attempts,reserved_attempts,policy_version FROM operational_quotas \
             WHERE user_context_id=$1 AND provider_external_key=$2 AND model_identifier=$3 \
             AND external_account_hash=$4 AND connection_id=$5 FOR UPDATE",
        )
        .bind(context.id.0)
        .bind(&identity.provider_external_key)
        .bind(&identity.model_identifier)
        .bind(&identity.account_hash)
        .bind(identity.connection_id)
        .fetch_optional(&mut *tx)
        .await?;
        let quota_snapshot = quota.as_ref().map(|row| {
            serde_json::json!({
                "id": row.get::<Uuid, _>("id"),
                "version": row.get::<i64, _>("policy_version"),
                "max_attempts": row.get::<i32, _>("max_attempts"),
            })
        });
        if let Some(quota) = quota {
            let quota_id: Uuid = quota.get("id");
            let duplicate = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM operational_quota_reservations WHERE attempt_id=$1)",
            )
            .bind(request.attempt_id)
            .fetch_one(&mut *tx)
            .await?;
            if !duplicate {
                if quota.get::<i32, _>("reserved_attempts") >= quota.get::<i32, _>("max_attempts") {
                    record_decision(
                        &mut tx,
                        context,
                        request.approval_id,
                        request.attempt_id,
                        "quota_exhausted",
                        serde_json::json!({"spending":spending_snapshot,"quota":quota_snapshot}),
                    )
                    .await?;
                    tx.commit().await?;
                    return Err(ExecutionPolicyError::QuotaExhausted);
                }
                sqlx::query("UPDATE operational_quotas SET reserved_attempts=reserved_attempts+1,updated_at=$2 WHERE id=$1")
                    .bind(quota_id)
                    .bind(now)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("INSERT INTO operational_quota_reservations (quota_id,attempt_id,approval_id) VALUES ($1,$2,$3)")
                    .bind(quota_id)
                    .bind(request.attempt_id)
                    .bind(request.approval_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        let policy_snapshot =
            serde_json::json!({"spending":spending_snapshot,"quota":quota_snapshot});
        record_decision(
            &mut tx,
            context,
            request.approval_id,
            request.attempt_id,
            "constraints_satisfied",
            policy_snapshot.clone(),
        )
        .await?;
        tx.commit().await?;
        Ok(PolicyDecision { policy_snapshot })
    }
}

struct NormalizedExecutionIdentity {
    provider_external_key: String,
    model_identifier: String,
    account_hash: Vec<u8>,
    connection_id: Uuid,
    price_amount_minor: i64,
    price_currency: String,
}

impl PartialEq for NormalizedExecutionIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.provider_external_key == other.provider_external_key
            && self.model_identifier == other.model_identifier
            && self.account_hash == other.account_hash
            && self.connection_id == other.connection_id
            && self.price_amount_minor == other.price_amount_minor
            && self.price_currency == other.price_currency
    }
}

fn proposal_execution(
    details: &Value,
) -> Result<NormalizedExecutionIdentity, ExecutionPolicyError> {
    let execution = details
        .get("execution")
        .cloned()
        .ok_or(ExecutionPolicyError::FreshProposalRequired)?;
    serde_json::from_value(execution)
        .map_err(|_| ExecutionPolicyError::FreshProposalRequired)
        .and_then(|identity| execution_identity(&identity))
        .map_err(|_| ExecutionPolicyError::FreshProposalRequired)
}

fn execution_identity(
    identity: &ExecutionIdentity,
) -> Result<NormalizedExecutionIdentity, ExecutionPolicyError> {
    if identity.price_amount_minor < 0 {
        return Err(ExecutionPolicyError::Invalid);
    }
    Ok(NormalizedExecutionIdentity {
        provider_external_key: key(&identity.provider_external_key)?,
        model_identifier: key(&identity.model_identifier)?,
        account_hash: account_hash(&identity.account_reference)?,
        connection_id: identity.connection_id,
        price_amount_minor: identity.price_amount_minor,
        price_currency: currency(&identity.price_currency)?,
    })
}

struct QuotaIdentity {
    provider_external_key: String,
    model_identifier: String,
    account_hash: Vec<u8>,
    connection_id: Uuid,
}

fn quota_identity(
    request: &OperationalQuotaRequest,
) -> Result<QuotaIdentity, ExecutionPolicyError> {
    if request.max_attempts <= 0 {
        return Err(ExecutionPolicyError::Invalid);
    }
    Ok(QuotaIdentity {
        provider_external_key: key(&request.provider_external_key)?,
        model_identifier: key(&request.model_identifier)?,
        account_hash: account_hash(&request.account_reference)?,
        connection_id: request.connection_id,
    })
}

fn key(value: &str) -> Result<String, ExecutionPolicyError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_KEY_BYTES {
        Err(ExecutionPolicyError::Invalid)
    } else {
        Ok(value.to_owned())
    }
}

fn currency(value: &str) -> Result<String, ExecutionPolicyError> {
    let value = value.trim();
    if value.len() != CURRENCY_BYTES || !value.bytes().all(|byte| byte.is_ascii_uppercase()) {
        Err(ExecutionPolicyError::Invalid)
    } else {
        Ok(value.to_owned())
    }
}

fn account_hash(value: &str) -> Result<Vec<u8>, ExecutionPolicyError> {
    let value = key(value)?;
    Ok(Sha256::digest(value.as_bytes()).to_vec())
}

fn hash(value: &Value) -> Result<Vec<u8>, ExecutionPolicyError> {
    serde_json::to_vec(value)
        .map(|bytes| Sha256::digest(bytes).to_vec())
        .map_err(|_| ExecutionPolicyError::Invalid)
}

async fn record_decision(
    connection: &mut PgConnection,
    context: &ResolvedUserContext,
    approval_id: Uuid,
    attempt_id: Uuid,
    decision: &'static str,
    policy_snapshot: Value,
) -> Result<(), ExecutionPolicyError> {
    sqlx::query(
        "INSERT INTO execution_policy_decisions \
         (user_context_id,approval_id,attempt_id,decision,policy_snapshot) \
         VALUES ($1,$2,$3,$4,$5) ON CONFLICT (attempt_id) DO NOTHING",
    )
    .bind(context.id.0)
    .bind(approval_id)
    .bind(attempt_id)
    .bind(decision)
    .bind(policy_snapshot)
    .execute(connection)
    .await?;
    Ok(())
}
