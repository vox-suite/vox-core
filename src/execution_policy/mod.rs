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
        let capability = key(&request.capability_external_key)?;
        let provider = request
            .provider_external_key
            .as_deref()
            .map(key)
            .transpose()?
            .unwrap_or_else(|| "*".into());
        let currency = currency(&request.currency)?;
        if request.max_amount_minor < 0 {
            return Err(ExecutionPolicyError::Invalid);
        }
        sqlx::query(
            "INSERT INTO spending_policies (user_context_id,capability_external_key,provider_external_key,currency,max_amount_minor)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (user_context_id,capability_external_key,provider_external_key,currency)
             DO UPDATE SET max_amount_minor=EXCLUDED.max_amount_minor,version=spending_policies.version+1",
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
        if request.max_attempts <= 0 {
            return Err(ExecutionPolicyError::Invalid);
        }
        let owns_connection = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM connections WHERE id = $1 AND user_context_id = $2
             AND provider_key=$3 AND external_account_hash=$4)",
        )
        .bind(identity.connection_id)
        .bind(context.id.0)
        .bind(&identity.provider_external_key)
        .bind(hex::encode(&identity.account_hash))
        .fetch_one(self.db.pool())
        .await?;
        if !owns_connection {
            return Err(ExecutionPolicyError::Invalid);
        }
        sqlx::query(
            "INSERT INTO operational_quotas (user_context_id,provider_external_key,model_identifier,account_hash,connection_id,max_attempts)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (user_context_id,provider_external_key,model_identifier,account_hash,connection_id)
             DO UPDATE SET max_attempts=EXCLUDED.max_attempts,version=operational_quotas.version+1",
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
        let mut tx = self.db.pool().begin().await?;
        let decision = self
            .evaluate_in_transaction(context, request, now, &mut tx)
            .await?;
        tx.commit().await?;
        Ok(decision)
    }

    pub async fn evaluate_in_transaction(
        &self,
        context: &ResolvedUserContext,
        request: ExecutionRequest,
        now: DateTime<Utc>,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<PolicyDecision, ExecutionPolicyError> {
        let identity = execution_identity(&request.execution)?;
        let proposal = sqlx::query(
            "SELECT a.proposal_id, a.consumed_execution_id, a.approved_details_hash, p.details, p.details_hash, \
                    p.expires_at, p.state, p.capability \
             FROM action_approvals a JOIN action_proposals p ON p.id = a.proposal_id \
             WHERE a.id = $1 AND a.user_id = $2 FOR UPDATE OF a, p",
        )
        .bind(request.approval_id)
        .bind(context.user_id.0)
        .fetch_optional(&mut **tx)
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
            return Err(ExecutionPolicyError::FreshProposalRequired);
        }
        let spending = sqlx::query(
            "SELECT id, version, max_amount_minor FROM spending_policies
             WHERE user_context_id=$1 AND capability_external_key=$2
               AND provider_external_key IN ($3,'*') AND currency=$4 ORDER BY id FOR UPDATE",
        )
        .bind(context.id.0)
        .bind(proposal.get::<String, _>("capability"))
        .bind(&identity.provider_external_key)
        .bind(&identity.price_currency)
        .fetch_all(&mut **tx)
        .await?;
        if spending
            .iter()
            .any(|row| identity.price_amount_minor > row.get::<i64, _>("max_amount_minor"))
        {
            return Err(ExecutionPolicyError::SpendingPolicyExceeded);
        }
        let quota = sqlx::query(
            "SELECT id, version, max_attempts, reserved_attempts FROM operational_quotas
             WHERE user_context_id=$1 AND provider_external_key=$2 AND model_identifier=$3
               AND account_hash=$4 AND connection_id=$5 FOR UPDATE",
        )
        .bind(context.id.0)
        .bind(&identity.provider_external_key)
        .bind(&identity.model_identifier)
        .bind(&identity.account_hash)
        .bind(identity.connection_id)
        .fetch_optional(&mut **tx)
        .await?;
        let mut quota_snapshot = Vec::new();
        if let Some(row) = quota {
            let quota_id: Uuid = row.get("id");
            let prior_reservation = sqlx::query_scalar::<_, Uuid>(
                "SELECT quota_id FROM operational_quota_reservations WHERE attempt_id=$1 AND approval_id=$2",
            )
            .bind(request.attempt_id)
            .bind(request.approval_id)
            .fetch_optional(&mut **tx)
            .await?;
            if prior_reservation != Some(quota_id) {
                if prior_reservation.is_some()
                    || row.get::<i32, _>("reserved_attempts") >= row.get::<i32, _>("max_attempts")
                {
                    return Err(ExecutionPolicyError::QuotaExhausted);
                }
                sqlx::query("UPDATE operational_quotas SET reserved_attempts=reserved_attempts+1 WHERE id=$1")
                    .bind(quota_id)
                    .execute(&mut **tx)
                    .await?;
                sqlx::query("INSERT INTO operational_quota_reservations (attempt_id,quota_id,approval_id) VALUES ($1,$2,$3)")
                    .bind(request.attempt_id)
                    .bind(quota_id)
                    .bind(request.approval_id)
                    .execute(&mut **tx)
                    .await?;
            }
            quota_snapshot.push(serde_json::json!({"id":quota_id,"version":row.get::<i32,_>("version"),"max_attempts":row.get::<i32,_>("max_attempts")}));
        }
        let spending_snapshot: Vec<Value> = spending
            .iter()
            .map(|row| {
                serde_json::json!({
                    "id":row.get::<Uuid,_>("id"),"version":row.get::<i32,_>("version"),
                    "max_amount_minor":row.get::<i64,_>("max_amount_minor")
                })
            })
            .collect();
        Ok(PolicyDecision {
            policy_snapshot: serde_json::json!({"spending":spending_snapshot,"quota":quota_snapshot}),
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
    provider_external_key: String,
    model_identifier: String,
    account_hash: Vec<u8>,
}

fn quota_identity(
    request: &OperationalQuotaRequest,
) -> Result<QuotaIdentity, ExecutionPolicyError> {
    Ok(QuotaIdentity {
        connection_id: request.connection_id,
        provider_external_key: key(&request.provider_external_key)?,
        model_identifier: key(&request.model_identifier)?,
        account_hash: account_hash(&request.account_reference)?,
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
