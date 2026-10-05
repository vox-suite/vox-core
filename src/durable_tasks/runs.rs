//! Immutable assigned-run authority and lease-fenced state transitions.
use super::*;
use crate::agent_registry::{AgentRegistry, SelectedAgent};
use chrono::{DateTime, Duration};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunCapability {
    pub connection_id: Uuid,
    pub capability_external_key: String,
    pub extension_version: i32,
    pub declaration_digest: String,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunSkill {
    pub skill_id: Uuid,
    pub version: i32,
    pub digest: String,
}
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunAuthority {
    pub capabilities: Vec<RunCapability>,
    pub skills: Vec<RunSkill>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PinnedRunActor {
    pub agent: SelectedAgent,
    pub authority: RunAuthority,
}
#[derive(Clone, Debug)]
pub struct AssignedRun {
    pub task: DurableTask,
    pub context: ResolvedUserContext,
    pub actor: PinnedRunActor,
    pub instruction: String,
    pub checkpoint: Value,
    pub lease_owner: String,
    pub lease_generation: i64,
    pub deadline_at: DateTime<Utc>,
    pub parent_run_id: Option<Uuid>,
    pub delegation_permission_id: Option<Uuid>,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunOutcome {
    Completed {
        summary: String,
    },
    Waiting {
        reason: WaitReason,
        checkpoint: Value,
    },
    Failed {
        code: String,
    },
}

/// Compare this immutable reviewed declaration on every invocation. Matching
/// keys alone never permits a newly expanded provider contract.
pub async fn current_capability(
    db: &Db,
    context: &ResolvedUserContext,
    connection_id: Uuid,
    key: &str,
) -> Result<RunCapability, DurableTaskError> {
    let row=sqlx::query("SELECT e.current_version, jsonb_build_object('endpoint_url',e.endpoint_url,'protocol',e.protocol,'operator_id',e.operator_id,'capability',cap) AS declaration FROM external_connections x JOIN remote_extensions e ON e.id=x.remote_extension_id AND e.user_context_id=x.user_context_id JOIN remote_extension_versions v ON v.extension_id=e.id AND v.version=e.current_version CROSS JOIN LATERAL jsonb_array_elements(v.capabilities) cap WHERE x.id=$1 AND x.user_context_id=$2 AND cap->>'external_key'=$3 AND x.authorization_state='authorized' AND e.lifecycle_state='active' AND e.consent_status='consented' AND e.conformance_status='passed' AND v.conformance_status='passed' AND e.operator_enabled")
        .bind(connection_id).bind(context.id.0).bind(key).fetch_optional(db.pool()).await?.ok_or(DurableTaskError::NotFound)?;
    Ok(RunCapability {
        connection_id,
        capability_external_key: key.into(),
        extension_version: row.get("current_version"),
        declaration_digest: hex::encode(Sha256::digest(
            serde_json::to_vec(&row.get::<Value, _>("declaration"))
                .map_err(|_| DurableTaskError::Invalid)?,
        )),
    })
}

pub async fn capture_actor(
    db: &Db,
    context: &ResolvedUserContext,
    key: Option<&str>,
) -> Result<PinnedRunActor, DurableTaskError> {
    let registry = AgentRegistry::new(db.clone());
    let agent = if let Some(key) = key {
        registry
            .selected_for_context(context, key)
            .await
            .map_err(|_| DurableTaskError::Invalid)?
    } else {
        registry
            .owned_for_context(context)
            .await
            .map_err(|_| DurableTaskError::Invalid)?
            .into_iter()
            .find(|a| a.definition.is_default)
            .ok_or(DurableTaskError::Invalid)?
    };
    let grants = crate::capability_grants::CapabilityGrantService::new(db.pool().clone())
        .effective_for_agent(&context.request_context(), &agent.definition.external_key)
        .await
        .map_err(|_| DurableTaskError::Invalid)?;
    let mut authority = RunAuthority::default();
    for grant in grants {
        match current_capability(
            db,
            context,
            grant.connection_id,
            &grant.capability_external_key,
        )
        .await
        {
            Ok(cap) => authority.capabilities.push(cap),
            Err(DurableTaskError::NotFound) => {} // Native adapters are not AgentLibrary MCP capabilities.
            Err(e) => return Err(e),
        }
    }
    let skills = crate::skills::SkillService::new(db.pool().clone())
        .effective(context, &agent.definition.external_key)
        .await
        .map_err(|_| DurableTaskError::Invalid)?;
    for skill in skills {
        let digest:String=sqlx::query_scalar("SELECT digest FROM skill_package_versions WHERE skill_id=$1 AND version=$2 AND digest IS NOT NULL")
            .bind(skill.id).bind(skill.version).fetch_optional(db.pool()).await?.ok_or(DurableTaskError::Invalid)?;
        authority.skills.push(RunSkill {
            skill_id: skill.id,
            version: skill.version,
            digest,
        });
    }
    if authority.capabilities.len() > 256 || authority.skills.len() > 128 {
        return Err(DurableTaskError::Invalid);
    }
    Ok(PinnedRunActor { agent, authority })
}

impl DurableTaskService {
    pub async fn claim_assigned(
        &self,
        worker: &str,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<Option<AssignedRun>, DurableTaskError> {
        text(worker, 255)?;
        if lease <= Duration::zero() || lease > Duration::minutes(5) {
            return Err(DurableTaskError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;
        // Lock the context in a separate statement before counting running
        // work, then lock span before job consistently with cancel/proposals.
        let context_id:Option<Uuid>=sqlx::query_scalar("SELECT c.id FROM user_contexts c WHERE (SELECT count(*) FROM jobs active JOIN assigned_task_runs ar ON ar.job_id=active.id WHERE ar.user_context_id=c.id AND active.state='running' AND active.lease_expires_at>$1)<2 AND EXISTS(SELECT 1 FROM jobs j JOIN assigned_task_runs r ON r.job_id=j.id WHERE r.user_context_id=c.id AND j.kind='execute_span' AND j.wait_reason IS NULL AND ((j.state='pending' AND j.available_at<=$1) OR (j.state='running' AND j.lease_expires_at<=$1))) ORDER BY c.id FOR UPDATE OF c SKIP LOCKED LIMIT 1").bind(now).fetch_optional(&mut *tx).await?;
        let Some(context_id) = context_id else {
            tx.commit().await?;
            return Ok(None);
        };
        let running:i64=sqlx::query_scalar("SELECT count(*) FROM jobs j JOIN assigned_task_runs r ON r.job_id=j.id WHERE r.user_context_id=$1 AND j.state='running' AND j.lease_expires_at>$2").bind(context_id).bind(now).fetch_one(&mut *tx).await?;
        if running >= 2 {
            tx.commit().await?;
            return Ok(None);
        }
        let candidate=sqlx::query("SELECT j.id,j.span_id FROM jobs j JOIN assigned_task_runs r ON r.job_id=j.id JOIN spans s ON s.id=j.span_id WHERE r.user_context_id=$2 AND j.kind='execute_span' AND j.wait_reason IS NULL AND ((j.state='pending' AND j.available_at<=$1) OR (j.state='running' AND j.lease_expires_at<=$1)) AND s.status<>'cancelled' ORDER BY j.priority DESC,j.available_at,j.created_at FOR UPDATE OF s SKIP LOCKED LIMIT 1").bind(now).bind(context_id).fetch_optional(&mut *tx).await?;
        let Some(candidate) = candidate else {
            tx.commit().await?;
            return Ok(None);
        };
        let row=sqlx::query("UPDATE jobs SET state='running',lease_generation=lease_generation+1,attempt_count=attempt_count+1,lease_owner=$2,lease_expires_at=$3 WHERE id=$4 AND wait_reason IS NULL AND ((state='pending' AND available_at<=$1) OR (state='running' AND lease_expires_at<=$1)) RETURNING id,span_id,checkpoint,lease_generation")
            .bind(now).bind(worker).bind(now+lease).bind(candidate.get::<Uuid,_>("id")).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let run_id: Uuid = row.get("id");
        let span_id: Uuid = row.get("span_id");
        let snapshot=sqlx::query("SELECT actor_snapshot,authority,task_instruction,deadline_at,parent_run_id,delegation_permission_id FROM assigned_task_runs WHERE job_id=$1").bind(run_id).fetch_one(&mut *tx).await?;
        if let Some(parent) = snapshot.get::<Option<Uuid>, _>("parent_run_id") {
            let charged=sqlx::query("UPDATE jobs j SET attempt_count=attempt_count+1 FROM assigned_task_runs r WHERE j.id=$1 AND r.job_id=j.id AND j.state='pending' AND j.wait_reason='specialist' AND j.attempt_count<j.max_attempts AND r.deadline_at>$2").bind(parent).bind(now).execute(&mut *tx).await?.rows_affected();
            if charged != 1 {
                sqlx::query("UPDATE jobs SET state='pending',wait_reason='budget',lease_owner=NULL,lease_expires_at=NULL WHERE id=$1").bind(run_id).execute(&mut *tx).await?;
                tx.commit().await?;
                return Ok(None);
            }
        }
        sqlx::query(
            "UPDATE spans SET status='active',updated_at=now() WHERE id=$1 AND status<>'cancelled'",
        )
        .bind(span_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let context = self.context_for_task(span_id).await?;
        let task = self.load(&context, span_id).await?;
        Ok(Some(AssignedRun {
            task,
            context,
            actor: PinnedRunActor {
                agent: serde_json::from_value(snapshot.get("actor_snapshot"))
                    .map_err(|_| DurableTaskError::Invalid)?,
                authority: serde_json::from_value(snapshot.get("authority"))
                    .map_err(|_| DurableTaskError::Invalid)?,
            },
            instruction: snapshot.get("task_instruction"),
            checkpoint: row.get("checkpoint"),
            lease_owner: worker.into(),
            lease_generation: row.get("lease_generation"),
            deadline_at: snapshot.get("deadline_at"),
            parent_run_id: snapshot.get("parent_run_id"),
            delegation_permission_id: snapshot.get("delegation_permission_id"),
        }))
    }

    pub async fn verify_run(&self, run: &AssignedRun) -> Result<(), DurableTaskError> {
        let current=sqlx::query("SELECT r.deadline_at,j.attempt_count,j.max_attempts FROM jobs j JOIN assigned_task_runs r ON r.job_id=j.id JOIN spans s ON s.id=j.span_id WHERE j.id=$1 AND j.state='running' AND j.lease_owner=$2 AND j.lease_generation=$3 AND j.lease_expires_at>now() AND j.wait_reason IS NULL AND s.status<>'cancelled'")
            .bind(run.task.run_id).bind(&run.lease_owner).bind(run.lease_generation).fetch_optional(self.db.pool()).await?.ok_or(DurableTaskError::Conflict)?;
        if current.get::<DateTime<Utc>, _>("deadline_at") <= Utc::now()
            || current.get::<i32, _>("attempt_count") > current.get::<i32, _>("max_attempts")
        {
            return Err(DurableTaskError::BudgetExceeded);
        }
        let selected = AgentRegistry::new(self.db.clone())
            .selected_for_context(&run.context, &run.actor.agent.definition.external_key)
            .await
            .map_err(|_| DurableTaskError::NotFound)?;
        if selected.definition.id != run.actor.agent.definition.id
            || selected.model_configuration.id != run.actor.agent.model_configuration.id
        {
            return Err(DurableTaskError::NotFound);
        }
        crate::delegation::DelegationService::new(self.db.clone())
            .authorize_run(run)
            .await?;
        Ok(())
    }

    pub async fn enter_tool(&self, run: &AssignedRun) -> Result<(), DurableTaskError> {
        self.verify_run(run).await?;
        if let Some(parent) = run.parent_run_id {
            let changed=sqlx::query("UPDATE assigned_task_runs r SET tool_calls=tool_calls+1 FROM jobs j WHERE r.job_id=$1 AND j.id=r.job_id AND j.state IN ('pending','running') AND r.tool_calls<r.max_tool_calls AND r.deadline_at>now()")
                .bind(parent).execute(self.db.pool()).await?.rows_affected();
            if changed != 1 {
                return Err(DurableTaskError::BudgetExceeded);
            }
        }
        let changed=sqlx::query("UPDATE assigned_task_runs r SET tool_calls=tool_calls+1 FROM jobs j WHERE r.job_id=$1 AND j.id=r.job_id AND j.state='running' AND j.lease_owner=$2 AND j.lease_generation=$3 AND j.lease_expires_at>now() AND r.tool_calls<r.max_tool_calls")
            .bind(run.task.run_id).bind(&run.lease_owner).bind(run.lease_generation).execute(self.db.pool()).await?.rows_affected();
        if changed != 1 {
            self.verify_run(run).await?;
            return Err(DurableTaskError::BudgetExceeded);
        }
        Ok(())
    }

    pub async fn heartbeat_assigned(&self, run: &AssignedRun) -> Result<(), DurableTaskError> {
        let changed=sqlx::query("UPDATE jobs SET lease_expires_at=now()+interval '30 seconds' WHERE id=$1 AND state='running' AND wait_reason IS NULL AND lease_owner=$2 AND lease_generation=$3 AND lease_expires_at>now()")
            .bind(run.task.run_id).bind(&run.lease_owner).bind(run.lease_generation).execute(self.db.pool()).await?.rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
        }
        Ok(())
    }

    pub async fn finish_assigned(
        &self,
        run: &AssignedRun,
        outcome: RunOutcome,
    ) -> Result<(), DurableTaskError> {
        let serialized = serde_json::to_value(&outcome).map_err(|_| DurableTaskError::Invalid)?;
        if serialized.to_string().len() > 32 * 1024 {
            return Err(DurableTaskError::Invalid);
        }
        if let RunOutcome::Completed { summary } = &outcome {
            text(summary, 16 * 1024)?;
        }
        if let RunOutcome::Waiting { checkpoint, .. } = &outcome
            && !checkpoint.is_object()
        {
            return Err(DurableTaskError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;
        // Lock span before job, the same order as public cancel/wait/resume.
        sqlx::query("SELECT id FROM spans WHERE id=$1 FOR UPDATE")
            .bind(run.task.id)
            .fetch_one(&mut *tx)
            .await?;
        let (state, span_state, reason) = match &outcome {
            RunOutcome::Completed { .. } => ("completed", "done", None),
            RunOutcome::Waiting { reason, .. } => {
                ("pending", "waiting_user", Some(wait_name(reason)))
            }
            RunOutcome::Failed { .. } => ("failed", "failed", None),
        };
        let checkpoint = match &outcome {
            RunOutcome::Waiting { checkpoint, .. } => Some(checkpoint.clone()),
            _ => None,
        };
        let changed=sqlx::query("UPDATE jobs SET state=$4,wait_reason=$5,checkpoint=COALESCE($6,checkpoint),lease_owner=NULL,lease_expires_at=NULL,completed_at=CASE WHEN $4='pending' THEN NULL ELSE now() END WHERE id=$1 AND state='running' AND lease_owner=$2 AND lease_generation=$3 AND lease_expires_at>now() AND ($4<>'completed' OR (attempt_count<=max_attempts AND EXISTS(SELECT 1 FROM assigned_task_runs r WHERE r.job_id=jobs.id AND r.deadline_at>now())))")
            .bind(run.task.run_id).bind(&run.lease_owner).bind(run.lease_generation).bind(state).bind(reason).bind(checkpoint).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
        }
        sqlx::query("UPDATE assigned_task_runs SET result=$2 WHERE job_id=$1")
            .bind(run.task.run_id)
            .bind(&serialized)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE spans SET status=$2,execution_result=$3,updated_at=now(),completed_at=CASE WHEN $2 IN ('done','failed') THEN now() ELSE NULL END WHERE id=$1 AND status<>'cancelled'")
            .bind(run.task.id).bind(span_state).bind(serialized).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}
impl DurableTaskService {
    pub async fn save_checkpoint(
        &self,
        run: &AssignedRun,
        response: &Value,
    ) -> Result<(), DurableTaskError> {
        if response.to_string().len() > 16 * 1024 {
            return Err(DurableTaskError::Invalid);
        }
        let changed=sqlx::query("UPDATE jobs SET checkpoint=jsonb_build_object('last_tool_result',$4) WHERE id=$1 AND state='running' AND lease_owner=$2 AND lease_generation=$3 AND lease_expires_at>now()")
            .bind(run.task.run_id).bind(&run.lease_owner).bind(run.lease_generation).bind(response).execute(self.db.pool()).await?.rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
        }
        Ok(())
    }
}

#[cfg(test)]
mod assigned_tests {
    use super::*;
    use crate::{
        identity::IdentityService,
        workers::task_executor::{
            AssignedTaskRunner, RunTools, TaskExecutorError, TaskExecutorHandler,
        },
    };
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct WorkProduct;
    #[async_trait]
    impl AssignedTaskRunner for WorkProduct {
        async fn run(
            &self,
            run: AssignedRun,
            _: RunTools,
        ) -> Result<RunOutcome, TaskExecutorError> {
            assert_eq!(run.instruction, "Write the requested report");
            Ok(RunOutcome::Completed {
                summary: "The requested report is here.".into(),
            })
        }
    }

    #[tokio::test]
    #[ignore = "requires isolated TEST_DATABASE_URL"]
    async fn assigned_worker_is_pinned_fenced_recoverable_and_queryable() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").expect("isolated database"))
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
        let service = DurableTaskService::new(db.clone());
        let request = || StartTaskRequest {
            title: "Assigned report".into(),
            instruction: "Write the requested report".into(),
            agent_external_key: None,
        };
        let task = service.start(&context, request()).await.unwrap();
        assert!(task.agent_external_key.is_some());
        assert_eq!(task.instruction_version, Some(1));
        let before = service.query(&context, None, 1).await.unwrap();
        assert_eq!(before.tasks[0].id, task.id);
        let original = service
            .claim_assigned("old-worker", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        let changed = sqlx::query("UPDATE assigned_task_runs SET authority='{}' WHERE job_id=$1")
            .bind(task.run_id)
            .execute(db.pool())
            .await;
        assert!(
            changed.is_err(),
            "database must reject authority replacement"
        );
        sqlx::query("UPDATE jobs SET lease_expires_at=now()-interval '1 second' WHERE id=$1")
            .bind(task.run_id)
            .execute(db.pool())
            .await
            .unwrap();
        let recovered = service
            .claim_assigned("new-worker", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert!(recovered.lease_generation > original.lease_generation);
        assert!(matches!(
            service
                .finish_assigned(
                    &original,
                    RunOutcome::Completed {
                        summary: "stale result".into()
                    }
                )
                .await,
            Err(DurableTaskError::Conflict)
        ));
        service
            .save_checkpoint(&recovered, &serde_json::json!({"read_evidence":"retained"}))
            .await
            .unwrap();
        TaskExecutorHandler::with_runner(db.clone(), Arc::new(WorkProduct))
            .execute_claim(recovered, CancellationToken::new())
            .await
            .unwrap();
        let result = service.get(&context, task.id).await.unwrap();
        assert_eq!(result.state, RunState::Completed);
        assert_eq!(result.result["summary"], "The requested report is here.");
        let checkpoint: Value = sqlx::query_scalar("SELECT checkpoint FROM jobs WHERE id=$1")
            .bind(task.run_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(checkpoint["last_tool_result"]["read_evidence"], "retained");
        let task = service.start(&context, request()).await.unwrap();
        let claim = service
            .claim_assigned("cancelled-worker", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        service.cancel(&context, task.id).await.unwrap();
        assert!(matches!(
            service
                .finish_assigned(
                    &claim,
                    RunOutcome::Completed {
                        summary: "must not overwrite cancel".into()
                    }
                )
                .await,
            Err(DurableTaskError::Conflict)
        ));
        assert_eq!(
            service.get(&context, task.id).await.unwrap().state,
            RunState::Cancelled
        );
        let limited = service.start(&context, request()).await.unwrap();
        sqlx::query("UPDATE jobs SET attempt_count=max_attempts WHERE id=$1")
            .bind(limited.run_id)
            .execute(db.pool())
            .await
            .unwrap();
        let claim = service
            .claim_assigned("budget-worker", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            service.verify_run(&claim).await,
            Err(DurableTaskError::BudgetExceeded)
        ));
        TaskExecutorHandler::with_runner(db.clone(), Arc::new(WorkProduct))
            .execute_claim(claim, CancellationToken::new())
            .await
            .unwrap();
        let limited = service.get(&context, limited.id).await.unwrap();
        assert_eq!(limited.wait_reason, Some(WaitReason::Budget));
        assert!(matches!(
            service.resume(&context, limited.id, None).await,
            Err(DurableTaskError::Conflict)
        ));
        let expired = service.start(&context, request()).await.unwrap();
        let mut claim = service
            .claim_assigned("deadline-worker", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        // Read-only injected claim time cannot replace the database deadline.
        claim.deadline_at = Utc::now() - Duration::seconds(1);
        assert!(service.verify_run(&claim).await.is_ok());
        service.cancel(&context, expired.id).await.unwrap();
        let extension:Uuid=sqlx::query_scalar("INSERT INTO remote_extensions(user_context_id,external_key,display_name,protocol,endpoint_url,operator_id,operator_name,conformance_status,operator_enabled,lifecycle_state) VALUES($1,'assigned-write-fixture','Assigned write fixture','mcp','https://example.com/mcp','fixture','Fixture','passed',true,'active') RETURNING id").bind(context.id.0).fetch_one(db.pool()).await.unwrap();
        let capability = serde_json::json!({"external_key":"fixture.write","display_name":"Controlled write","effect":"write","consequential":true,"input_schema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false},"data_recipients":["Fixture"],"access_needs":[],"supported_regions":[],"optional_guarantees":{}});
        sqlx::query("INSERT INTO remote_extension_versions(extension_id,version,endpoint_url,operator_id,operator_name,capabilities,conformance_status) VALUES($1,1,'https://example.com/mcp','fixture','Fixture',$2,'passed')").bind(extension).bind(serde_json::json!([capability.clone()])).execute(db.pool()).await.unwrap();
        sqlx::query("INSERT INTO remote_extension_credentials(extension_id,issuer,token_endpoint,client_id,resource,access_token_ciphertext,tools) VALUES($1,'https://example.com','https://example.com/token','fixture','https://example.com/mcp',$2,$3)")
            .bind(extension).bind(vec![1u8,2,3]).bind(serde_json::json!([{"name":"fixture.write","inputSchema":capability["input_schema"]}])).execute(db.pool()).await.unwrap();
        let connection:Uuid=sqlx::query_scalar("INSERT INTO external_connections(user_context_id,remote_extension_id,external_account_hash,credential_custody,authorization_state,authorized_capabilities) VALUES($1,$2,$3,'platform_held','authorized',ARRAY['fixture.write']) RETURNING id").bind(context.id.0).bind(extension).bind(vec![4u8;32]).fetch_one(db.pool()).await.unwrap();
        let selected = capture_actor(&db, &context, None).await.unwrap().agent;
        sqlx::query("INSERT INTO agent_capability_grants(user_context_id,agent_definition_id,connection_id,capability_external_key) VALUES($1,$2,$3,'fixture.write')").bind(context.id.0).bind(selected.definition.id).bind(connection).execute(db.pool()).await.unwrap();
        let proposed_task = service.start(&context, request()).await.unwrap();
        let claim = service
            .claim_assigned("proposal-worker", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        let proposal_request = || crate::approvals::CreateProposalRequest {
            span_id: proposed_task.id,
            task_run_id: proposed_task.run_id,
            agent_external_key: selected.definition.external_key.clone(),
            capability_external_key: "fixture.write".into(),
            details: serde_json::json!({"execution":{"connection_id":connection},"invocation":{"tool_name":"fixture.write","arguments":{"text":"exact approved content"}}}),
            expires_at: Utc::now() + Duration::minutes(10),
            replaces_proposal_id: None,
        };
        let approvals = crate::approvals::ApprovalService::new(db.clone());
        let proposal = approvals
            .propose_for_run(
                &context,
                proposal_request(),
                Utc::now(),
                &claim.lease_owner,
                claim.lease_generation,
            )
            .await
            .unwrap();
        let waiting = service.get(&context, proposed_task.id).await.unwrap();
        assert_eq!(waiting.state, RunState::Waiting);
        assert_eq!(waiting.wait_reason, Some(WaitReason::Approval));
        assert_eq!(waiting.result["reason"], "approval");
        assert_eq!(
            waiting.result["checkpoint"]["proposal_id"],
            proposal.id.to_string()
        );
        assert!(
            approvals
                .propose_for_run(
                    &context,
                    proposal_request(),
                    Utc::now(),
                    &claim.lease_owner,
                    claim.lease_generation
                )
                .await
                .is_err()
        );
        assert!(matches!(
            service.resume(&context, proposed_task.id, None).await,
            Err(DurableTaskError::Conflict)
        ));
        assert!(
            service
                .claim_assigned("another-worker", Utc::now(), Duration::seconds(30))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM executions WHERE proposal_id=$1")
                .bind(proposal.id)
                .fetch_one(db.pool())
                .await
                .unwrap(),
            0
        );
        service.cancel(&context, proposed_task.id).await.unwrap();
        let clarification = service.start(&context, request()).await.unwrap();
        let claim = service
            .claim_assigned("clarification-worker", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        service
            .finish_assigned(
                &claim,
                RunOutcome::Waiting {
                    reason: WaitReason::Clarification,
                    checkpoint: serde_json::json!({"question":"Which reporting period?"}),
                },
            )
            .await
            .unwrap();
        assert!(
            service
                .resume(&context, clarification.id, None)
                .await
                .is_err()
        );
        service
            .resume(&context, clarification.id, Some("September".into()))
            .await
            .unwrap();
        let resumed = service
            .claim_assigned("clarification-resumed", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert!(resumed.checkpoint.to_string().contains("September"));
        assert_eq!(
            resumed.actor.agent.definition.external_key,
            claim.actor.agent.definition.external_key
        );
        service.cancel(&context, clarification.id).await.unwrap();
        let mut other = context.clone();
        other.id = UserContextId(Uuid::new_v4());
        assert!(
            service
                .query(&other, None, 20)
                .await
                .unwrap()
                .tasks
                .is_empty()
        );
        assert!(matches!(
            service.query(&context, None, 51).await,
            Err(DurableTaskError::Invalid)
        ));
    }
}
