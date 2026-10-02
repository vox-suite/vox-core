//! Explicit specialist delegation; result access never creates connection grants.
use crate::{
    agent_registry::AgentRegistry,
    db::Db,
    durable_tasks::{
        DurableTaskError, DurableTaskService,
        runs::{AssignedRun, RunAuthority, capture_actor, current_capability},
    },
    identity::ResolvedUserContext,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone)]
pub struct DelegationService {
    db: Db,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionRequest {
    pub requester_agent_key: String,
    pub specialist_agent_key: String,
    pub scope: RunAuthority,
    pub parent_run_id: Option<Uuid>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct DelegateRequest {
    pub specialist_agent_key: String,
    pub brief: String,
    pub scope: RunAuthority,
    pub permission_id: Option<Uuid>,
}
impl DelegationService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
    pub async fn create_permission(
        &self,
        context: &ResolvedUserContext,
        r: PermissionRequest,
    ) -> Result<Uuid, DurableTaskError> {
        validate_scope(&r.scope)?;
        let registry = AgentRegistry::new(self.db.clone());
        let parent = registry
            .selected_for_context(context, &r.requester_agent_key)
            .await
            .map_err(|_| DurableTaskError::NotFound)?;
        let child = registry
            .selected_for_context(context, &r.specialist_agent_key)
            .await
            .map_err(|_| DurableTaskError::NotFound)?;
        if parent.definition.id == child.definition.id {
            return Err(DurableTaskError::Invalid);
        }
        let current = capture_actor(&self.db, context, Some(&r.specialist_agent_key)).await?;
        if !subset(&r.scope, &current.authority) {
            return Err(DurableTaskError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("SELECT id FROM user_contexts WHERE id=$1 FOR UPDATE")
            .bind(context.id.0)
            .execute(&mut *tx)
            .await?;
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM agent_delegation_permissions WHERE user_context_id=$1 AND state='enabled'").bind(context.id.0).fetch_one(&mut *tx).await?;
        if count >= 64 {
            return Err(DurableTaskError::Invalid);
        }
        if let Some(run) = r.parent_run_id {
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM assigned_task_runs WHERE job_id=$1 AND user_context_id=$2 AND agent_id=$3 AND parent_run_id IS NULL)").bind(run).bind(context.id.0).bind(parent.definition.id).fetch_one(&mut *tx).await?;
            if !valid {
                return Err(DurableTaskError::NotFound);
            }
        }
        let id=sqlx::query_scalar("INSERT INTO agent_delegation_permissions(user_context_id,requester_agent_id,specialist_agent_id,scope,parent_run_id,mode) VALUES($1,$2,$3,$4,$5,$6) RETURNING id")
   .bind(context.id.0).bind(parent.definition.id).bind(child.definition.id).bind(json!(r.scope)).bind(r.parent_run_id).bind(if r.parent_run_id.is_some(){"once"}else{"remembered"}).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(id)
    }
    pub async fn list(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<Value>, DurableTaskError> {
        Ok(sqlx::query_scalar("SELECT jsonb_build_object('id',p.id,'requester_agent_key',a.external_key,'specialist_agent_key',b.external_key,'scope',p.scope,'mode',p.mode,'parent_run_id',p.parent_run_id,'state',p.state) FROM agent_delegation_permissions p JOIN agent_definitions a ON a.id=p.requester_agent_id JOIN agent_definitions b ON b.id=p.specialist_agent_id WHERE p.user_context_id=$1 ORDER BY p.created_at DESC LIMIT 100").bind(context.id.0).fetch_all(self.db.pool()).await?)
    }
    pub async fn revoke(
        &self,
        context: &ResolvedUserContext,
        id: Uuid,
    ) -> Result<(), DurableTaskError> {
        let changed=sqlx::query("UPDATE agent_delegation_permissions SET state='revoked',revoked_at=now() WHERE id=$1 AND user_context_id=$2 AND state='enabled'").bind(id).bind(context.id.0).execute(self.db.pool()).await?.rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::NotFound);
        }
        Ok(())
    }
    pub async fn authorize_run(&self, run: &AssignedRun) -> Result<(), DurableTaskError> {
        let Some(parent_id) = run.parent_run_id else {
            return Ok(());
        };
        let row=sqlx::query("SELECT r.agent_id,r.actor_snapshot,r.authority,r.parent_run_id,j.state FROM assigned_task_runs r JOIN jobs j ON j.id=r.job_id WHERE r.job_id=$1 AND r.user_context_id=$2").bind(parent_id).bind(run.context.id.0).fetch_optional(self.db.pool()).await?.ok_or(DurableTaskError::NotFound)?;
        if row.get::<Option<Uuid>, _>("parent_run_id").is_some()
            || matches!(
                row.get::<String, _>("state").as_str(),
                "cancelled" | "failed"
            )
        {
            return Err(DurableTaskError::NotFound);
        }
        let parent: crate::agent_registry::SelectedAgent =
            serde_json::from_value(row.get("actor_snapshot"))
                .map_err(|_| DurableTaskError::Invalid)?;
        let parent_current = capture_actor(
            &self.db,
            &run.context,
            Some(&parent.definition.external_key),
        )
        .await?;
        let child_current = capture_actor(
            &self.db,
            &run.context,
            Some(&run.actor.agent.definition.external_key),
        )
        .await?;
        if !subset(&run.actor.authority, &child_current.authority) {
            return Err(DurableTaskError::NotFound);
        }
        if let Some(permission) = run.delegation_permission_id {
            let permitted:Option<Value>=sqlx::query_scalar("SELECT scope FROM agent_delegation_permissions WHERE id=$1 AND user_context_id=$2 AND requester_agent_id=$3 AND specialist_agent_id=$4 AND state='enabled' AND (parent_run_id IS NULL OR parent_run_id=$5)")
    .bind(permission).bind(run.context.id.0).bind(parent.definition.id).bind(run.actor.agent.definition.id).bind(parent_id).fetch_optional(self.db.pool()).await?;
            let scope: RunAuthority =
                serde_json::from_value(permitted.ok_or(DurableTaskError::NotFound)?)
                    .map_err(|_| DurableTaskError::Invalid)?;
            if !subset(&run.actor.authority, &scope) {
                return Err(DurableTaskError::NotFound);
            }
        } else {
            let pinned_parent: RunAuthority = serde_json::from_value(row.get("authority"))
                .map_err(|_| DurableTaskError::Invalid)?;
            if !subset(&run.actor.authority, &pinned_parent)
                || !subset(&run.actor.authority, &parent_current.authority)
            {
                return Err(DurableTaskError::NotFound);
            }
        }
        for cap in &run.actor.authority.capabilities {
            if current_capability(
                &self.db,
                &run.context,
                cap.connection_id,
                &cap.capability_external_key,
            )
            .await?
                != *cap
            {
                return Err(DurableTaskError::NotFound);
            }
        }
        Ok(())
    }
    pub async fn delegate(
        &self,
        parent: &AssignedRun,
        r: DelegateRequest,
    ) -> Result<crate::durable_tasks::DurableTask, DurableTaskError> {
        validate_scope(&r.scope)?;
        if r.brief.trim().is_empty() || r.brief.len() > 8192 || parent.parent_run_id.is_some() {
            return Err(DurableTaskError::Invalid);
        }
        DurableTaskService::new(self.db.clone())
            .verify_run(parent)
            .await?;
        let mut actor =
            capture_actor(&self.db, &parent.context, Some(&r.specialist_agent_key)).await?;
        if actor.agent.definition.id == parent.actor.agent.definition.id
            || !subset(&r.scope, &actor.authority)
        {
            return Err(DurableTaskError::Invalid);
        }
        actor.authority = r.scope;
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("SELECT id FROM user_contexts WHERE id=$1 FOR UPDATE")
            .bind(parent.context.id.0)
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT id FROM spans WHERE id=$1 FOR UPDATE")
            .bind(parent.task.id)
            .execute(&mut *tx)
            .await?;
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1 AND state='running' AND lease_owner=$2 AND lease_generation=$3 AND lease_expires_at>now())").bind(parent.task.run_id).bind(&parent.lease_owner).bind(parent.lease_generation).fetch_one(&mut *tx).await?;
        if !valid {
            return Err(DurableTaskError::Conflict);
        }
        let children: i64 =
            sqlx::query_scalar("SELECT count(*) FROM assigned_task_runs WHERE parent_run_id=$1")
                .bind(parent.task.run_id)
                .fetch_one(&mut *tx)
                .await?;
        if children >= 2 {
            return Err(DurableTaskError::BudgetExceeded);
        }
        if let Some(permission) = r.permission_id {
            let row=sqlx::query("SELECT scope,mode,used FROM agent_delegation_permissions WHERE id=$1 AND user_context_id=$2 AND requester_agent_id=$3 AND specialist_agent_id=$4 AND state='enabled' AND (parent_run_id IS NULL OR parent_run_id=$5) FOR UPDATE")
    .bind(permission).bind(parent.context.id.0).bind(parent.actor.agent.definition.id).bind(actor.agent.definition.id).bind(parent.task.run_id).fetch_optional(&mut *tx).await?.ok_or(DurableTaskError::NotFound)?;
            let permitted: RunAuthority =
                serde_json::from_value(row.get("scope")).map_err(|_| DurableTaskError::Invalid)?;
            if !subset(&actor.authority, &permitted)
                || (row.get::<String, _>("mode") == "once" && row.get::<bool, _>("used"))
            {
                return Err(DurableTaskError::NotFound);
            }
            sqlx::query("UPDATE agent_delegation_permissions SET used=true WHERE id=$1")
                .bind(permission)
                .execute(&mut *tx)
                .await?;
        } else if !subset(&actor.authority, &parent.actor.authority) {
            return Err(DurableTaskError::NotFound);
        }
        let span:Uuid=sqlx::query_scalar("INSERT INTO spans(user_id,user_context_id,title,notes,status,execution_type,parent_id) VALUES($1,$2,$3,$4,'planned','interactive',$5) RETURNING id").bind(parent.context.user_id.0).bind(parent.context.id.0).bind(format!("Specialist: {}",actor.agent.definition.display_name)).bind(&r.brief).bind(parent.task.id).fetch_one(&mut *tx).await?;
        let job:Uuid=sqlx::query_scalar("INSERT INTO jobs(user_id,user_context_id,kind,payload_reference_id,span_id,max_attempts) VALUES($1,$2,'execute_span',$3,$3,3) RETURNING id").bind(parent.context.user_id.0).bind(parent.context.id.0).bind(span).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO assigned_task_runs(job_id,user_context_id,agent_id,instruction_version,model_configuration_id,model_version,actor_snapshot,authority,task_instruction,parent_run_id,delegation_permission_id,deadline_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
   .bind(job).bind(parent.context.id.0).bind(actor.agent.definition.id).bind(actor.agent.definition.instruction_version).bind(actor.agent.model_configuration.id).bind(actor.agent.model_configuration.version).bind(json!(actor.agent)).bind(json!(actor.authority)).bind(r.brief).bind(parent.task.run_id).bind(r.permission_id).bind(parent.deadline_at).execute(&mut *tx).await?;
        sqlx::query("UPDATE jobs SET state='pending',wait_reason='delegation',lease_owner=NULL,lease_expires_at=NULL WHERE id=$1").bind(parent.task.run_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE spans SET status='waiting_user',execution_result=$2 WHERE id=$1").bind(parent.task.id).bind(json!({"state":"waiting","reason":"delegation","checkpoint":{"child_task_id":span}})).execute(&mut *tx).await?;
        tx.commit().await?;
        DurableTaskService::new(self.db.clone())
            .get(&parent.context, span)
            .await
    }
    pub async fn reconcile_children(&self) -> Result<u64, DurableTaskError> {
        // Return only the child's bounded requested work product/status. No transcript,
        // private memory or provider credentials are copied into the parent's run.
        let rows=sqlx::query("SELECT parent.job_id, child.job_id AS child_id, child.result,j.state FROM assigned_task_runs child JOIN jobs j ON j.id=child.job_id JOIN assigned_task_runs parent ON parent.job_id=child.parent_run_id JOIN jobs pj ON pj.id=parent.job_id WHERE child.result_received_at IS NULL AND j.state IN ('completed','failed','cancelled') AND pj.state='pending' AND pj.wait_reason='delegation' ORDER BY child.job_id LIMIT 20").fetch_all(self.db.pool()).await?;
        let mut count = 0;
        for row in rows {
            let parent: Uuid = row.get("job_id");
            let child: Uuid = row.get("child_id");
            let result: Value = row.get("result");
            if result.to_string().len() > 32 * 1024 {
                return Err(DurableTaskError::Invalid);
            }
            let mut tx = self.db.pool().begin().await?;
            let span:Option<Uuid>=sqlx::query_scalar("SELECT s.id FROM spans s JOIN jobs j ON j.span_id=s.id WHERE j.id=$1 AND s.status<>'cancelled' FOR UPDATE OF s").bind(parent).fetch_optional(&mut *tx).await?;
            let Some(span) = span else { continue };
            let changed=sqlx::query("UPDATE assigned_task_runs SET result_received_at=now() WHERE job_id=$1 AND result_received_at IS NULL").bind(child).execute(&mut *tx).await?.rows_affected();
            if changed == 1 {
                sqlx::query("UPDATE jobs SET checkpoint=jsonb_build_object('specialist_result',$2,'child_run_id',$3,'child_state',$4),wait_reason=NULL WHERE id=$1 AND state='pending' AND wait_reason='delegation'").bind(parent).bind(result).bind(child).bind(row.get::<String,_>("state")).execute(&mut *tx).await?;
                sqlx::query("UPDATE spans SET status='planned' WHERE id=$1")
                    .bind(span)
                    .execute(&mut *tx)
                    .await?;
                count += 1;
            }
            tx.commit().await?;
        }
        Ok(count)
    }
}
fn validate_scope(scope: &RunAuthority) -> Result<(), DurableTaskError> {
    if scope.capabilities.is_empty() || scope.capabilities.len() > 32 || !scope.skills.is_empty() {
        return Err(DurableTaskError::Invalid);
    }
    Ok(())
}
fn subset(request: &RunAuthority, available: &RunAuthority) -> bool {
    request
        .capabilities
        .iter()
        .all(|cap| available.capabilities.contains(cap))
        && request
            .skills
            .iter()
            .all(|skill| available.skills.contains(skill))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent_registry::AgentMutation,
        durable_tasks::{StartTaskRequest, runs::RunOutcome},
        identity::IdentityService,
        workers::task_executor::{
            AssignedTaskRunner, RunTools, TaskExecutorError, TaskExecutorHandler,
        },
    };
    use async_trait::async_trait;
    use chrono::{Duration, Utc};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;
    struct Specialist;
    #[async_trait]
    impl AssignedTaskRunner for Specialist {
        async fn run(
            &self,
            run: AssignedRun,
            tools: RunTools,
        ) -> Result<RunOutcome, TaskExecutorError> {
            assert_eq!(run.instruction, "Only report the selected repository count");
            assert!(
                tools.memory_get().await.is_err(),
                "delegation never includes unselected specialist private memory"
            );
            Ok(RunOutcome::Completed {
                summary: "Selected repository count: 3".into(),
            })
        }
    }
    #[tokio::test]
    #[ignore = "requires isolated TEST_DATABASE_URL"]
    async fn delegation_is_scoped_revocable_and_returns_only_bounded_work() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let context = IdentityService::new(db.clone())
            .resolve_for_user(user)
            .await
            .unwrap();
        let registry = AgentRegistry::new(db.clone());
        let default = registry.owned_for_context(&context).await.unwrap()[0].clone();
        registry
            .mutate_owned(
                &context,
                AgentMutation::Create {
                    name: "Repository specialist".into(),
                    instructions: "Report selected repository facts only".into(),
                },
            )
            .await
            .unwrap();
        let child = registry
            .owned_for_context(&context)
            .await
            .unwrap()
            .into_iter()
            .find(|a| !a.definition.is_default)
            .unwrap();
        let extension:Uuid=sqlx::query_scalar("INSERT INTO remote_extensions(user_context_id,external_key,display_name,protocol,endpoint_url,operator_id,operator_name,conformance_status,operator_enabled,lifecycle_state) VALUES($1,'delegation-fixture','Delegation fixture','mcp','https://example.com/mcp','fixture','Fixture','passed',true,'active') RETURNING id").bind(context.id.0).fetch_one(db.pool()).await.unwrap();
        let cap = json!({"external_key":"repository.read","display_name":"Repository read","effect":"read","consequential":false,"input_schema":{"type":"object"},"data_recipients":[],"access_needs":[],"supported_regions":[],"optional_guarantees":{}});
        sqlx::query("INSERT INTO remote_extension_versions(extension_id,version,endpoint_url,operator_id,operator_name,capabilities,conformance_status) VALUES($1,1,'https://example.com/mcp','fixture','Fixture',$2,'passed')").bind(extension).bind(json!([cap])).execute(db.pool()).await.unwrap();
        let connection:Uuid=sqlx::query_scalar("INSERT INTO external_connections(user_context_id,remote_extension_id,external_account_hash,credential_custody,authorization_state,authorized_capabilities) VALUES($1,$2,$3,'platform_held','authorized',ARRAY['repository.read']) RETURNING id").bind(context.id.0).bind(extension).bind(vec![5u8;32]).fetch_one(db.pool()).await.unwrap();
        sqlx::query("INSERT INTO agent_capability_grants(user_context_id,agent_definition_id,connection_id,capability_external_key) VALUES($1,$2,$3,'repository.read')").bind(context.id.0).bind(child.definition.id).bind(connection).execute(db.pool()).await.unwrap();
        let scope = capture_actor(&db, &context, Some(&child.definition.external_key))
            .await
            .unwrap()
            .authority;
        let service = DurableTaskService::new(db.clone());
        let delegation = DelegationService::new(db.clone());
        let request = || StartTaskRequest {
            title: "Engineering report".into(),
            instruction: "Produce the engineering report".into(),
            agent_external_key: Some(default.definition.external_key.clone()),
        };
        let task = service.start(&context, request()).await.unwrap();
        let parent = service
            .claim_assigned("parent-worker", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        let delegate = |permission_id| DelegateRequest {
            specialist_agent_key: child.definition.external_key.clone(),
            brief: "Only report the selected repository count".into(),
            scope: scope.clone(),
            permission_id,
        };
        assert!(
            delegation.delegate(&parent, delegate(None)).await.is_err(),
            "specialist grant does not imply parent access"
        );
        let permission = delegation
            .create_permission(
                &context,
                PermissionRequest {
                    requester_agent_key: default.definition.external_key.clone(),
                    specialist_agent_key: child.definition.external_key.clone(),
                    scope: scope.clone(),
                    parent_run_id: Some(parent.task.run_id),
                },
            )
            .await
            .unwrap();
        let child_task = delegation
            .delegate(&parent, delegate(Some(permission)))
            .await
            .unwrap();
        assert_eq!(
            service.get(&context, task.id).await.unwrap().wait_reason,
            Some(crate::durable_tasks::WaitReason::Delegation)
        );
        let child_run = service
            .claim_assigned("child-worker", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child_run.parent_run_id, Some(parent.task.run_id));
        assert!(delegation.authorize_run(&child_run).await.is_ok());
        assert!(
            delegation
                .delegate(&child_run, delegate(Some(permission)))
                .await
                .is_err(),
            "no recursive delegation"
        );
        TaskExecutorHandler::with_runner(db.clone(), Arc::new(Specialist))
            .execute_claim(child_run, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            service.get(&context, child_task.id).await.unwrap().state,
            crate::durable_tasks::RunState::Completed
        );
        assert_eq!(delegation.reconcile_children().await.unwrap(), 1);
        let resumed = service
            .claim_assigned("parent-resumed", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            resumed.checkpoint["specialist_result"]["summary"],
            "Selected repository count: 3"
        );
        assert!(
            delegation
                .delegate(&resumed, delegate(Some(permission)))
                .await
                .is_err(),
            "once permission cannot create another child"
        );
        service.cancel(&context, task.id).await.unwrap();
        let remembered = delegation
            .create_permission(
                &context,
                PermissionRequest {
                    requester_agent_key: default.definition.external_key.clone(),
                    specialist_agent_key: child.definition.external_key.clone(),
                    scope: scope.clone(),
                    parent_run_id: None,
                },
            )
            .await
            .unwrap();
        let task = service.start(&context, request()).await.unwrap();
        let parent = service
            .claim_assigned("parent2", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        let child_task = delegation
            .delegate(&parent, delegate(Some(remembered)))
            .await
            .unwrap();
        let child_run = service
            .claim_assigned("child2", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        delegation.revoke(&context, remembered).await.unwrap();
        assert!(
            service.enter_tool(&child_run).await.is_err(),
            "revoked remembered access stops subsequent invocations"
        );
        service.cancel(&context, task.id).await.unwrap();
        assert_eq!(
            service.get(&context, child_task.id).await.unwrap().state,
            crate::durable_tasks::RunState::Cancelled
        );
        let mut foreign = context.clone();
        foreign.id = crate::identity::UserContextId(Uuid::new_v4());
        assert!(delegation.list(&foreign).await.unwrap().is_empty());
        assert!(delegation.revoke(&foreign, permission).await.is_err());
        assert!(
            sqlx::query("UPDATE agent_delegation_permissions SET scope='{}' WHERE id=$1")
                .bind(permission)
                .execute(db.pool())
                .await
                .is_err()
        );
    }
}
