//! Agent guidance and explicitly consented specialist delegation.
use crate::{agent_registry::AgentRegistry, db::Db, identity::ResolvedUserContext};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("The requested library operation is unavailable or unauthorized")]
    Unavailable,
    #[error(
        "The capability context budget is exhausted; refine the task or continue in a new turn"
    )]
    BudgetExhausted,
}

/// Only governed tools record a task handle; HTTP re-resolves it in the
/// authenticated context before projecting a public task.
#[derive(Clone, Debug, Default)]
pub struct TaskCapture(Arc<Mutex<Option<Uuid>>>);
impl PartialEq for TaskCapture {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for TaskCapture {}
impl TaskCapture {
    pub(crate) fn record(&self, id: Uuid) {
        if let Ok(mut saved) = self.0.lock() {
            *saved = Some(id)
        }
    }
    pub fn task_id(&self) -> Option<Uuid> {
        self.0.lock().ok().and_then(|saved| *saved)
    }
}

/// UTF-8 bytes conservatively bound tokens without assuming a model tokenizer.
/// The context budget is shared by cloned tools for the same model turn.
#[derive(Clone, Copy)]
pub struct LibraryLimits {
    pub max_tools: usize,
    pub max_skills: usize,
    pub max_context_bytes: usize,
}
impl Default for LibraryLimits {
    fn default() -> Self {
        Self {
            max_tools: 8,
            max_skills: 3,
            max_context_bytes: 12_000,
        }
    }
}
#[derive(Default)]
struct LibraryBudget {
    tools: HashSet<(Uuid, String)>,
    skills: HashSet<Uuid>,
    bytes: usize,
    delegation: Option<(Value, Option<Value>)>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum LibraryRequest {
    Specialists,
    SpecialistScope {
        specialist_agent_key: String,
    },
    Delegate {
        specialist_agent_key: String,
        brief: String,
        scope: crate::delegation::CapabilitySelection,
        permission_id: Option<Uuid>,
    },
    Search {
        query: String,
        #[serde(default)]
        offset: usize,
    },
    LoadSkill {
        skill_id: Uuid,
    },
}

#[derive(Clone)]
pub struct AgentLibrary {
    db: Option<Db>,

    context: ResolvedUserContext,
    agent: String,
    task_binding: bool,
    task_capture: TaskCapture,
    limits: LibraryLimits,
    budget: Arc<Mutex<LibraryBudget>>,
}

impl AgentLibrary {
    pub fn new(db: Option<Db>, context: ResolvedUserContext, agent: String) -> Self {
        Self {
            db,

            context,
            agent,
            task_binding: false,
            task_capture: TaskCapture::default(),
            limits: LibraryLimits::default(),
            budget: Arc::default(),
        }
    }

    /// Background proposals belong to the actual executing run. A lease fence
    /// prevents a cancelled or superseded worker from creating a new decision.
    pub fn with_task_binding(
        mut self,
        _task_id: Uuid,
        _run_id: Uuid,
        _lease_owner: String,
        _lease_generation: i64,
    ) -> Self {
        self.task_binding = true;
        self
    }

    pub fn with_task_capture(mut self, capture: TaskCapture) -> Self {
        self.task_capture = capture;
        self
    }

    pub fn with_limits(mut self, limits: LibraryLimits) -> Self {
        self.limits = limits;
        self
    }

    fn charge(
        &self,
        content: &Value,
        tool: Option<(Uuid, String)>,
        skill: Option<Uuid>,
    ) -> Result<(), LibraryError> {
        let bytes = serde_json::to_vec(content)
            .map_err(|_| LibraryError::Unavailable)?
            .len();
        let mut budget = self.budget.lock().map_err(|_| LibraryError::Unavailable)?;
        if budget
            .bytes
            .checked_add(bytes)
            .is_none_or(|total| total > self.limits.max_context_bytes)
            || tool.as_ref().is_some_and(|key| {
                !budget.tools.contains(key) && budget.tools.len() >= self.limits.max_tools
            })
            || skill.is_some_and(|key| {
                !budget.skills.contains(&key) && budget.skills.len() >= self.limits.max_skills
            })
        {
            return Err(LibraryError::BudgetExhausted);
        }
        budget.bytes += bytes;
        if let Some(tool) = tool {
            budget.tools.insert(tool);
        }
        if let Some(skill) = skill {
            budget.skills.insert(skill);
        }
        Ok(())
    }

