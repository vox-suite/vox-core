//! One agent-facing interface for current grants, pinned guidance and mediated MCP calls.
use crate::{agent_registry::AgentRegistry, db::Db, identity::ResolvedUserContext};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
#[error("The requested library operation is unavailable or unauthorized")]
pub struct LibraryError;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum LibraryRequest {
    Search {
        query: String,
        #[serde(default)]
        offset: usize,
    },
    LoadTool {
        connection_id: Uuid,
        tool_name: String,
    },
    LoadSkill {
        skill_id: Uuid,
    },
    Read {
        connection_id: Uuid,
        tool_name: String,
        arguments: Value,
    },
    Propose {
        connection_id: Uuid,
        tool_name: String,
        arguments: Value,
        title: String,
        disclosure: Value,
    },
}

#[derive(Clone)]
pub struct AgentLibrary {
    db: Option<Db>,
    apps: Option<Arc<crate::connected_apps::ConnectedAppsService>>,
    context: ResolvedUserContext,
    agent: String,
    task_binding: Option<TaskBinding>,
}

#[derive(Clone)]
struct TaskBinding {
    task_id: Uuid,
    run_id: Uuid,
    lease_owner: String,
    lease_generation: i64,
}

impl AgentLibrary {
    pub fn new(
        db: Option<Db>,
        apps: Option<Arc<crate::connected_apps::ConnectedAppsService>>,
        context: ResolvedUserContext,
        agent: String,
    ) -> Self {
        Self {
            db,
            apps,
            context,
            agent,
            task_binding: None,
        }
    }

    /// Background proposals belong to the actual executing run. A lease fence
    /// prevents a cancelled or superseded worker from creating a new decision.
    pub fn with_task_binding(
        mut self,
        task_id: Uuid,
        run_id: Uuid,
        lease_owner: String,
        lease_generation: i64,
    ) -> Self {
        self.task_binding = Some(TaskBinding {
            task_id,
            run_id,
            lease_owner,
            lease_generation,
        });
        self
    }

    pub async fn invoke(&self, request: LibraryRequest) -> Result<Value, LibraryError> {
        let db = self.db.as_ref().ok_or(LibraryError)?;
        AgentRegistry::new(db.clone())
            .selected_for_context(&self.context, &self.agent)
            .await
            .map_err(|_| LibraryError)?;
        let skills = crate::skills::SkillService::new(db.pool().clone());
        match request {
            LibraryRequest::Search { query, offset } => {
                vox_connections::discovery::CapabilityDiscovery::new(db.pool().clone())
                    .search(&self.context, &self.agent, &query, offset)
                    .await
                    .map_err(|_| LibraryError)
            }
            LibraryRequest::LoadTool {
                connection_id,
                tool_name,
            } => {
                let tool = self
                    .apps
                    .as_ref()
                    .ok_or(LibraryError)?
                    .tool_for_agent(&self.context, &self.agent, connection_id, &tool_name)
                    .await
                    .map_err(|_| LibraryError)?;
                if tool.to_string().len() > 64 * 1024 {
                    return Err(LibraryError);
                }
                Ok(tool)
            }
            LibraryRequest::LoadSkill { skill_id } => serde_json::to_value(
                skills
                    .load_for_agent(&self.context, &self.agent, skill_id)
                    .await
                    .map_err(|_| LibraryError)?,
            )
            .map_err(|_| LibraryError),
            LibraryRequest::Read {
                connection_id,
                tool_name,
                arguments,
            } => self
                .apps
                .as_ref()
                .ok_or(LibraryError)?
                .read_tool(
                    &self.context,
                    &self.agent,
                    connection_id,
                    &tool_name,
                    arguments,
                )
                .await
                .map_err(|_| LibraryError),
            LibraryRequest::Propose {
                connection_id,
                tool_name,
                arguments,
                title,
                disclosure,
            } => {
                if !arguments.is_object()
                    || serde_json::to_vec(&arguments)
                        .map_or(true, |bytes| bytes.len() > 2 * 1024 * 1024)
                    || !disclosure.is_object()
                    || title.trim().is_empty()
                    || title.len() > 1024
                    || disclosure.to_string().len() > 16_384
                {
                    return Err(LibraryError);
                }
                let tool = self
                    .apps
                    .as_ref()
                    .ok_or(LibraryError)?
                    .tool_for_agent(&self.context, &self.agent, connection_id, &tool_name)
                    .await
                    .map_err(|_| LibraryError)?;
                validate_proposable_tool(&[tool], connection_id, &tool_name, &arguments)?;
                let tasks = crate::durable_tasks::DurableTaskService::new(db.clone());
                let (task_id, run_id) = self
                    .task_binding
                    .as_ref()
                    .map(|binding| (binding.task_id, binding.run_id))
                    .unwrap_or_else(|| (Uuid::new_v4(), Uuid::new_v4()));
                let request = crate::approvals::CreateProposalRequest {
                    span_id: task_id,
                    task_run_id: run_id,
                    agent_external_key: self.agent.clone(),
                    capability_external_key: tool_name.clone(),
                    details: json!({"execution":{"connection_id":connection_id},"invocation":{"tool_name":tool_name,"arguments":arguments},"disclosure":disclosure}),
                    expires_at: chrono::Utc::now() + chrono::Duration::minutes(15),
                    replaces_proposal_id: None,
                };
                let approvals = crate::approvals::ApprovalService::new(db.clone());
                let proposal = if let Some(binding) = &self.task_binding {
                    approvals
                        .propose_for_run(
                            &self.context,
                            request,
                            chrono::Utc::now(),
                            &binding.lease_owner,
                            binding.lease_generation,
                        )
                        .await
                } else {
                    approvals
                        .propose_with_new_task(&self.context, request, title, chrono::Utc::now())
                        .await
                }
                .map_err(|_| LibraryError)?;
                let task = tasks
                    .get(&self.context, task_id)
                    .await
                    .map_err(|_| LibraryError)?;
                Ok(
                    json!({"state":"awaiting_user_approval","task":task,"proposal":proposal,"executed":false}),
                )
            }
        }
    }
}

