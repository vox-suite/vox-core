use crate::{db::Db, execution_policy::{ExecutionIdentity, ExecutionPolicyService, ExecutionRequest}, identity::ResolvedUserContext};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize)]
pub struct StartExecutionRequest { pub approval_id: Uuid, pub idempotency_key: String }
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Execution { pub id: Uuid, pub state: String, pub provider_reference: Option<String>, pub confirmation_evidence: Option<Value> }
#[derive(Clone, Debug)]
pub struct AdapterRequest { pub execution_id: Uuid, pub idempotency_key: String, pub identity: ExecutionIdentity, pub capability_external_key: String }
#[derive(Clone, Debug)]
pub enum AdapterOutcome { Succeeded { provider_reference: String, evidence: Value }, Failed { code: String }, Cancelled { provider_reference: Option<String>, evidence: Value }, AwaitingProviderAuthentication { provider_reference: Option<String> }, Reconciling { provider_reference: Option<String> }, Unknown { provider_reference: Option<String>, code: String } }
#[async_trait::async_trait]
pub trait ExecutionAdapter: Send + Sync { async fn dispatch(&self, request: AdapterRequest) -> AdapterOutcome; async fn reconcile(&self, request: AdapterRequest, provider_reference: Option<&str>) -> AdapterOutcome; }
#[derive(Clone)] pub struct ExecutionCoordinator { db: Db }
#[derive(Debug, thiserror::Error)] pub enum ExecutionError { #[error("execution request invalid")] Invalid, #[error("execution is unavailable")] Unavailable, #[error("execution requires a fresh approval")] FreshApproval, #[error("execution storage unavailable")] Database(#[from] sqlx::Error) }

