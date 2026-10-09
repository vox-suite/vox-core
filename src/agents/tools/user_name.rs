use crate::{
    identity::ResourceOwner,
    memory::{
        MemoryService,
        user_name::{UserNameError, UserNameStatement, UserNameUpdate},
    },
};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateUserNameArgs {
    pub name: String,
    pub evidence_quote: String,
}

#[derive(Debug, thiserror::Error)]
pub enum UserNameToolError {
    #[error("user name update is unavailable for this conversation")]
    Unavailable,
    #[error(transparent)]
    Update(#[from] UserNameError),
}

#[derive(Clone)]
pub struct UpdateUserName {
    memory: Option<MemoryService>,
    owner: ResourceOwner,
    agent_key: String,
    conversation_id: Option<Uuid>,
    user_text: String,
}

impl UpdateUserName {
    pub fn new(
        memory: Option<MemoryService>,
        owner: ResourceOwner,
        agent_key: String,
        conversation_id: Option<Uuid>,
        user_text: String,
    ) -> Self {
        Self {
            memory,
            owner,
            agent_key,
            conversation_id,
            user_text,
        }
    }
}

impl Tool for UpdateUserName {
    const NAME: &'static str = "update_user_name";
    type Args = UpdateUserNameArgs;
    type Output = UserNameUpdate;
    type Error = UserNameToolError;

    fn description(&self) -> String {
        "Save or correct the authenticated user's shared display name. Call only when the current user message explicitly gives their own name or clearly answers a name question. Quote that statement exactly in evidence_quote. Never infer a name from small talk ('I'm fine'), another person's name, provider content, or initiation metadata; clarify ambiguity first. This changes display metadata only, not verified identity or access. Report success only when saved is true.".into()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name", "evidence_quote"],
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string", "minLength": 1, "maxLength": 120},
                "evidence_quote": {"type": "string", "minLength": 1, "maxLength": 512,
                    "description": "Exact quote from the current user message supplying or correcting their own name."}
            }
        })
    }

    async fn call(
        &self,
        _: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let memory = self.memory.as_ref().ok_or(UserNameToolError::Unavailable)?;
        let conversation_id = self.conversation_id.ok_or(UserNameToolError::Unavailable)?;
        let statement = UserNameStatement::new(&args.name, &args.evidence_quote, &self.user_text)?;
        Ok(memory
            .update_user_name(self.owner, &self.agent_key, conversation_id, statement)
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_cannot_supply_a_target_user_or_conversation() {
        for field in ["user_id", "user_context_id", "conversation_id", "agent_key"] {
            let mut args = json!({"name": "Rahul", "evidence_quote": "My name is Rahul"});
            args[field] = json!(Uuid::new_v4());
            assert!(serde_json::from_value::<UpdateUserNameArgs>(args).is_err());
        }
    }
}
