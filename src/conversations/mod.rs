pub mod service;

use crate::identity::ChannelIdentity;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ConversationId(pub Uuid);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RespondRequest {
    pub identity: ChannelIdentity,
    pub external_conversation_id: String,
    pub text: String,
    pub initiation_context: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RespondResponse {
    pub conversation_id: ConversationId,
    pub text: String,
}
