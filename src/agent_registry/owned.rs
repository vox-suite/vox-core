use super::*;
use crate::identity::ResolvedUserContext;
use sqlx::Row;

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentMutation {
    Create {
        name: String,
        instructions: String,
    },
    Update {
        agent_key: String,
        name: String,
        instructions: String,
        expected_version: i32,
    },
    Archive {
        agent_key: String,
    },
}

impl AgentRegistry {
    /// Serializing on the context row makes default provisioning and the roster
    /// limit atomic across API processes. Templates are copied, never mutated.
    pub async fn owned_for_context(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<SelectedAgent>, AgentRegistryError> {
        let mut tx = self.db.pool().begin().await?;
        lock_context(&mut tx, context).await?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_definitions WHERE owner_user_context_id=$1 AND is_default)")
            .bind(context.id.0).fetch_one(&mut *tx).await?;
        if !exists {
            insert_owned(&mut tx, context, "general", "Personal Assistant", "Help the user with everyday questions and explicitly assigned work. Discover relevant permitted tools and skills as needed. Never infer account access or action approval from instructions.", true).await?;
        }
        tx.commit().await?;
        let rows = sqlx::query("SELECT a.*, c.id AS model_id,c.version AS model_version,c.model_adapter,c.model FROM agent_definitions a JOIN deployment_agent_selections s ON s.agent_definition_id=a.id AND s.deployment_id=a.deployment_id JOIN agent_model_configurations c ON c.id=s.model_configuration_id AND c.agent_definition_id=a.id JOIN agent_definitions t ON t.id=a.template_id AND t.state='enabled' WHERE a.owner_user_context_id=$1 AND a.deployment_id=$2 AND a.state='enabled' ORDER BY a.is_default DESC,a.created_at,a.id")
            .bind(context.id.0).bind(context.subject.deployment_id.0).fetch_all(self.db.pool()).await?;
        Ok(rows
            .into_iter()
            .map(|r| SelectedAgent {
                definition: AgentDefinition {
                    id: r.get("id"),
                    deployment_id: context.subject.deployment_id,
                    external_key: r.get("external_key"),
                    purpose: r.get("purpose"),
                    requested_capability_categories: r.get("requested_capability_categories"),
                    display_name: r.get("display_name"),
                    is_default: r.get("is_default"),
                    instruction_version: r.get("instruction_version"),
                },
                model_configuration: ModelConfiguration {
                    id: r.get("model_id"),
                    version: r.get("model_version"),
                    model_adapter: r.get("model_adapter"),
                    model: r.get("model"),
                },
            })
            .collect())
    }

