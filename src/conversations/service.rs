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
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

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
                    let _ = sqlx::query(
                        "UPDATE user_profiles \
                         SET facts = jsonb_set(facts, '{name}', to_jsonb($1::text), true), \
                             updated_at = now() \
                         WHERE user_id = $2 AND (facts->>'name' IS NULL OR facts->>'name' = '')",
                    )
                    .bind(wa_name_clean)
                    .bind(user_id.0)
                    .execute(self.db.pool())
                    .await;
                }
            }
        }

        // Check if user's name is known in facts
        let known_name: Option<String> = sqlx::query_scalar(
            "SELECT facts->>'name' FROM user_profiles WHERE user_id = $1",
        )
        .bind(user_id.0)
        .fetch_optional(self.db.pool())
        .await?
        .flatten();

        let has_name = known_name.as_deref().map(str::trim).filter(|s| !s.is_empty()).is_some();
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
        Ok(RespondResponse {
            conversation_id,
            text,
        })
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
        Ok(ConversationId(row.get("id")))\n    }

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
