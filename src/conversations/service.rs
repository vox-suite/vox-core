use super::{CompleteConversationRequest, ConversationId, RespondRequest, RespondResponse};
use crate::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder, PromptMessage},
    },
    db::Db,
    identity::{IdentityService, UserId},
    memory::MemoryService,
};
use futures_util::{Stream, StreamExt, stream};
use sqlx::Row;
use std::{pin::Pin, sync::Arc};
use uuid::Uuid;

pub type ConversationTextStream =
    Pin<Box<dyn Stream<Item = Result<String, ConversationError>> + Send>>;

#[derive(Clone)]
pub struct ConversationService {
    db: Db,
    agent: Arc<dyn ConversationResponder>,
    identities: IdentityService,
    memory: MemoryService,
}

#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    #[error("invalid conversation request")]
    Invalid,
    #[error("conversation not found")]
    NotFound,
    #[error("conversation storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("conversation agent unavailable")]
    Agent(#[from] AgentError),
    #[error("conversation identity conflict")]
    IdentityConflict,
}

impl ConversationService {
    pub fn new(db: Db, agent: Arc<dyn ConversationResponder>) -> Self {
        let memory = MemoryService::new(db.clone(), None);
        Self::with_memory(db, agent, memory)
    }

    pub fn with_memory(
        db: Db,
        agent: Arc<dyn ConversationResponder>,
        memory: MemoryService,
    ) -> Self {
        Self {
            identities: IdentityService::new(db.clone()),
            db,
            agent,
            memory,
        }
    }

    pub async fn respond(
        &self,
        request: RespondRequest,
    ) -> Result<RespondResponse, ConversationError> {
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.external_conversation_id.trim().is_empty()
            || request.text.trim().is_empty()
        {
            return Err(ConversationError::Invalid);
        }
        let user_id = self.identities.resolve(&request.identity).await?;

        // Seed name if provided via initiation_context (e.g. from WhatsApp profile)
        if let Some(init_ctx) = &request.initiation_context {
            if let Some(wa_name) = init_ctx.strip_prefix("whatsapp_name:") {
                let wa_name_clean = wa_name.trim();
                if !wa_name_clean.is_empty() {
                    let _ = self.memory.set_user_name(user_id, wa_name_clean).await;
                }
            }
        }

        // Fast lookup of user's name via Redis cache (falls back to PostgreSQL)
        let mut known_name = self.memory.get_user_name(user_id).await?;
        let mut has_name = known_name.as_deref().map(str::trim).filter(|s| !s.is_empty()).is_some();

        // Fast-path name extraction: If user states their name during onboarding, save to Redis + DB
        if !has_name {
            if let Some(extracted_name) = extract_name_from_text(&request.text) {
                let _ = self.memory.set_user_name(user_id, &extracted_name).await;
                has_name = true;
                known_name = Some(extracted_name);
            }
        }

        let needs_onboarding = !has_name;

        let conversation_id = self
            .resolve_conversation(
                user_id,
                &request.identity.channel,
                &request.external_conversation_id,
            )
            .await?;

        // Load prior messages in this call to maintain a running session
        let prior_messages = self.load_recent_messages(conversation_id).await?;

        self.append_message(conversation_id, "user", request.text.trim())
            .await?;

        // Inbound call opening fast-path: Greet immediately (<1ms) using cached name from Redis
        let is_voice = crate::agents::conversation::is_voice_channel(&request.identity.channel);
        let is_inbound_connect = is_voice
            && prior_messages.is_empty()
            && (request.text.trim() == "The call just connected. Greet the user."
                || request.initiation_context.as_deref()
                    == Some("The call just connected. Greet the user."));

        if is_inbound_connect {
            let greeting = if let Some(ref name) = known_name {
                format!("Hello {}! How can I help you today?", name.trim())
            } else {
                "Hello! I'm Vox, your personal AI assistant. What should I call you?".to_string()
            };

            self.append_message(conversation_id, "assistant", &greeting).await?;
            let _ = self.memory.refresh(user_id).await;
            return Ok(RespondResponse {
                conversation_id,
                text: greeting,
            });
        }

        let text = self
            .agent
            .respond(ConversationPrompt {
                user_id,
                channel: request.identity.channel.clone(),
                user_context: self.memory.load(user_id).await?,
                recent_messages: prior_messages,
                user_text: request.text,
                initiation_context: request.initiation_context,
                needs_onboarding,
            })
            .await?;
        self.append_message(conversation_id, "assistant", text.trim())
            .await?;
        let _ = self.memory.refresh(user_id).await;
        Ok(RespondResponse {
            conversation_id,
            text,
        })
    }

