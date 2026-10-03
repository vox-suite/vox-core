/**
* Conversational state, turn handling, and dialogue domain types.
*/
pub mod service;
pub(crate) mod speculation;

use crate::identity::ChannelIdentity;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ConversationId(pub Uuid);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RespondRequest {
    pub agent_external_key: String,
    pub identity: ChannelIdentity,
    pub external_conversation_id: String,
    pub text: String,
    pub initiation_context: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub revision: Option<u64>,
    #[serde(default)]
    pub tts_provider: Option<String>,
    #[serde(default)]
    pub filler: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RespondResponse {
    pub conversation_id: ConversationId,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub task: Option<crate::durable_tasks::DurableTask>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompleteConversationRequest {
    pub agent_external_key: String,
    pub identity: ChannelIdentity,
    pub external_conversation_id: String,
}