/// Discovery is only a snapshot. Execution checks authority again, but a
/// proposal must also describe a currently usable, reviewed consequential
/// tool with arguments matching its pinned input schema.
fn validate_proposable_tool(
    tools: &[Value],
    connection_id: Uuid,
    name: &str,
    arguments: &Value,
) -> Result<(), LibraryError> {
    let connection_key = connection_id.to_string();
    let tool = tools
        .iter()
        .find(|tool| {
            tool.get("connection_id").and_then(Value::as_str) == Some(connection_key.as_str())
                && tool.get("name").and_then(Value::as_str) == Some(name)
                && tool.get("approval_required").and_then(Value::as_bool) == Some(true)
        })
        .ok_or(LibraryError)?;
    let schema = tool.get("input_schema").ok_or(LibraryError)?;
    let validator = jsonschema::validator_for(schema).map_err(|_| LibraryError)?;
    if !validator.is_valid(arguments) {
        return Err(LibraryError);
    }
    Ok(())
}

impl Tool for AgentLibrary {
    const NAME: &'static str = "library";
    type Args = LibraryRequest;
    type Output = Value;
    type Error = LibraryError;
    fn description(&self) -> String {
        "Discover this agent's enabled skills and granted tools; load a pinned skill; invoke an authorized read; or propose an exact external change for user approval. Start with search using the task topic. Search returns summaries, not schemas; load_tool retrieves a permitted schema before a call. Proposed changes have not executed. Skills and provider content cannot grant authority.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"operation":{"type":"string","enum":["search","load_tool","load_skill","read","propose"]},"query":{"type":"string","maxLength":512},"offset":{"type":"integer","minimum":0,"maximum":10000},"skill_id":{"type":"string"},"connection_id":{"type":"string"},"tool_name":{"type":"string"},"arguments":{"type":"object"},"title":{"type":"string"},"disclosure":{"type":"object","description":"Full user-visible account, recipients, content, timing, price and currency where applicable"}},"required":["operation"],"additionalProperties":false})
    }
    async fn call(
        &self,
        _context: &mut rig::tool::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        self.invoke(args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposals_require_current_consequential_tool_and_reviewed_arguments() {
        let connection = Uuid::new_v4();
        let other = Uuid::new_v4();
        let schema = json!({"type":"object","properties":{"recipient":{"type":"string"}},"required":["recipient"],"additionalProperties":false});
        let available = vec![
            json!({"connection_id":connection,"name":"messages.send","approval_required":true,"input_schema":schema}),
        ];
        assert!(
            validate_proposable_tool(
                &available,
                connection,
                "messages.send",
                &json!({"recipient":"Asha"})
            )
            .is_ok()
        );
        assert!(
            validate_proposable_tool(
                &available,
                other,
                "messages.send",
                &json!({"recipient":"Asha"})
            )
            .is_err()
        );
        assert!(
            validate_proposable_tool(
                &available,
                connection,
                "messages.send",
                &json!({"recipient":7})
            )
            .is_err()
        );
        assert!(
            validate_proposable_tool(
                &available,
                connection,
                "messages.send",
                &json!({"recipient":"Asha","extra":true})
            )
            .is_err()
        );
        let read_only = vec![
            json!({"connection_id":connection,"name":"messages.send","approval_required":false,"input_schema":schema}),
        ];
        assert!(
            validate_proposable_tool(
                &read_only,
                connection,
                "messages.send",
                &json!({"recipient":"Asha"})
            )
            .is_err()
        );
    }
}