    pub async fn respond_stream(
        &self,
        request: RespondRequest,
    ) -> Result<ConversationTextStream, ConversationError> {
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.external_conversation_id.trim().is_empty()
            || request.text.trim().is_empty()
        {
            return Err(ConversationError::Invalid);
        }
        let user_id = self.identities.resolve(&request.identity).await?;

        if let Some(init_ctx) = &request.initiation_context {
            if let Some(wa_name) = init_ctx.strip_prefix("whatsapp_name:") {
                let wa_name_clean = wa_name.trim();
                if !wa_name_clean.is_empty() {
                    let _ = self.memory.set_user_name(user_id, wa_name_clean).await;
                }
            }
        }

        // Fast lookup of user's name via Redis cache (falls back to PostgreSQL)
        let mut known_name = self.memory.get_user_name(user_id).await?;
        let mut has_name = known_name.as_deref().map(str::trim).filter(|s| !s.is_empty()).is_some();

        // Fast-path name extraction: If user states their name during onboarding, save to Redis + DB
        if !has_name {
            if let Some(extracted_name) = extract_name_from_text(&request.text) {
                let _ = self.memory.set_user_name(user_id, &extracted_name).await;
                has_name = true;
                known_name = Some(extracted_name);
            }
        }

        let needs_onboarding = !has_name;

        let conversation_id = self
            .resolve_conversation(
                user_id,
                &request.identity.channel,
                &request.external_conversation_id,
            )
            .await?;

        let prior_messages = self.load_recent_messages(conversation_id).await?;

        self.append_message(conversation_id, "user", request.text.trim())
            .await?;

        // Inbound call opening fast-path: Stream greeting immediately (<1ms) using cached name from Redis
        let is_voice = crate::agents::conversation::is_voice_channel(&request.identity.channel);
        let is_inbound_connect = is_voice
            && prior_messages.is_empty()
            && (request.text.trim() == "The call just connected. Greet the user."
                || request.initiation_context.as_deref()
                    == Some("The call just connected. Greet the user."));

        if is_inbound_connect {
            let greeting = if let Some(ref name) = known_name {
                format!("Hello {}! How can I help you today?", name.trim())
            } else {
                "Hello! I'm Vox, your personal AI assistant. What should I call you?".to_string()
            };

            self.append_message(conversation_id, "assistant", &greeting).await?;
            let _ = self.memory.refresh(user_id).await;

            let chunks = if let Some(ref name) = known_name {
                vec![
                    Ok(format!("Hello {}! ", name.trim())),
                    Ok("How can I help you today?".to_string()),
                ]
            } else {
                vec![
                    Ok("Hello! I'm Vox, your personal AI assistant. ".to_string()),
                    Ok("What should I call you?".to_string()),
                ]
            };
            return Ok(Box::pin(futures_util::stream::iter(chunks)));
        }

        let user_context = self.memory.load(user_id).await?;

        let stream = self
            .agent
            .respond_stream(ConversationPrompt {
                user_id,
                channel: request.identity.channel.clone(),
                user_context,
                recent_messages: prior_messages,
                user_text: request.text,
                initiation_context: request.initiation_context,
                needs_onboarding,
            })
            .await?;

        let service = self.clone();
        let out_stream = stream::unfold(
            (stream, String::new(), false, service, conversation_id, user_id),
            |(mut stream, mut full_text, mut finished, service, conv_id, uid)| async move {
                if finished {
                    return None;
                }
                match stream.next().await {
                    Some(Ok(chunk)) => {
                        full_text.push_str(&chunk);
                        Some((Ok(chunk), (stream, full_text, false, service, conv_id, uid)))
                    }
                    Some(Err(err)) => {
                        Some((
                            Err(ConversationError::Agent(err)),
                            (stream, full_text, true, service, conv_id, uid),
                        ))
                    }
                    None => {
                        let text_to_save = full_text.trim().to_string();
                        if !text_to_save.is_empty() {
                            let _ = service.append_message(conv_id, "assistant", &text_to_save).await;
                            let _ = service.memory.refresh(uid).await;
                        }
                        finished = true;
                        None
                    }
                }
            },
        );

        Ok(Box::pin(out_stream))
    }