impl ExecutionCoordinator {
pub fn new(db: Db) -> Self { Self { db } }
 pub async fn start(&self, context: &ResolvedUserContext, request: StartExecutionRequest, now: DateTime<Utc>) -> Result<Execution, ExecutionError> {
  let key=request.idempotency_key.trim(); if key.is_empty() || key.len()>255 { return Err(ExecutionError::Invalid); }
  let mut tx=self.db.pool().begin().await?;
  if let Some(row)=sqlx::query("SELECT id,state,provider_reference,confirmation_evidence FROM executions WHERE user_context_id=$1 AND idempotency_key=$2 FOR UPDATE").bind(context.id.0).bind(key).fetch_optional(&mut *tx).await? { let result=row_execution(row)?; tx.commit().await?; return Ok(result); }
  let row=sqlx::query("SELECT a.proposal_id,a.consumed_attempt_id,p.details,p.details_hash,p.expires_at,p.state,p.capability_external_key,p.agent_definition_id FROM action_approvals a JOIN action_proposals p ON p.id=a.proposal_id WHERE a.id=$1 AND a.user_context_id=$2 FOR UPDATE OF a,p").bind(request.approval_id).bind(context.id.0).fetch_optional(&mut *tx).await?.ok_or(ExecutionError::Unavailable)?;
  if row.get::<Option<Uuid>,_>("consumed_attempt_id").is_some() || row.get::<String,_>("state")!="approved" || row.get::<DateTime<Utc>,_>("expires_at")<=now { return Err(ExecutionError::FreshApproval); }
  let details:Value=row.get("details"); let identity:ExecutionIdentity=serde_json::from_value(details.get("execution").cloned().ok_or(ExecutionError::FreshApproval)?).map_err(|_|ExecutionError::FreshApproval)?;
  let capability:String=row.get("capability_external_key");
  let connection_ok=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM agent_capability_grants g JOIN external_connections x ON x.id=g.connection_id JOIN agent_definitions a ON a.id=g.agent_definition_id WHERE g.user_context_id=$1 AND a.id=$2 AND g.capability_external_key=$3 AND g.connection_id=$4 AND g.state='enabled' AND x.authorization_state='authorized' AND (x.expires_at IS NULL OR x.expires_at>$5))").bind(context.id.0).bind(row.get::<Uuid,_>("agent_definition_id")).bind(&capability).bind(identity.connection_id).bind(now).fetch_one(&mut *tx).await?; if !connection_ok{return Err(ExecutionError::Unavailable);}
  let id=Uuid::new_v4(); let proposal_id:Uuid=row.get("proposal_id"); let integration=capability.split('.').next().ok_or(ExecutionError::Invalid)?;
  // The policy service owns immutable policy evidence and exact quota reservation.
  // It is evaluated before the external boundary; any later failure stays internal.
  tx.commit().await?;
  let decision=ExecutionPolicyService::new(self.db.clone()).evaluate(context,ExecutionRequest{approval_id:request.approval_id,attempt_id:id,execution:identity.clone()},now).await.map_err(|_|ExecutionError::Unavailable)?;
  let mut tx=self.db.pool().begin().await?;
  let row=sqlx::query("SELECT a.proposal_id,a.consumed_attempt_id,p.state FROM action_approvals a JOIN action_proposals p ON p.id=a.proposal_id WHERE a.id=$1 AND a.user_context_id=$2 FOR UPDATE OF a,p").bind(request.approval_id).bind(context.id.0).fetch_optional(&mut *tx).await?.ok_or(ExecutionError::Unavailable)?;
  if row.get::<Option<Uuid>,_>("consumed_attempt_id").is_some() || row.get::<String,_>("state")!="approved" { return Err(ExecutionError::FreshApproval); }
  sqlx::query("INSERT INTO executions (id,user_context_id,approval_id,proposal_id,idempotency_key,integration_external_key,capability_external_key,connection_id,execution_identity,policy_snapshot,state) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'pending')").bind(id).bind(context.id.0).bind(request.approval_id).bind(proposal_id).bind(key).bind(integration).bind(&capability).bind(identity.connection_id).bind(serde_json::to_value(&identity).map_err(|_|ExecutionError::Invalid)?).bind(decision.policy_snapshot).execute(&mut *tx).await?;
  sqlx::query("INSERT INTO execution_attempts (execution_id) VALUES ($1)").bind(id).execute(&mut *tx).await?;
  sqlx::query("UPDATE action_approvals SET consumed_attempt_id=$2,consumed_at=$3 WHERE id=$1").bind(request.approval_id).bind(id).bind(now).execute(&mut *tx).await?;
  sqlx::query("UPDATE action_proposals SET state='consumed',updated_at=$2 WHERE id=$1").bind(proposal_id).bind(now).execute(&mut *tx).await?;
  tx.commit().await?; Ok(Execution{id,state:"pending".into(),provider_reference:None,confirmation_evidence:None})
 }
 pub async fn dispatch(&self, context:&ResolvedUserContext, execution_id:Uuid, adapter:&dyn ExecutionAdapter, now:DateTime<Utc>)->Result<Execution,ExecutionError>{
  let request=self.adapter_request(context,execution_id).await?;
  self.record_outcome(context,execution_id,AdapterOutcome::Reconciling{provider_reference:None},now).await?;
  let outcome=adapter.dispatch(request).await;
  self.record_outcome(context,execution_id,outcome,now).await
 }
 pub async fn reconcile(&self, context:&ResolvedUserContext, execution_id:Uuid, adapter:&dyn ExecutionAdapter, now:DateTime<Utc>)->Result<Execution,ExecutionError>{
  let request=self.adapter_request(context,execution_id).await?;
  let row=sqlx::query("SELECT state,provider_reference FROM executions WHERE id=$1 AND user_context_id=$2").bind(execution_id).bind(context.id.0).fetch_optional(self.db.pool()).await?.ok_or(ExecutionError::Unavailable)?;
  let state:String=row.get("state"); if !matches!(state.as_str(),"unknown"|"reconciling"|"awaiting_provider_authentication"){return Err(ExecutionError::Unavailable);}
  let reference:Option<String>=row.get("provider_reference");
  self.record_reconciled_outcome(context,execution_id,adapter.reconcile(request,reference.as_deref()).await,now).await
 }
 async fn adapter_request(&self,context:&ResolvedUserContext,execution_id:Uuid)->Result<AdapterRequest,ExecutionError>{let row=sqlx::query("SELECT idempotency_key,execution_identity,capability_external_key FROM executions WHERE id=$1 AND user_context_id=$2").bind(execution_id).bind(context.id.0).fetch_optional(self.db.pool()).await?.ok_or(ExecutionError::Unavailable)?;Ok(AdapterRequest{execution_id,idempotency_key:row.get::<String,_>("idempotency_key"),identity:serde_json::from_value(row.get::<Value,_>("execution_identity")).map_err(|_|ExecutionError::Invalid)?,capability_external_key:row.get::<String,_>("capability_external_key")})}
 pub async fn record_outcome(&self, context:&ResolvedUserContext, execution_id:Uuid, outcome:AdapterOutcome, now:DateTime<Utc>)->Result<Execution,ExecutionError>{self.persist_outcome(context,execution_id,outcome,now,false).await}
 async fn record_reconciled_outcome(&self, context:&ResolvedUserContext, execution_id:Uuid, outcome:AdapterOutcome, now:DateTime<Utc>)->Result<Execution,ExecutionError>{self.persist_outcome(context,execution_id,outcome,now,true).await}
 async fn persist_outcome(&self, context:&ResolvedUserContext, execution_id:Uuid, outcome:AdapterOutcome, now:DateTime<Utc>, allow_unknown_transition:bool)->Result<Execution,ExecutionError>{
  let (state,reference,evidence,code,done)=match outcome { AdapterOutcome::Succeeded{provider_reference,evidence} if confirmation_evidence_is_present(&evidence)=> ("succeeded",Some(provider_reference),Some(evidence),None,true), AdapterOutcome::Succeeded{..}=>return Err(ExecutionError::Invalid), AdapterOutcome::Failed{code}=>("failed",None,None,Some(code),true), AdapterOutcome::Cancelled{provider_reference,evidence} if confirmation_evidence_is_present(&evidence)=>("cancelled",provider_reference,Some(evidence),None,true), AdapterOutcome::Cancelled{..}=>return Err(ExecutionError::Invalid), AdapterOutcome::AwaitingProviderAuthentication{provider_reference}=>("awaiting_provider_authentication",provider_reference,None,None,false), AdapterOutcome::Reconciling{provider_reference}=>("reconciling",provider_reference,None,None,false), AdapterOutcome::Unknown{provider_reference,code}=>("unknown",provider_reference,None,Some(code),true)};
  let row=sqlx::query("UPDATE executions SET state=$1,provider_reference=COALESCE($2,provider_reference),confirmation_evidence=$3,error_code=$4,updated_at=$5,completed_at=CASE WHEN $6 THEN $5 ELSE NULL END WHERE id=$7 AND user_context_id=$8 AND state NOT IN ('succeeded','failed','cancelled','expired') AND (($9 AND state IN ('unknown','reconciling','awaiting_provider_authentication')) OR (NOT $9 AND state <> 'unknown')) RETURNING id,state,provider_reference,confirmation_evidence").bind(state).bind(reference).bind(evidence).bind(code).bind(now).bind(done).bind(execution_id).bind(context.id.0).bind(allow_unknown_transition).fetch_optional(self.db.pool()).await?.ok_or(ExecutionError::Unavailable)?; row_execution(row)
 }
}
fn row_execution(row:sqlx::postgres::PgRow)->Result<Execution,ExecutionError>{Ok(Execution{id:row.try_get("id")?,state:row.try_get("state")?,provider_reference:row.try_get("provider_reference")?,confirmation_evidence:row.try_get("confirmation_evidence")?})}
fn confirmation_evidence_is_present(evidence:&Value)->bool{evidence.as_object().is_some_and(|object|!object.is_empty())}

#[cfg(test)]
mod tests {
 use super::confirmation_evidence_is_present;
 #[test]
 fn confirmation_evidence_must_be_a_non_empty_object(){assert!(!confirmation_evidence_is_present(&serde_json::json!(null)));assert!(!confirmation_evidence_is_present(&serde_json::json!({})));assert!(!confirmation_evidence_is_present(&serde_json::json!("receipt")));assert!(confirmation_evidence_is_present(&serde_json::json!({"receipt":"synthetic"})));}
}
