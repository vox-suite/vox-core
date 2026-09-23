use crate::{db::Db, identity::ResolvedUserContext};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
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
        let _ = key(&request.capability_external_key)?;
        let _ = request
            .provider_external_key
            .as_deref()
            .map(key)
            .transpose()?;
        let _ = currency(&request.currency)?;
        if request.max_amount_minor < 0 {
            return Err(ExecutionPolicyError::Invalid);
        }
        let _ = context;
        Ok(())
    }

    pub async fn set_operational_quota(
        &self,
        context: &ResolvedUserContext,
        request: OperationalQuotaRequest,
    ) -> Result<(), ExecutionPolicyError> {
        let identity = quota_identity(&request)?;
        if request.max_attempts <= 0 {
            return Err(ExecutionPolicyError::Invalid);
        }
        let owns_connection = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM connections WHERE id = $1 AND user_id = $2)",
        )
        .bind(identity.connection_id)
        .bind(context.user_id.0)
        .fetch_one(self.db.pool())
        .await?;
        if !owns_connection {
            return Err(ExecutionPolicyError::Invalid);
        }
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
            "SELECT a.proposal_id, a.consumed_execution_id, a.approved_details_hash, p.details, p.details_hash, \
                    p.expires_at, p.state, p.capability \
             FROM action_approvals a JOIN action_proposals p ON p.id = a.proposal_id \
             WHERE a.id = $1 AND a.user_id = $2 FOR UPDATE OF a, p",
        )
        .bind(request.approval_id)
        .bind(context.user_id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ExecutionPolicyError::ApprovalRequired)?;
        if proposal
            .get::<Option<Uuid>, _>("consumed_execution_id")
            .is_some()
            || proposal.get::<String, _>("state") != "approved"
            || proposal.get::<DateTime<Utc>, _>("expires_at") <= now
            || hash(&proposal.get::<Value, _>("details"))?
                != proposal.get::<String, _>("approved_details_hash")
            || proposal.get::<String, _>("approved_details_hash")
                != proposal.get::<String, _>("details_hash")
        {
            return Err(ExecutionPolicyError::ApprovalRequired);
        }
        let approved = proposal_execution(&proposal.get::<Value, _>("details"))?;
        if approved != identity {
            tx.commit().await?;
            return Err(ExecutionPolicyError::FreshProposalRequired);
        }
        tx.commit().await?;
        Ok(PolicyDecision {
            policy_snapshot: serde_json::json!({}),
        })
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
    connection_id: Uuid,
}

fn quota_identity(
    request: &OperationalQuotaRequest,
) -> Result<QuotaIdentity, ExecutionPolicyError> {
    let _ = key(&request.provider_external_key)?;
    let _ = key(&request.model_identifier)?;
    let _ = account_hash(&request.account_reference)?;
    Ok(QuotaIdentity {
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

fn hash(value: &Value) -> Result<String, ExecutionPolicyError> {
    serde_json::to_vec(value)
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
        .map_err(|_| ExecutionPolicyError::Invalid)
}