    pub async fn complete(
        &self,
        request: CompleteConversationRequest,
    ) -> Result<(), ConversationError> {
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.external_conversation_id.trim().is_empty()
        {
            return Err(ConversationError::Invalid);
        }
        let conversation = sqlx::query(
            "SELECT c.id FROM conversations c \
             JOIN user_identities ui ON ui.user_id = c.user_id \
             WHERE c.channel = $1 AND c.external_id = $2 AND ui.channel = $1 AND ui.external_id = $3",
        )
        .bind(request.identity.channel.trim())
        .bind(request.external_conversation_id.trim())
        .bind(request.identity.external_id.trim())
        .fetch_optional(self.db.pool())
        .await?;

        let conversation_id = match conversation {
            Some(row) => row.get::<Uuid, _>("id"),
            None => return Ok(()),
        };

        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "UPDATE conversations SET status = 'completed', completed_at = COALESCE(completed_at, now()) WHERE id = $1",
        )
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;

        // Enqueue post-conversation summarization job if not already enqueued
        sqlx::query(
            "INSERT INTO jobs (kind, payload_reference_id) VALUES ('summarize_conversation', $1) \
             ON CONFLICT DO NOTHING",
        )
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn load_recent_messages(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<PromptMessage>, ConversationError> {
        let rows = sqlx::query(
            "SELECT role, text FROM messages WHERE conversation_id = $1 ORDER BY sequence_number ASC",
        )
        .bind(conversation_id.0)
        .fetch_all(self.db.pool())
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| PromptMessage {
                role: row.get("role"),
                text: row.get("text"),
            })
            .collect())
    }

    async fn resolve_conversation(
        &self,
        user_id: UserId,
        channel: &str,
        external_id: &str,
    ) -> Result<ConversationId, ConversationError> {
        let row = sqlx::query(
            "INSERT INTO conversations (user_id, channel, external_id) VALUES ($1, $2, $3) \
             ON CONFLICT (channel, external_id) DO UPDATE SET external_id = EXCLUDED.external_id RETURNING id, user_id",
        )
        .bind(user_id.0)
        .bind(channel.trim())
        .bind(external_id.trim())
        .fetch_one(self.db.pool())
        .await?;
        let stored_user: Uuid = row.get("user_id");
        if stored_user != user_id.0 {
            return Err(ConversationError::IdentityConflict);
        }
        Ok(ConversationId(row.get("id")))
    }

    async fn append_message(
        &self,
        conversation_id: ConversationId,
        role: &str,
        text: &str,
    ) -> Result<(), ConversationError> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(conversation_id.0.to_string())
            .execute(&mut *tx)
            .await?;
        let sequence = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(sequence_number), 0) + 1 FROM messages WHERE conversation_id = $1",
        )
        .bind(conversation_id.0)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO messages (conversation_id, sequence_number, role, text) VALUES ($1, $2, $3, $4)",
        )
        .bind(conversation_id.0)
        .bind(sequence)
        .bind(role)
        .bind(text)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Extracts a user's stated name from an introductory or onboarding message
/// (e.g. "My name is Rahul", "I'm Rahul", "Call me Rahul") to bypass synchronous tool calls.
pub fn extract_name_from_text(text: &str) -> Option<String> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();

    let prefixes = [
        "my name is ",
        "i am ",
        "i'm ",
        "call me ",
        "this is ",
        "it's ",
        "it is ",
    ];

    for prefix in prefixes {
        if lower.starts_with(prefix) {
            let candidate = t[prefix.len()..].trim().trim_end_matches(['.', '!', '?']);
            let word_count = candidate.split_whitespace().count();
            if !candidate.is_empty()
                && word_count >= 1
                && word_count <= 3
                && !candidate.contains(['\n', '\r', '\t', '{', '}', '[', ']'])
            {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_name_from_text() {
        assert_eq!(extract_name_from_text("My name is Rahul."), Some("Rahul".into()));
        assert_eq!(extract_name_from_text("my name is rahul"), Some("rahul".into()));
        assert_eq!(extract_name_from_text("I'm Rahul Sharma"), Some("Rahul Sharma".into()));
        assert_eq!(extract_name_from_text("Call me John Doe"), Some("John Doe".into()));
        assert_eq!(extract_name_from_text("Nope."), None);
        assert_eq!(extract_name_from_text("Hello there"), None);
    }
}
