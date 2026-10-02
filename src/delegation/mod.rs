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
use sha2::{Digest, Sha256};
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
    pub scope: CapabilitySelection,
    pub parent_run_id: Option<Uuid>,
    #[serde(default)]
    pub preference_keys: Vec<String>,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct DelegateRequest {
    pub specialist_agent_key: String,
    pub brief: String,
    pub scope: CapabilitySelection,
    pub permission_id: Option<Uuid>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityReference {
    pub connection_id: Uuid,
    pub capability_external_key: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySelection {
    pub capabilities: Vec<CapabilityReference>,
}
impl CapabilitySelection {
    pub fn from_authority(scope: &RunAuthority) -> Self {
        Self {
            capabilities: scope
                .capabilities
                .iter()
                .map(|cap| CapabilityReference {
                    connection_id: cap.connection_id,
                    capability_external_key: cap.capability_external_key.clone(),
                })
                .collect(),
        }
    }
}
impl DelegationService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
    async fn capture_preferences(
        &self,
        context: &ResolvedUserContext,
        keys: &[String],
    ) -> Result<Value, DurableTaskError> {
        if keys.len() > 8 {
            return Err(DurableTaskError::Invalid);
        }
        let mut seen = std::collections::HashSet::new();
        let mut pins = Vec::new();
        let mut bytes = 0;
        for key in keys {
            if key.is_empty() || key.len() > 128 || !seen.insert(key) {
                return Err(DurableTaskError::Invalid);
            }
            let value = crate::preferences::PreferenceService::new(self.db.clone())
                .selected_nonsensitive(context, key)
                .await?
                .ok_or(DurableTaskError::NotFound)?;
            bytes += value.to_string().len() + key.len();
            if bytes > 2048 {
                return Err(DurableTaskError::Invalid);
            }
            pins.push(json!({"key":key,"digest":hex::encode(Sha256::digest(value.to_string().as_bytes()))}));
        }
        Ok(json!(pins))
    }
    pub async fn shared_preferences(&self, run: &AssignedRun) -> Result<Value, DurableTaskError> {
        let Some(id) = run.delegation_permission_id else {
            return Ok(json!({}));
        };
        let pins:Value=sqlx::query_scalar("SELECT shared_preferences FROM agent_delegation_permissions WHERE id=$1 AND user_context_id=$2 AND state='enabled'").bind(id).bind(run.context.id.0).fetch_optional(self.db.pool()).await?.ok_or(DurableTaskError::NotFound)?;
        let pins = pins.as_array().ok_or(DurableTaskError::Invalid)?;
        let keys: Vec<String> = pins
            .iter()
            .map(|p| {
                p["key"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(DurableTaskError::Invalid)
            })
            .collect::<Result<_, _>>()?;
        let mut selected = serde_json::Map::new();
        let mut bytes = 0;
        for (key, pin) in keys.into_iter().zip(pins) {
            let value = crate::preferences::PreferenceService::new(self.db.clone())
                .selected_nonsensitive(&run.context, &key)
                .await?
                .ok_or(DurableTaskError::NotFound)?;
            if pin["digest"] != json!(hex::encode(Sha256::digest(value.to_string().as_bytes()))) {
                return Err(DurableTaskError::NotFound);
            }
            bytes += key.len() + value.to_string().len();
            if bytes > 2048 {
                return Err(DurableTaskError::Invalid);
            }
            selected.insert(key, value);
        }
        Ok(Value::Object(selected))
    }
    pub async fn scopes(
        &self,
        context: &ResolvedUserContext,
        requester: &str,
        specialist: &str,
    ) -> Result<Value, DurableTaskError> {
        AgentRegistry::new(self.db.clone())
            .selected_for_context(context, requester)
            .await
            .map_err(|_| DurableTaskError::NotFound)?;
        let actor = capture_actor(&self.db, context, Some(specialist)).await?;
        let mut capabilities = Vec::new();
        for cap in actor.authority.capabilities {
            let metadata:Value=sqlx::query_scalar("SELECT jsonb_build_object('connection_id',x.id,'capability_external_key',$3::text,'account_display_id',x.account_display_id,'integration_name',e.display_name,'tool_name',m.display_name) FROM external_connections x JOIN remote_extensions e ON e.id=x.remote_extension_id JOIN connector_tool_metadata m ON m.extension_id=e.id AND m.version=e.current_version AND m.external_key=$3 WHERE x.id=$1 AND x.user_context_id=$2").bind(cap.connection_id).bind(context.id.0).bind(cap.capability_external_key).fetch_one(self.db.pool()).await?;
            capabilities.push(metadata);
        }
        let preferences:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('key',preference_key) FROM user_preferences WHERE user_context_id=$1 AND NOT is_sensitive ORDER BY preference_key LIMIT 64").bind(context.id.0).fetch_all(self.db.pool()).await?;
        Ok(json!({"capabilities":capabilities,"preferences":preferences}))
    }
    pub async fn specialists(
        &self,
        context: &ResolvedUserContext,
        requester: &str,
    ) -> Result<Value, DurableTaskError> {
        let agents = AgentRegistry::new(self.db.clone())
            .owned_for_context(context)
            .await
            .map_err(|_| DurableTaskError::NotFound)?;
        let items:Vec<Value>=agents.into_iter().filter(|a|a.definition.external_key!=requester).take(20).map(|a|json!({"agent_external_key":a.definition.external_key,"display_name":a.definition.display_name,"purpose":a.definition.purpose.chars().take(240).collect::<String>()})).collect();
        let permissions:Vec<Value>=self.list(context).await?.into_iter().filter(|p|p["requester_agent_key"]==requester && p["state"]=="enabled" && !(p["mode"]=="once" && p["used"]==true)).take(20).map(|p|json!({"id":p["id"],"specialist_agent_key":p["specialist_agent_key"],"mode":p["mode"],"parent_run_id":p["parent_run_id"],"capabilities":p["scope"]["capabilities"].as_array().unwrap_or(&Vec::new()).iter().map(|cap|json!({"connection_id":cap["connection_id"],"capability_external_key":cap["capability_external_key"]})).collect::<Vec<_>>() })).collect();
        Ok(json!({"specialists":items,"permissions":permissions}))
    }
    pub async fn assignment(
        &self,
        context: &ResolvedUserContext,
        job: Uuid,
    ) -> Result<Option<AssignedRun>, DurableTaskError> {
        let row=sqlx::query("SELECT j.span_id,j.checkpoint,r.actor_snapshot,r.authority,r.task_instruction,r.deadline_at,r.parent_run_id,r.delegation_permission_id FROM assigned_task_runs r JOIN jobs j ON j.id=r.job_id WHERE r.job_id=$1 AND r.user_context_id=$2").bind(job).bind(context.id.0).fetch_optional(self.db.pool()).await?;
        let Some(row) = row else { return Ok(None) };
        Ok(Some(AssignedRun {
            task: DurableTaskService::new(self.db.clone())
                .get(context, row.get("span_id"))
                .await?,
            context: context.clone(),
            actor: crate::durable_tasks::runs::PinnedRunActor {
                agent: serde_json::from_value(row.get("actor_snapshot"))
                    .map_err(|_| DurableTaskError::Invalid)?,
                authority: serde_json::from_value(row.get("authority"))
                    .map_err(|_| DurableTaskError::Invalid)?,
            },
            instruction: row.get("task_instruction"),
            checkpoint: row.get("checkpoint"),
            lease_owner: String::new(),
            lease_generation: 0,
            deadline_at: row.get("deadline_at"),
            parent_run_id: row.get("parent_run_id"),
            delegation_permission_id: row.get("delegation_permission_id"),
        }))
    }
    pub async fn lock_assignment_authority(
        &self,
        context: &ResolvedUserContext,
        job: Uuid,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> Result<(), DurableTaskError> {
        let row=sqlx::query("SELECT c.agent_id,c.authority,c.parent_run_id,c.delegation_permission_id,p.agent_id AS parent_agent_id,p.authority AS parent_authority FROM assigned_task_runs c JOIN assigned_task_runs p ON p.job_id=c.parent_run_id WHERE c.job_id=$1 AND c.user_context_id=$2 AND p.user_context_id=$2 FOR SHARE OF c,p").bind(job).bind(context.id.0).fetch_optional(&mut **tx).await?;
        let Some(row) = row else { return Ok(()) };
        let authority: RunAuthority =
            serde_json::from_value(row.get("authority")).map_err(|_| DurableTaskError::Invalid)?;
        let parent_authority: RunAuthority = serde_json::from_value(row.get("parent_authority"))
            .map_err(|_| DurableTaskError::Invalid)?;
        let child_agent: Uuid = row.get("agent_id");
        let parent_agent: Uuid = row.get("parent_agent_id");
        let permission: Option<Uuid> = row.get("delegation_permission_id");
        if permission.is_none() && !subset(&authority, &parent_authority) {
            return Err(DurableTaskError::NotFound);
        }
        let ids = vec![parent_agent, child_agent];
        // Freeze all mutable inputs of effective grants, then re-evaluate them.
        // Rows are locked even when revoked, so a committed revoke cannot hide
        // from the subsequent check and an in-flight revoke serializes here.
        sqlx::query("SELECT id FROM agent_definitions WHERE id=ANY($1) OR id IN (SELECT template_id FROM agent_definitions WHERE id=ANY($1)) ORDER BY id FOR SHARE").bind(&ids).fetch_all(&mut **tx).await?;
        sqlx::query("SELECT agent_definition_id FROM deployment_agent_selections WHERE agent_definition_id=ANY($1) ORDER BY agent_definition_id FOR SHARE").bind(&ids).fetch_all(&mut **tx).await?;
        let connections: Vec<Uuid> = authority
            .capabilities
            .iter()
            .map(|c| c.connection_id)
            .collect();
        sqlx::query("SELECT id FROM remote_extensions WHERE id IN (SELECT remote_extension_id FROM external_connections WHERE id=ANY($1)) ORDER BY id FOR SHARE").bind(&connections).fetch_all(&mut **tx).await?;
        sqlx::query("SELECT v.extension_id FROM remote_extension_versions v JOIN remote_extensions e ON e.id=v.extension_id WHERE e.id IN (SELECT remote_extension_id FROM external_connections WHERE id=ANY($1)) AND v.version=e.current_version ORDER BY v.extension_id FOR SHARE OF v").bind(&connections).fetch_all(&mut **tx).await?;
        sqlx::query("SELECT id FROM external_connections WHERE id=ANY($1) ORDER BY id FOR SHARE")
            .bind(&connections)
            .fetch_all(&mut **tx)
            .await?;
        sqlx::query("SELECT id FROM agent_capability_grants WHERE user_context_id=$1 AND agent_definition_id=ANY($2) AND connection_id=ANY($3) ORDER BY id FOR SHARE").bind(context.id.0).bind(&ids).bind(&connections).fetch_all(&mut **tx).await?;
        if let Some(permission) = permission {
            sqlx::query("SELECT p.id FROM user_preferences p WHERE p.user_context_id=$1 AND p.preference_key IN (SELECT pin->>'key' FROM agent_delegation_permissions d CROSS JOIN LATERAL jsonb_array_elements(d.shared_preferences) pin WHERE d.id=$2) ORDER BY p.id FOR SHARE OF p").bind(context.id.0).bind(permission).fetch_all(&mut **tx).await?;
        }
        for cap in &authority.capabilities {
            for agent in if permission.is_some() {
                vec![child_agent]
            } else {
                vec![child_agent, parent_agent]
            } {
                let granted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_capability_grants g JOIN agent_definitions a ON a.id=g.agent_definition_id JOIN deployment_agent_selections s ON s.agent_definition_id=a.id JOIN external_connections x ON x.id=g.connection_id WHERE g.user_context_id=$1 AND a.owner_user_context_id=$1 AND a.deployment_id=$2 AND a.id=$3 AND g.connection_id=$4 AND g.capability_external_key=$5 AND a.state='enabled' AND g.state='enabled' AND x.user_context_id=$1 AND x.authorization_state='authorized' AND (x.expires_at IS NULL OR x.expires_at>now()) AND (g.capability_external_key=ANY(a.requested_capability_categories) OR '*'=ANY(a.requested_capability_categories)) AND g.capability_external_key=ANY(x.authorized_capabilities) AND (a.template_id IS NULL OR EXISTS(SELECT 1 FROM agent_definitions t WHERE t.id=a.template_id AND t.state='enabled' AND (g.capability_external_key=ANY(t.requested_capability_categories) OR '*'=ANY(t.requested_capability_categories)))))").bind(context.id.0).bind(context.subject.deployment_id.0).bind(agent).bind(cap.connection_id).bind(&cap.capability_external_key).fetch_one(&mut **tx).await?;
                if !granted {
                    return Err(DurableTaskError::NotFound);
                }
            }
            let current=sqlx::query("SELECT e.current_version,jsonb_build_object('endpoint_url',e.endpoint_url,'protocol',e.protocol,'operator_id',e.operator_id,'capability',cap) AS declaration FROM external_connections x JOIN remote_extensions e ON e.id=x.remote_extension_id AND e.user_context_id=x.user_context_id JOIN remote_extension_versions v ON v.extension_id=e.id AND v.version=e.current_version CROSS JOIN LATERAL jsonb_array_elements(v.capabilities) cap WHERE x.id=$1 AND x.user_context_id=$2 AND cap->>'external_key'=$3 AND x.authorization_state='authorized' AND e.lifecycle_state='active' AND e.consent_status='consented' AND e.conformance_status='passed' AND v.conformance_status='passed' AND e.operator_enabled").bind(cap.connection_id).bind(context.id.0).bind(&cap.capability_external_key).fetch_optional(&mut **tx).await?.ok_or(DurableTaskError::NotFound)?;
            if current.get::<i32, _>("current_version") != cap.extension_version
                || hex::encode(Sha256::digest(
                    serde_json::to_vec(&current.get::<Value, _>("declaration"))
                        .map_err(|_| DurableTaskError::Invalid)?,
                )) != cap.declaration_digest
            {
                return Err(DurableTaskError::NotFound);
            }
        }
        if let Some(permission) = permission {
            let pins:Value=sqlx::query_scalar("SELECT shared_preferences FROM agent_delegation_permissions WHERE id=$1 AND user_context_id=$2 AND state='enabled'").bind(permission).bind(context.id.0).fetch_optional(&mut **tx).await?.ok_or(DurableTaskError::NotFound)?;
            for pin in pins.as_array().ok_or(DurableTaskError::Invalid)? {
                let value:Value=sqlx::query_scalar("SELECT value FROM user_preferences WHERE user_context_id=$1 AND preference_key=$2 AND NOT is_sensitive").bind(context.id.0).bind(pin["key"].as_str().ok_or(DurableTaskError::Invalid)?).fetch_optional(&mut **tx).await?.ok_or(DurableTaskError::NotFound)?;
                if pin["digest"] != json!(hex::encode(Sha256::digest(value.to_string().as_bytes())))
                {
                    return Err(DurableTaskError::NotFound);
                }
            }
        }
        Ok(())
    }
    pub async fn validate_assignment_authority(
        &self,
        context: &ResolvedUserContext,
        job: Uuid,
    ) -> Result<(), DurableTaskError> {
        if let Some(run) = self.assignment(context, job).await? {
            self.authorize_run(&run).await?;
        }
        Ok(())
    }
    pub async fn delegate_conversation(
        &self,
        context: &ResolvedUserContext,
        requester: &str,
        r: DelegateRequest,
    ) -> Result<crate::durable_tasks::DurableTask, DurableTaskError> {
        let actor = capture_actor(&self.db, context, Some(requester)).await?;
        let child = capture_actor(&self.db, context, Some(&r.specialist_agent_key)).await?;
        let scope = select_authority(&r.scope, &child.authority)?;
        if r.brief.trim().is_empty() || r.brief.len() > 8192 {
            return Err(DurableTaskError::Invalid);
        }
        let service = DurableTaskService::new(self.db.clone());
        let bound: Option<Uuid> = if let Some(permission) = r.permission_id {
            sqlx::query_scalar("SELECT parent_run_id FROM agent_delegation_permissions WHERE id=$1 AND user_context_id=$2 AND requester_agent_id=$3 AND specialist_agent_id=$4 AND state='enabled' AND NOT used").bind(permission).bind(context.id.0).bind(actor.agent.definition.id).bind(child.agent.definition.id).fetch_optional(self.db.pool()).await?.flatten()
        } else {
            None
        };
        let task = if let Some(job) = bound {
            let parent = self
                .assignment(context, job)
                .await?
                .ok_or(DurableTaskError::NotFound)?;
            if parent.parent_run_id.is_some()
                || parent.actor.agent.definition.id != actor.agent.definition.id
                || parent.instruction != r.brief
                || parent.checkpoint["delegation_request"]["specialist_agent_key"]
                    != r.specialist_agent_key
            {
                return Err(DurableTaskError::NotFound);
            }
            parent.task
        } else {
            service
                .start_reserved(
                    context,
                    crate::durable_tasks::StartTaskRequest {
                        title: format!(
                            "Specialist request: {}",
                            child.agent.definition.display_name
                        ),
                        instruction: r.brief.clone(),
                        agent_external_key: Some(requester.into()),
                    },
                    true,
                )
                .await?
        };
        if r.permission_id.is_none() && !subset(&scope, &actor.authority) {
            let checkpoint = json!({"code":"delegation_consent_required","question":"Allow the named specialist to use the selected tools for this work?","delegation_request":r});
            let mut tx = self.db.pool().begin().await?;
            sqlx::query("SELECT id FROM spans WHERE id=$1 FOR UPDATE")
                .bind(task.id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE jobs SET wait_reason='clarification',checkpoint=$2,available_at=now() WHERE id=$1 AND state='pending'").bind(task.run_id).bind(&checkpoint).execute(&mut *tx).await?;
            sqlx::query("UPDATE spans SET status='waiting_user',execution_result=$2 WHERE id=$1")
                .bind(task.id)
                .bind(json!({"state":"waiting","reason":"clarification","checkpoint":checkpoint}))
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return service.get(context, task.id).await;
        }
        let owner = format!("conversation-delegation:{}", Uuid::new_v4());
        let generation:Option<i64>=sqlx::query_scalar("UPDATE jobs SET state='running',wait_reason=NULL,lease_owner=$2,lease_expires_at=now()+interval '30 seconds',lease_generation=lease_generation+1,attempt_count=attempt_count+1 WHERE id=$1 AND state='pending' AND lease_owner IS NULL AND (wait_reason IS NULL OR (wait_reason='clarification' AND checkpoint->>'code'='delegation_consent_required')) RETURNING lease_generation").bind(task.run_id).bind(&owner).fetch_optional(self.db.pool()).await?;
        let Some(generation) = generation else {
            return Err(DurableTaskError::Conflict);
        };
        let mut parent = self
            .assignment(context, task.run_id)
            .await?
            .ok_or(DurableTaskError::NotFound)?;
        parent.lease_owner = owner;
        parent.lease_generation = generation;
        match self.delegate(&parent, r).await {
            Ok(child) => Ok(child),
            Err(error) => {
                service.cancel(context, task.id).await?;
                Err(error)
            }
        }
    }
    pub async fn create_permission(
        &self,
        context: &ResolvedUserContext,
        r: PermissionRequest,
    ) -> Result<Uuid, DurableTaskError> {
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
        let scope = select_authority(&r.scope, &current.authority)?;
        let shared = self
            .capture_preferences(context, &r.preference_keys)
            .await?;
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
        let id=sqlx::query_scalar("INSERT INTO agent_delegation_permissions(user_context_id,requester_agent_id,specialist_agent_id,scope,parent_run_id,mode,shared_preferences) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id")
   .bind(context.id.0).bind(parent.definition.id).bind(child.definition.id).bind(json!(scope)).bind(r.parent_run_id).bind(if r.parent_run_id.is_some(){"once"}else{"remembered"}).bind(shared).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(id)
    }
    pub async fn list(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<Value>, DurableTaskError> {
        Ok(sqlx::query_scalar("SELECT jsonb_build_object('id',p.id,'requester_agent_key',a.external_key,'specialist_agent_key',b.external_key,'scope',p.scope,'selected_shared_preferences',p.shared_preferences,'mode',p.mode,'parent_run_id',p.parent_run_id,'state',p.state,'used',p.used) FROM agent_delegation_permissions p JOIN agent_definitions a ON a.id=p.requester_agent_id JOIN agent_definitions b ON b.id=p.specialist_agent_id WHERE p.user_context_id=$1 ORDER BY p.created_at DESC LIMIT 100").bind(context.id.0).fetch_all(self.db.pool()).await?)
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
        let row=sqlx::query("SELECT r.agent_id,r.actor_snapshot,r.authority,r.parent_run_id,r.deadline_at,r.tool_calls,r.max_tool_calls,j.attempt_count,j.max_attempts,j.state FROM assigned_task_runs r JOIN jobs j ON j.id=r.job_id WHERE r.job_id=$1 AND r.user_context_id=$2").bind(parent_id).bind(run.context.id.0).fetch_optional(self.db.pool()).await?.ok_or(DurableTaskError::NotFound)?;
        if row.get::<Option<Uuid>, _>("parent_run_id").is_some()
            || matches!(
                row.get::<String, _>("state").as_str(),
                "cancelled" | "failed" | "completed"
            )
        {
            return Err(DurableTaskError::NotFound);
        }
        if row.get::<chrono::DateTime<chrono::Utc>, _>("deadline_at") <= chrono::Utc::now()
            || row.get::<i32, _>("tool_calls") > row.get::<i32, _>("max_tool_calls")
            || row.get::<i32, _>("attempt_count") > row.get::<i32, _>("max_attempts")
        {
            return Err(DurableTaskError::BudgetExceeded);
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
        self.shared_preferences(run).await?;
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
        if r.brief.trim().is_empty() || r.brief.len() > 8192 || parent.parent_run_id.is_some() {
            return Err(DurableTaskError::Invalid);
        }
        DurableTaskService::new(self.db.clone())
            .verify_run(parent)
            .await?;
        let mut actor =
            capture_actor(&self.db, &parent.context, Some(&r.specialist_agent_key)).await?;
        if actor.agent.definition.id == parent.actor.agent.definition.id {
            return Err(DurableTaskError::Invalid);
        }
        actor.authority = select_authority(&r.scope, &actor.authority)?;
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
        sqlx::query("UPDATE jobs SET state='pending',wait_reason='specialist',lease_owner=NULL,lease_expires_at=NULL WHERE id=$1").bind(parent.task.run_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE spans SET status='waiting_user',execution_result=$2 WHERE id=$1").bind(parent.task.id).bind(json!({"state":"waiting","reason":"specialist","checkpoint":{"child_task_id":span}})).execute(&mut *tx).await?;
        tx.commit().await?;
        DurableTaskService::new(self.db.clone())
            .get(&parent.context, span)
            .await
    }
    pub async fn reconcile_children(&self) -> Result<u64, DurableTaskError> {
        // Return only the child's bounded requested work product/status. No transcript,
        // private memory or provider credentials are copied into the parent's run.
        sqlx::query("UPDATE jobs j SET wait_reason='budget' FROM assigned_task_runs r WHERE j.id=r.job_id AND j.state='pending' AND j.wait_reason='specialist' AND r.deadline_at<=now()").execute(self.db.pool()).await?;
        let rows=sqlx::query("SELECT parent.job_id, child.job_id AS child_id, child.result,j.state,j.wait_reason AS child_wait FROM assigned_task_runs child JOIN jobs j ON j.id=child.job_id JOIN assigned_task_runs parent ON parent.job_id=child.parent_run_id JOIN jobs pj ON pj.id=parent.job_id WHERE child.result_received_at IS NULL AND (j.state IN ('completed','failed','cancelled') OR j.wait_reason='budget') AND pj.state='pending' AND pj.wait_reason='specialist' ORDER BY child.job_id LIMIT 20").fetch_all(self.db.pool()).await?;
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
            let permission: Option<Uuid> = sqlx::query_scalar(
                "SELECT delegation_permission_id FROM assigned_task_runs WHERE job_id=$1",
            )
            .bind(child)
            .fetch_one(&mut *tx)
            .await?;
            let allowed = if let Some(id) = permission {
                sqlx::query_scalar::<_,bool>("SELECT state='enabled' FROM agent_delegation_permissions WHERE id=$1 FOR SHARE").bind(id).fetch_one(&mut *tx).await?
            } else {
                true
            };
            let changed=sqlx::query("UPDATE assigned_task_runs SET result_received_at=now() WHERE job_id=$1 AND result_received_at IS NULL").bind(child).execute(&mut *tx).await?.rows_affected();
            if changed == 1 {
                let child_budget =
                    row.get::<Option<String>, _>("child_wait").as_deref() == Some("budget");
                let awakened = if child_budget {
                    0
                } else {
                    sqlx::query("UPDATE jobs SET checkpoint=checkpoint || jsonb_build_object('specialist_result',$2,'child_run_id',$3,'child_state',$4,'specialist_result_is_untrusted',true),wait_reason=NULL WHERE id=$1 AND state='pending' AND wait_reason='specialist' AND EXISTS(SELECT 1 FROM assigned_task_runs p WHERE p.job_id=$1 AND p.deadline_at>now() AND p.tool_calls<=p.max_tool_calls) AND attempt_count<max_attempts").bind(parent).bind(if allowed {result}else{json!({"state":"unavailable","code":"delegation_revoked"})}).bind(child).bind(row.get::<String,_>("state")).execute(&mut *tx).await?.rows_affected()
                };
                if awakened == 0 {
                    sqlx::query("UPDATE jobs SET wait_reason='budget' WHERE id=$1 AND state='pending' AND wait_reason='specialist'").bind(parent).execute(&mut *tx).await?;
                    sqlx::query("UPDATE spans SET status='waiting_user',execution_result=$2 WHERE id=$1").bind(span).bind(json!({"state":"waiting","reason":"budget","checkpoint":{"code":"shared_specialist_budget_exhausted"}})).execute(&mut *tx).await?;
                    tx.commit().await?;
                    continue;
                }
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
fn select_authority(
    selection: &CapabilitySelection,
    available: &RunAuthority,
) -> Result<RunAuthority, DurableTaskError> {
    if selection.capabilities.is_empty() || selection.capabilities.len() > 32 {
        return Err(DurableTaskError::Invalid);
    }
    let mut scope = RunAuthority::default();
    for reference in &selection.capabilities {
        let cap = available
            .capabilities
            .iter()
            .find(|cap| {
                cap.connection_id == reference.connection_id
                    && cap.capability_external_key == reference.capability_external_key
            })
            .ok_or(DurableTaskError::NotFound)?;
        if scope.capabilities.contains(cap) {
            return Err(DurableTaskError::Invalid);
        }
        scope.capabilities.push(cap.clone());
    }
    Ok(scope)
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
            scope: CapabilitySelection::from_authority(&scope),
            permission_id,
        };
        assert!(
            delegation.delegate(&parent, delegate(None)).await.is_err(),
            "specialist grant does not imply parent access"
        );
        sqlx::query("INSERT INTO user_preferences(user_context_id,category,preference_key,value) VALUES($1,'work','report_style','\"brief\"'),($1,'work','unselected','\"private\"')").bind(context.id.0).execute(db.pool()).await.unwrap();
        let metadata = delegation
            .scopes(
                &context,
                &default.definition.external_key,
                &child.definition.external_key,
            )
            .await
            .unwrap();
        assert_eq!(
            metadata["capabilities"][0]["connection_id"],
            json!(connection)
        );
        assert!(
            metadata["preferences"]
                .as_array()
                .unwrap()
                .iter()
                .all(|p| p.get("value").is_none())
        );
        let permission = delegation
            .create_permission(
                &context,
                PermissionRequest {
                    requester_agent_key: default.definition.external_key.clone(),
                    specialist_agent_key: child.definition.external_key.clone(),
                    scope: CapabilitySelection::from_authority(&scope),
                    parent_run_id: Some(parent.task.run_id),
                    preference_keys: vec!["report_style".into()],
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
            Some(crate::durable_tasks::WaitReason::Specialist)
        );
        let child_run = service
            .claim_assigned("child-worker", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child_run.parent_run_id, Some(parent.task.run_id));
        assert!(delegation.authorize_run(&child_run).await.is_ok());
        assert_eq!(
            delegation.shared_preferences(&child_run).await.unwrap(),
            json!({"report_style":"brief"})
        );
        sqlx::query("UPDATE user_preferences SET value='\"verbose\"' WHERE user_context_id=$1 AND preference_key='report_style'").bind(context.id.0).execute(db.pool()).await.unwrap();
        assert!(
            delegation.authorize_run(&child_run).await.is_err(),
            "changing consented preference requires new consent"
        );
        sqlx::query("UPDATE user_preferences SET value='\"brief\"' WHERE user_context_id=$1 AND preference_key='report_style'").bind(context.id.0).execute(db.pool()).await.unwrap();
        assert_eq!(child_run.deadline_at, parent.deadline_at);
        assert_eq!(child_task.parent_task_id, Some(task.id));
        assert_eq!(child_task.root_task_id, task.id);

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
                    scope: CapabilitySelection::from_authority(&scope),
                    parent_run_id: None,
                    preference_keys: vec![],
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
        let details = json!({"fixture":"exact reviewed delegated action"});
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(&details).unwrap()));
        let proposal:Uuid=sqlx::query_scalar("INSERT INTO action_proposals(user_id,user_context_id,span_id,job_id,actor_key,capability,details,details_hash,expires_at,state) VALUES($1,$2,$3,$4,$5,'fixture.write',$6,$7,now()+interval '1 hour','approved') RETURNING id").bind(user).bind(context.id.0).bind(child_task.id).bind(child_task.run_id).bind(&child.definition.external_key).bind(details).bind(&digest).fetch_one(db.pool()).await.unwrap();
        let approval:Uuid=sqlx::query_scalar("INSERT INTO action_approvals(user_id,user_context_id,proposal_id,approved_details_hash) VALUES($1,$2,$3,$4) RETURNING id").bind(user).bind(context.id.0).bind(proposal).bind(digest).fetch_one(db.pool()).await.unwrap();
        let execution:Uuid=sqlx::query_scalar("INSERT INTO executions(user_id,user_context_id,proposal_id,approval_id,idempotency_key) VALUES($1,$2,$3,$4,'delegation-revocation-proof') RETURNING id").bind(user).bind(context.id.0).bind(proposal).bind(approval).fetch_one(db.pool()).await.unwrap();
        delegation.revoke(&context, remembered).await.unwrap();
        let executions = crate::execution::ExecutionCoordinator::new(db.clone());
        assert!(
            executions
                .start(
                    &context,
                    crate::execution::StartExecutionRequest {
                        approval_id: approval,
                        idempotency_key: "new-delegated-start".into()
                    },
                    Utc::now()
                )
                .await
                .is_err()
        );
        assert!(
            executions
                .claim_dispatch(&context, execution, Utc::now())
                .await
                .is_err()
        );
        let state: String = sqlx::query_scalar("SELECT state FROM executions WHERE id=$1")
            .bind(execution)
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(
            state, "pending",
            "revocation prevents the provider dispatch claim"
        );

        assert!(
            delegation
                .validate_assignment_authority(&context, child_run.task.run_id)
                .await
                .is_err(),
            "execution start/dispatch authority guard rejects revoked permission even without a running lease"
        );
        assert!(
            service.enter_tool(&child_run).await.is_err(),
            "revoked remembered access stops subsequent invocations"
        );
        service.cancel(&context, task.id).await.unwrap();
        assert_eq!(
            service.get(&context, child_task.id).await.unwrap().state,
            crate::durable_tasks::RunState::Cancelled
        );
        let remembered = delegation
            .create_permission(
                &context,
                PermissionRequest {
                    requester_agent_key: default.definition.external_key.clone(),
                    specialist_agent_key: child.definition.external_key.clone(),
                    scope: CapabilitySelection::from_authority(&scope),
                    parent_run_id: None,
                    preference_keys: vec![],
                },
            )
            .await
            .unwrap();
        let library = crate::agents::tools::library::AgentLibrary::new(
            Some(db.clone()),
            None,
            context.clone(),
            default.definition.external_key.clone(),
        );
        let discovery = library
            .invoke(crate::agents::tools::library::LibraryRequest::Specialists)
            .await
            .unwrap();
        assert!(
            discovery["specialists"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["agent_external_key"] == child.definition.external_key)
        );
        let response = library
            .invoke(crate::agents::tools::library::LibraryRequest::Delegate {
                specialist_agent_key: child.definition.external_key.clone(),
                brief: "Only report the selected repository count".into(),
                scope: CapabilitySelection::from_authority(&scope),
                permission_id: Some(remembered),
            })
            .await
            .unwrap();
        assert_eq!(response["state"], "awaiting_specialist");
        let repeat = library
            .clone()
            .invoke(crate::agents::tools::library::LibraryRequest::Delegate {
                specialist_agent_key: child.definition.external_key.clone(),
                brief: "Only report the selected repository count".into(),
                scope: CapabilitySelection::from_authority(&scope),
                permission_id: Some(remembered),
            })
            .await
            .unwrap();
        assert_eq!(
            repeat, response,
            "same turn cloned library reuses the exact durable task"
        );
        assert!(matches!(
            library
                .invoke(crate::agents::tools::library::LibraryRequest::Delegate {
                    specialist_agent_key: child.definition.external_key.clone(),
                    brief: "different work".into(),
                    scope: CapabilitySelection::from_authority(&scope),
                    permission_id: Some(remembered)
                })
                .await,
            Err(crate::agents::tools::library::LibraryError::BudgetExhausted)
        ));

        let child_id = Uuid::parse_str(response["task"]["id"].as_str().unwrap()).unwrap();
        assert!(
            service
                .get(&context, child_id)
                .await
                .unwrap()
                .parent_task_id
                .is_some()
        );
        let parent_id = service
            .get(&context, child_id)
            .await
            .unwrap()
            .parent_task_id
            .unwrap();
        sqlx::query("UPDATE jobs SET wait_reason='budget' WHERE span_id=$1")
            .bind(child_id)
            .execute(db.pool())
            .await
            .unwrap();
        delegation.reconcile_children().await.unwrap();
        assert_eq!(
            service.get(&context, parent_id).await.unwrap().wait_reason,
            Some(crate::durable_tasks::WaitReason::Budget)
        );
        assert!(
            service.resume(&context, parent_id, None).await.is_err(),
            "shared budget cannot be reset by resume"
        );
        assert!(service.stop_all(&context).await.unwrap() > 0);
        let consent = delegation
            .delegate_conversation(&context, &default.definition.external_key, delegate(None))
            .await
            .unwrap();
        assert_eq!(
            consent.result["checkpoint"]["code"],
            "delegation_consent_required"
        );
        assert_eq!(
            consent.wait_reason,
            Some(crate::durable_tasks::WaitReason::Clarification)
        );
        let once = delegation
            .create_permission(
                &context,
                PermissionRequest {
                    requester_agent_key: default.definition.external_key.clone(),
                    specialist_agent_key: child.definition.external_key.clone(),
                    scope: CapabilitySelection::from_authority(&scope),
                    parent_run_id: Some(consent.run_id),
                    preference_keys: vec![],
                },
            )
            .await
            .unwrap();
        let authorized = delegation
            .delegate_conversation(
                &context,
                &default.definition.external_key,
                delegate(Some(once)),
            )
            .await
            .unwrap();
        assert_eq!(
            authorized.parent_task_id,
            Some(consent.id),
            "ordinary-chat once approval reuses its waiting root"
        );
        assert!(
            delegation
                .delegate_conversation(
                    &context,
                    &default.definition.external_key,
                    delegate(Some(once))
                )
                .await
                .is_err(),
            "once cannot attach a fresh unrelated root"
        );
        service.stop_all(&context).await.unwrap();

        sqlx::query("INSERT INTO agent_capability_grants(user_context_id,agent_definition_id,connection_id,capability_external_key) VALUES($1,$2,$3,'repository.read')").bind(context.id.0).bind(default.definition.id).bind(connection).execute(db.pool()).await.unwrap();
        let root = service.start(&context, request()).await.unwrap();
        let parent = service
            .claim_assigned("grant-race-parent", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(parent.task.id, root.id);
        let child_task = delegation.delegate(&parent, delegate(None)).await.unwrap();
        let proposal:Uuid=sqlx::query_scalar("INSERT INTO action_proposals(user_id,user_context_id,span_id,job_id,actor_key,capability,details,details_hash,expires_at,state) VALUES($1,$2,$3,$4,$5,'repository.read','{}','race-proof',now()+interval '1 hour','approved') RETURNING id").bind(user).bind(context.id.0).bind(child_task.id).bind(child_task.run_id).bind(&child.definition.external_key).fetch_one(db.pool()).await.unwrap();
        let approval:Uuid=sqlx::query_scalar("INSERT INTO action_approvals(user_id,user_context_id,proposal_id,approved_details_hash) VALUES($1,$2,$3,'race-proof') RETURNING id").bind(user).bind(context.id.0).bind(proposal).fetch_one(db.pool()).await.unwrap();
        let execution:Uuid=sqlx::query_scalar("INSERT INTO executions(user_id,user_context_id,proposal_id,approval_id,idempotency_key) VALUES($1,$2,$3,$4,'parent-grant-race') RETURNING id").bind(user).bind(context.id.0).bind(proposal).bind(approval).fetch_one(db.pool()).await.unwrap();
        delegation
            .validate_assignment_authority(&context, child_task.run_id)
            .await
            .unwrap();
        let mut revoke = db.pool().begin().await.unwrap();
        sqlx::query("UPDATE agent_capability_grants SET state='revoked',revoked_at=now() WHERE user_context_id=$1 AND agent_definition_id=$2 AND connection_id=$3").bind(context.id.0).bind(default.definition.id).bind(connection).execute(&mut *revoke).await.unwrap();
        let coordinator = crate::execution::ExecutionCoordinator::new(db.clone());
        let race_context = context.clone();
        let mut dispatch = tokio::spawn(async move {
            coordinator
                .claim_dispatch(&race_context, execution, Utc::now())
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut dispatch)
                .await
                .is_err(),
            "dispatch must wait for the in-flight parent grant revoke"
        );
        revoke.commit().await.unwrap();
        assert!(
            dispatch.await.unwrap().is_err(),
            "committed revoke wins before dispatch claim"
        );
        let state: String = sqlx::query_scalar("SELECT state FROM executions WHERE id=$1")
            .bind(execution)
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(state, "pending");
        service.stop_all(&context).await.unwrap();
        sqlx::query("WITH tasks AS (INSERT INTO spans(user_id,user_context_id,title,status,execution_type) SELECT $1,$2,'stop-all coverage','planned','interactive' FROM generate_series(1,129) RETURNING id) INSERT INTO jobs(user_id,user_context_id,kind,payload_reference_id,span_id) SELECT $1,$2,'execute_span',id,id FROM tasks").bind(user).bind(context.id.0).execute(db.pool()).await.unwrap();
        assert_eq!(
            service.stop_all(&context).await.unwrap(),
            129,
            "Stop all does not silently truncate at 128"
        );
        let active:i64=sqlx::query_scalar("SELECT count(*) FROM jobs WHERE user_context_id=$1 AND kind='execute_span' AND state IN ('pending','running')").bind(context.id.0).fetch_one(db.pool()).await.unwrap();
        assert_eq!(active, 0);
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
