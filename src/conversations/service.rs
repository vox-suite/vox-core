use super::{CompleteConversationRequest, ConversationId, RespondRequest, RespondResponse};
use crate::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder, PromptMessage},
    },
    db::Db,
    identity::{ChannelIdentity, UserId},
};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct ConversationService {
    db: Db,
    agent: Arc<dyn ConversationResponder>,
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
        Self { db, agent }
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
        let user_id = self.resolve_identity(&request.identity).await?;
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
                user_context: String::new(),
                recent_messages: prior_messages,
                user_text: request.text,
                initiation_context: request.initiation_context,
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
            None => return Ok(()), // Idempotent: nothing to complete
        };

        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "UPDATE conversations SET status = 'completed', completed_at = COALESCE(completed_at, now()) WHERE id = $1",
        )
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;

        // Enqueue post-conversation summarization job if not already enqueued
        let already_queued = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM jobs WHERE kind = 'summarize_conversation' AND payload_reference_id = $1",
        )
        .bind(conversation_id)
        .fetch_one(&mut *tx)
        .await?;

        if already_queued == 0 {
            sqlx::query(
                "INSERT INTO jobs (kind, payload_reference_id) VALUES ('summarize_conversation', $1)",
            )
            .bind(conversation_id)
            .execute(&mut *tx)
            .await?;
        }
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

    async fn resolve_identity(
        &self,
        identity: &ChannelIdentity,
    ) -> Result<UserId, ConversationError> {
        if let Some(id) = sqlx::query_scalar::<_, Uuid>(
            "SELECT user_id FROM user_identities WHERE channel = $1 AND external_id = $2",
        )
        .bind(identity.channel.trim())
        .bind(identity.external_id.trim())
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(UserId(id));
        }
        let mut tx = self.db.pool().begin().await?;
        let new_user =
            sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *tx)
                .await?;
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO user_identities (user_id, channel, external_id) VALUES ($1, $2, $3) \
             ON CONFLICT (channel, external_id) DO NOTHING RETURNING user_id",
        )
        .bind(new_user)
        .bind(identity.channel.trim())
        .bind(identity.external_id.trim())
        .fetch_optional(&mut *tx)
        .await?;
        let id = if let Some(id) = inserted {
            id
        } else {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(new_user)
                .execute(&mut *tx)
                .await?;
            sqlx::query_scalar::<_, Uuid>(
                "SELECT user_id FROM user_identities WHERE channel = $1 AND external_id = $2",
            )
            .bind(identity.channel.trim())
            .bind(identity.external_id.trim())
            .fetch_one(&mut *tx)
            .await?
        };
        tx.commit().await?;
        Ok(UserId(id))
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
