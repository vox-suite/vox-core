//! Immutable assigned-run authority and lease-fenced state transitions.
use super::*;
use crate::agent_registry::{AgentRegistry, SelectedAgent};
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
        let row=sqlx::query("WITH candidate AS (SELECT j.id FROM jobs j JOIN assigned_task_runs r ON r.job_id=j.id JOIN spans s ON s.id=j.span_id JOIN user_contexts c ON c.id=r.user_context_id WHERE (SELECT count(*) FROM jobs active JOIN assigned_task_runs ar ON ar.job_id=active.id WHERE ar.user_context_id=r.user_context_id AND active.state='running' AND active.lease_expires_at>$1)<2 AND j.kind='execute_span' AND j.wait_reason IS NULL AND ((j.state='pending' AND j.available_at<=$1) OR (j.state='running' AND j.lease_expires_at<=$1)) AND s.status<>'cancelled' ORDER BY j.priority DESC,j.available_at,j.created_at FOR UPDATE OF j,c SKIP LOCKED LIMIT 1) UPDATE jobs j SET state='running',lease_generation=j.lease_generation+1,attempt_count=j.attempt_count+1,lease_owner=$2,lease_expires_at=$3 FROM candidate WHERE j.id=candidate.id RETURNING j.id,j.span_id,j.checkpoint,j.lease_generation")
            .bind(now).bind(worker).bind(now+lease).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let run_id: Uuid = row.get("id");
        let span_id: Uuid = row.get("span_id");
        let snapshot=sqlx::query("SELECT actor_snapshot,authority,task_instruction,deadline_at,parent_run_id,delegation_permission_id FROM assigned_task_runs WHERE job_id=$1").bind(run_id).fetch_one(&mut *tx).await?;
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
        let current:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs j JOIN assigned_task_runs r ON r.job_id=j.id JOIN spans s ON s.id=j.span_id WHERE j.id=$1 AND j.state='running' AND j.lease_owner=$2 AND j.lease_generation=$3 AND j.lease_expires_at>now() AND j.wait_reason IS NULL AND s.status<>'cancelled' AND r.deadline_at>now() AND j.attempt_count<=j.max_attempts)")
            .bind(run.task.run_id).bind(&run.lease_owner).bind(run.lease_generation).fetch_one(self.db.pool()).await?;
        if !current {
            return Err(DurableTaskError::Conflict);
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
        // Delegated authority needs its current permission checked by the
        // delegation service before a worker can execute a child run.
        if run.delegation_permission_id.is_some() || run.parent_run_id.is_some() {
            return Err(DurableTaskError::Conflict);
        }
        Ok(())
    }

    pub async fn enter_tool(&self, run: &AssignedRun) -> Result<(), DurableTaskError> {
        self.verify_run(run).await?;
        let changed=sqlx::query("UPDATE assigned_task_runs r SET tool_calls=tool_calls+1 FROM jobs j WHERE r.job_id=$1 AND j.id=r.job_id AND j.state='running' AND j.lease_owner=$2 AND j.lease_generation=$3 AND j.lease_expires_at>now() AND r.tool_calls<r.max_tool_calls")
            .bind(run.task.run_id).bind(&run.lease_owner).bind(run.lease_generation).execute(self.db.pool()).await?.rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
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
        if let RunOutcome::Waiting { checkpoint, .. } = &outcome {
            if !checkpoint.is_object() {
                return Err(DurableTaskError::Invalid);
            }
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
            RunOutcome::Waiting { checkpoint, .. } => checkpoint.clone(),
            _ => run.checkpoint.clone(),
        };
        let changed=sqlx::query("UPDATE jobs SET state=$4,wait_reason=$5,checkpoint=$6,lease_owner=NULL,lease_expires_at=NULL,completed_at=CASE WHEN $4='pending' THEN NULL ELSE now() END WHERE id=$1 AND state='running' AND lease_owner=$2 AND lease_generation=$3 AND lease_expires_at>now()")
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

    pub async fn wait_for_proposal(
        &self,
        run: &AssignedRun,
        proposal_id: Uuid,
    ) -> Result<(), DurableTaskError> {
        let changed=sqlx::query("UPDATE assigned_task_runs r SET pending_proposal_id=$2 FROM action_proposals p,jobs j WHERE r.job_id=$1 AND p.id=$2 AND p.user_context_id=r.user_context_id AND p.actor_key=$3 AND j.id=r.job_id AND j.state='running' AND j.lease_owner=$4 AND j.lease_generation=$5 AND j.lease_expires_at>now()")
            .bind(run.task.run_id).bind(proposal_id).bind(&run.actor.agent.definition.external_key).bind(&run.lease_owner).bind(run.lease_generation).execute(self.db.pool()).await?.rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
        }
        self.finish_assigned(
            run,
            RunOutcome::Waiting {
                reason: WaitReason::Approval,
                checkpoint: serde_json::json!({"proposal_id":proposal_id,"executed":false}),
            },
        )
        .await
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