    pub async fn invoke(&self, request: LibraryRequest) -> Result<Value, LibraryError> {
        let db = self.db.as_ref().ok_or(LibraryError::Unavailable)?;
        AgentRegistry::new(db.clone())
            .selected_for_context(&self.context, &self.agent)
            .await
            .map_err(|_| LibraryError::Unavailable)?;
        let skills = crate::skills::SkillService::new(db.pool().clone());
        match request {
            LibraryRequest::SpecialistScope {
                specialist_agent_key,
            } => {
                let value = crate::delegation::DelegationService::new(db.clone())
                    .scopes(&self.context, &self.agent, &specialist_agent_key)
                    .await
                    .map_err(|_| LibraryError::Unavailable)?;
                self.charge(&value, None, None)?;
                Ok(value)
            }
            LibraryRequest::Specialists => {
                let value = crate::delegation::DelegationService::new(db.clone())
                    .specialists(&self.context, &self.agent)
                    .await
                    .map_err(|_| LibraryError::Unavailable)?;
                self.charge(&value, None, None)?;
                Ok(value)
            }
            LibraryRequest::Delegate {
                specialist_agent_key,
                brief,
                scope,
                permission_id,
            } => {
                if self.task_binding {
                    return Err(LibraryError::Unavailable);
                }
                let identity = json!({"specialist_agent_key":specialist_agent_key,"brief":brief,"scope":scope,"permission_id":permission_id});
                {
                    let mut budget = self.budget.lock().map_err(|_| LibraryError::Unavailable)?;
                    if let Some((prior, response)) = &budget.delegation {
                        if prior == &identity {
                            return response.clone().ok_or(LibraryError::Unavailable);
                        }
                        return Err(LibraryError::BudgetExhausted);
                    }
                    budget.delegation = Some((identity, None));
                }
                let child = crate::delegation::DelegationService::new(db.clone())
                    .delegate_conversation(
                        &self.context,
                        &self.agent,
                        crate::delegation::DelegateRequest {
                            specialist_agent_key,
                            brief,
                            scope,
                            permission_id,
                        },
                    )
                    .await
                    .map_err(|_| LibraryError::Unavailable)?;
                self.task_capture.record(child.id);
                let response = json!({"state":if child.result["checkpoint"]["code"]=="delegation_consent_required" {"requires_delegation_consent"}else{"awaiting_specialist"},"task":child,"authority":"No external action has executed; consequential actions require exact proposal approval."});
                if let Some((_, saved)) = &mut self
                    .budget
                    .lock()
                    .map_err(|_| LibraryError::Unavailable)?
                    .delegation
                {
                    *saved = Some(response.clone());
                }
                Ok(response)
            }
            LibraryRequest::Search { query, offset } => {
                let value = serde_json::to_value(
                    skills
                        .effective(&self.context, &self.agent)
                        .await
                        .map_err(|_| LibraryError::Unavailable)?,
                )
                .map_err(|_| LibraryError::Unavailable)?;
                let results: Vec<Value> = value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|item| {
                        item.to_string()
                            .to_lowercase()
                            .contains(&query.to_lowercase())
                    })
                    .skip(offset.min(64))
                    .take(20)
                    .cloned()
                    .collect();
                let response = json!({"skills":results});
                self.charge(&response, None, None)?;
                Ok(response)
            }
            LibraryRequest::LoadSkill { skill_id } => {
                let skill = serde_json::to_value(
                    skills
                        .load_for_agent(&self.context, &self.agent, skill_id)
                        .await
                        .map_err(|_| LibraryError::Unavailable)?,
                )
                .map_err(|_| LibraryError::Unavailable)?;
                self.charge(&skill, None, Some(skill_id))?;
                Ok(skill)
            }
        }
    }
}
impl Tool for AgentLibrary {
    const NAME: &'static str = "library";
    type Args = LibraryRequest;
    type Output = Value;
    type Error = LibraryError;
    fn description(&self) -> String {
        "Discover enabled skills and owned specialists, load a skill, or delegate scoped work. Connected account reads use read_connected_app and user preferences. Skills cannot grant account access.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"operation":{"type":"string","enum":["search","specialists","specialist_scope","load_skill","delegate"]},"specialist_agent_key":{"type":"string"},"brief":{"type":"string","maxLength":8192},"scope":{"type":"object","properties":{"capabilities":{"type":"array","maxItems":0,"items":{"type":"object","properties":{"connection_id":{"type":"string","format":"uuid"},"capability_external_key":{"type":"string"}},"required":["connection_id","capability_external_key"],"additionalProperties":false}}},"required":["capabilities"],"additionalProperties":false},"permission_id":{"type":["string","null"]},"query":{"type":"string","maxLength":512},"offset":{"type":"integer","minimum":0,"maximum":10000},"skill_id":{"type":"string"}},"required":["operation"],"additionalProperties":false})
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

    fn bounded_library(limits: LibraryLimits) -> AgentLibrary {
        use crate::identity::*;
        let context = ResolvedUserContext {
            id: UserContextId(Uuid::new_v4()),
            user_id: UserId(Uuid::new_v4()),
            subject: UserContextSubject {
                deployment_id: DeploymentId(Uuid::new_v4()),
                host_app_id: HostAppId(Uuid::new_v4()),
                organization_id: None,
                host_user_id: "budget-fixture".into(),
            },
        };
        AgentLibrary::new(None, context, "fixture".into()).with_limits(limits)
    }

    #[test]
    fn clones_share_context_limits_and_repeated_content_is_charged() {
        let library = bounded_library(LibraryLimits {
            max_tools: 1,
            max_skills: 1,
            max_context_bytes: 100,
        });
        let clone = library.clone();
        let tool = (Uuid::new_v4(), "read".into());
        let content = json!({"type":"object"});
        library.charge(&content, Some(tool.clone()), None).unwrap();
        clone.charge(&content, Some(tool), None).unwrap();
        assert!(matches!(
            clone.charge(&content, Some((Uuid::new_v4(), "other".into())), None),
            Err(LibraryError::BudgetExhausted)
        ));
        let skill = Uuid::new_v4();
        library.charge(&json!({}), None, Some(skill)).unwrap();
        assert!(matches!(
            clone.charge(&json!({}), None, Some(Uuid::new_v4())),
            Err(LibraryError::BudgetExhausted)
        ));
        assert!(matches!(
            clone.charge(&json!("x".repeat(100)), None, None),
            Err(LibraryError::BudgetExhausted)
        ));
        // A rejected charge cannot consume the remaining budget.
        clone.charge(&json!({}), None, Some(skill)).unwrap();
    }

    #[test]
    fn byte_budget_has_an_exact_boundary_and_counts_utf8() {
        let content = json!("💡");
        let bytes = serde_json::to_vec(&content).unwrap().len();
        let library = bounded_library(LibraryLimits {
            max_tools: 8,
            max_skills: 3,
            max_context_bytes: bytes,
        });
        library.charge(&content, None, None).unwrap();
        assert!(matches!(
            library.charge(&json!({}), None, None),
            Err(LibraryError::BudgetExhausted)
        ));
    }

    #[test]
    fn arbitrary_connector_operations_are_rejected() {
        for operation in ["read", "propose", "load_tool"] {
            assert!(serde_json::from_value::<LibraryRequest>(json!({"operation":operation,"connection_id":Uuid::new_v4(),"tool_name":"fixture"})).is_err());
        }
    }
}