    pub async fn mutate_owned(
        &self,
        context: &ResolvedUserContext,
        mutation: AgentMutation,
    ) -> Result<(), AgentRegistryError> {
        self.owned_for_context(context).await?;
        let mut tx = self.db.pool().begin().await?;
        lock_context(&mut tx, context).await?;
        match mutation {
            AgentMutation::Create { name, instructions } => {
                let name = normalize(&name, 100).ok_or(AgentRegistryError::InvalidDefinition)?;
                let instructions =
                    normalize(&instructions, 2048).ok_or(AgentRegistryError::InvalidDefinition)?;
                let count:i64=sqlx::query_scalar("SELECT count(*) FROM agent_definitions WHERE owner_user_context_id=$1 AND state='enabled'").bind(context.id.0).fetch_one(&mut *tx).await?;
                if count >= 32 {
                    return Err(AgentRegistryError::InvalidDefinition);
                }
                insert_owned(
                    &mut tx,
                    context,
                    &Uuid::new_v4().to_string(),
                    &name,
                    &instructions,
                    false,
                )
                .await?;
            }
            AgentMutation::Update {
                agent_key,
                name,
                instructions,
                expected_version,
            } => {
                let name = normalize(&name, 100).ok_or(AgentRegistryError::InvalidDefinition)?;
                let instructions =
                    normalize(&instructions, 2048).ok_or(AgentRegistryError::InvalidDefinition)?;
                let id:Uuid=sqlx::query_scalar("UPDATE agent_definitions SET display_name=$3,purpose=$4,instruction_version=instruction_version+1,updated_at=now() WHERE owner_user_context_id=$1 AND external_key=$2 AND state='enabled' AND instruction_version=$5 RETURNING id")
                    .bind(context.id.0).bind(agent_key).bind(name).bind(&instructions).bind(expected_version).fetch_optional(&mut *tx).await?.ok_or(AgentRegistryError::NotFound)?;
                sqlx::query("INSERT INTO agent_instruction_versions(agent_id,version,instructions) VALUES($1,$2,$3)").bind(id).bind(expected_version+1).bind(instructions).execute(&mut *tx).await?;
            }
            AgentMutation::Archive { agent_key } => {
                let id:Uuid=sqlx::query_scalar("UPDATE agent_definitions SET state='disabled',updated_at=now() WHERE owner_user_context_id=$1 AND external_key=$2 AND state='enabled' AND NOT is_default RETURNING id")
                    .bind(context.id.0).bind(agent_key).fetch_optional(&mut *tx).await?.ok_or(AgentRegistryError::NotFound)?;
                sqlx::query("UPDATE agent_capability_grants SET state='revoked',revoked_at=now(),updated_at=now() WHERE user_context_id=$1 AND agent_definition_id=$2 AND state='enabled'")
                    .bind(context.id.0).bind(id).execute(&mut *tx).await?;
                sqlx::query("UPDATE skill_agent_enablements SET enabled=false,updated_at=now() WHERE user_context_id=$1 AND agent_definition_id=$2")
                    .bind(context.id.0).bind(id).execute(&mut *tx).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

async fn lock_context(
    tx: &mut Transaction<'_, Postgres>,
    context: &ResolvedUserContext,
) -> Result<(), AgentRegistryError> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM user_contexts WHERE id=$1 AND user_id=$2 AND deployment_id=$3 FOR UPDATE",
    )
    .bind(context.id.0)
    .bind(context.user_id.0)
    .bind(context.subject.deployment_id.0)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(AgentRegistryError::NotFound)?;
    Ok(())
}

async fn insert_owned(
    tx: &mut Transaction<'_, Postgres>,
    context: &ResolvedUserContext,
    key: &str,
    name: &str,
    instructions: &str,
    is_default: bool,
) -> Result<(), AgentRegistryError> {
    let template=sqlx::query("SELECT a.id,c.model_adapter,c.model,c.configuration,a.requested_capability_categories FROM agent_definitions a JOIN deployment_agent_selections s ON s.agent_definition_id=a.id AND s.deployment_id=a.deployment_id JOIN agent_model_configurations c ON c.id=s.model_configuration_id AND c.agent_definition_id=a.id WHERE a.deployment_id=$1 AND a.external_key='general' AND a.owner_user_context_id IS NULL AND a.state='enabled' FOR SHARE OF a,s")
        .bind(context.subject.deployment_id.0).fetch_optional(&mut **tx).await?.ok_or(AgentRegistryError::NotFound)?;
    let id:Uuid=sqlx::query_scalar("INSERT INTO agent_definitions(deployment_id,external_key,purpose,requested_capability_categories,owner_user_context_id,template_id,display_name,is_default) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id")
        .bind(context.subject.deployment_id.0).bind(key).bind(instructions).bind(template.get::<Vec<String>,_>("requested_capability_categories")).bind(context.id.0).bind(template.get::<Uuid,_>("id")).bind(name).bind(is_default).fetch_one(&mut **tx).await?;
    let model:Uuid=sqlx::query_scalar("INSERT INTO agent_model_configurations(agent_definition_id,version,model_adapter,model,configuration) VALUES($1,1,$2,$3,$4) RETURNING id")
        .bind(id).bind(template.get::<String,_>("model_adapter")).bind(template.get::<String,_>("model")).bind(template.get::<Value,_>("configuration")).fetch_one(&mut **tx).await?;
    sqlx::query("INSERT INTO deployment_agent_selections(deployment_id,agent_definition_id,model_configuration_id) VALUES($1,$2,$3)").bind(context.subject.deployment_id.0).bind(id).bind(model).execute(&mut **tx).await?;
    sqlx::query(
        "INSERT INTO agent_instruction_versions(agent_id,version,instructions) VALUES($1,1,$2)",
    )
    .bind(id)
    .bind(instructions)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires an empty isolated TEST_DATABASE_URL with pgvector"]
    async fn ownership_transition_expires_unused_decisions_and_preserves_outcomes() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        let migrations = sqlx::migrate!("./migrations");
        for migration in migrations
            .iter()
            .filter(|migration| migration.version < 20260930000000)
        {
            sqlx::raw_sql(&migration.sql)
                .execute(db.pool())
                .await
                .unwrap();
        }
        let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let context = crate::identity::IdentityService::new(db.clone())
            .resolve_for_user(user)
            .await
            .unwrap();
        let mut cases = Vec::new();
        for state in [
            None,
            Some("pending"),
            Some("reconciling"),
            Some("succeeded"),
        ] {
            let proposal: Uuid = sqlx::query_scalar("INSERT INTO action_proposals(user_id,user_context_id,actor_key,capability,details,details_hash,state,expires_at) VALUES($1,$2,'general','fixture.write','{}','fixture','approved',now()+interval '1 day') RETURNING id")
                .bind(user).bind(context.id.0).fetch_one(db.pool()).await.unwrap();
            let approval: Uuid = sqlx::query_scalar("INSERT INTO action_approvals(proposal_id,user_id,user_context_id,approved_details_hash) VALUES($1,$2,$3,'fixture') RETURNING id")
                .bind(proposal).bind(user).bind(context.id.0).fetch_one(db.pool()).await.unwrap();
            let execution = if let Some(state) = state {
                Some(sqlx::query_scalar::<_,Uuid>("INSERT INTO executions(user_id,user_context_id,proposal_id,approval_id,idempotency_key,state,confirmation_evidence) VALUES($1,$2,$3,$4,$5,$6,'{\"proof\":\"retained\"}') RETURNING id")
                    .bind(user).bind(context.id.0).bind(proposal).bind(approval).bind(Uuid::new_v4().to_string()).bind(state).fetch_one(db.pool()).await.unwrap())
            } else {
                None
            };
            cases.push((proposal, execution, state));
        }
        let transition = migrations
            .iter()
            .find(|migration| migration.version == 20260930000000)
            .unwrap();
        sqlx::raw_sql(&transition.sql)
            .execute(db.pool())
            .await
            .unwrap();
        for (proposal, execution, previous) in cases {
            let state: String =
                sqlx::query_scalar("SELECT state FROM action_proposals WHERE id=$1")
                    .bind(proposal)
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
            assert_eq!(
                state,
                if execution.is_none() {
                    "expired"
                } else {
                    "approved"
                }
            );
            if let Some(execution) = execution {
                let row =
                    sqlx::query("SELECT state,confirmation_evidence FROM executions WHERE id=$1")
                        .bind(execution)
                        .fetch_one(db.pool())
                        .await
                        .unwrap();
                assert_eq!(
                    row.get::<String, _>("state"),
                    if previous == Some("pending") {
                        "failed"
                    } else {
                        previous.unwrap()
                    }
                );
                let evidence: Value = row.get("confirmation_evidence");
                assert_eq!(evidence["proof"], "retained");
                if previous == Some("pending") {
                    assert_eq!(evidence["dispatched"], false);
                }
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated TEST_DATABASE_URL with pgvector"]
    async fn concurrent_defaults_and_specialists_are_owned_and_versioned() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").expect("isolated database"))
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let identity = crate::identity::IdentityService::new(db.clone());
        let registry = AgentRegistry::new(db.clone());
        let first: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let second: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let a = identity.resolve_for_user(first).await.unwrap();
        let b = identity.resolve_for_user(second).await.unwrap();
        let concurrent =
            futures_util::future::join_all((0..8).map(|_| registry.owned_for_context(&a))).await;
        let id = concurrent[0].as_ref().unwrap()[0].definition.id;
        for result in concurrent {
            let agents = result.unwrap();
            assert_eq!(agents.len(), 1);
            assert_eq!(agents[0].definition.id, id);
            assert!(agents[0].definition.is_default);
        }
        let other = registry.owned_for_context(&b).await.unwrap();
        assert_ne!(id, other[0].definition.id);
        registry
            .mutate_owned(
                &a,
                AgentMutation::Create {
                    name: "Engineering".into(),
                    instructions: "Review source code.".into(),
                },
            )
            .await
            .unwrap();
        let owned = registry.owned_for_context(&a).await.unwrap();
        let specialist = &owned[1].definition;
        assert!(
            registry
                .selected_for_context(&b, &specialist.external_key)
                .await
                .is_err()
        );
        assert!(
            registry
                .mutate_owned(
                    &b,
                    AgentMutation::Archive {
                        agent_key: specialist.external_key.clone()
                    }
                )
                .await
                .is_err()
        );
        registry
            .mutate_owned(
                &a,
                AgentMutation::Update {
                    agent_key: specialist.external_key.clone(),
                    name: "Engineering".into(),
                    instructions: "Review source code and cite files.".into(),
                    expected_version: 1,
                },
            )
            .await
            .unwrap();
        assert!(
            registry
                .mutate_owned(
                    &a,
                    AgentMutation::Update {
                        agent_key: specialist.external_key.clone(),
                        name: "Stale".into(),
                        instructions: "Stale edit".into(),
                        expected_version: 1
                    }
                )
                .await
                .is_err()
        );
        let versions: i64 =
            sqlx::query_scalar("SELECT count(*) FROM agent_instruction_versions WHERE agent_id=$1")
                .bind(specialist.id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(versions, 2);
        assert!(
            registry
                .mutate_owned(
                    &a,
                    AgentMutation::Archive {
                        agent_key: "general".into()
                    }
                )
                .await
                .is_err()
        );
        registry
            .mutate_owned(
                &a,
                AgentMutation::Archive {
                    agent_key: specialist.external_key.clone(),
                },
            )
            .await
            .unwrap();
        assert!(
            registry
                .selected_for_context(&a, &specialist.external_key)
                .await
                .is_err()
        );
    }
}
