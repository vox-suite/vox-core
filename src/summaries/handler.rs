use crate::{
    agents::{
        AgentError,
        conversation::PromptMessage,
        summarizer::{Summarizing, SummaryPrompt},
    },
    conversations::ConversationId,
    db::Db,
    identity::UserId,
    memory::MemoryService,
};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct SummaryHandler {
    db: Db,
    summarizer: Arc<dyn Summarizing>,
    memory: MemoryService,
}

#[derive(Debug, thiserror::Error)]
pub enum SummaryHandlerError {
    #[error("summary storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("summarizer agent unavailable")]
    Agent(#[from] AgentError),
    #[error("conversation not found")]
    NotFound,
}

impl SummaryHandler {
    pub fn new(db: Db, summarizer: Arc<dyn Summarizing>) -> Self {
        let memory = MemoryService::new(db.clone(), None);
        Self::with_memory(db, summarizer, memory)
    }

    pub fn with_memory(db: Db, summarizer: Arc<dyn Summarizing>, memory: MemoryService) -> Self {
        Self {
            db,
            summarizer,
            memory,
        }
    }

    pub async fn handle(&self, conversation_id: ConversationId) -> Result<(), SummaryHandlerError> {
        let conversation = sqlx::query("SELECT user_id FROM conversations WHERE id = $1")
            .bind(conversation_id.0)
            .fetch_optional(self.db.pool())
            .await?;

        let user_id: Uuid = match conversation {
            Some(row) => row.get("user_id"),
            None => return Err(SummaryHandlerError::NotFound),
        };

        let message_rows = sqlx::query(
            "SELECT role, text FROM messages WHERE conversation_id = $1 ORDER BY sequence_number ASC",
        )
        .bind(conversation_id.0)
        .fetch_all(self.db.pool())
        .await?;

        if message_rows.is_empty() {
            return Ok(());
        }

        let messages: Vec<PromptMessage> = message_rows
            .into_iter()
            .map(|row| PromptMessage {
                role: row.get("role"),
                text: row.get("text"),
            })
            .collect();

        let summary = self
            .summarizer
            .summarize(SummaryPrompt { messages })
            .await?;

        let mut tx = self.db.pool().begin().await?;

        let profile_updates_val = serde_json::to_value(&summary.profile_updates)
            .unwrap_or_else(|_| serde_json::json!({}));
        let commitments_val =
            serde_json::to_value(&summary.commitments).unwrap_or_else(|_| serde_json::json!([]));
        let decisions_val =
            serde_json::to_value(&summary.decisions).unwrap_or_else(|_| serde_json::json!([]));

        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO conversation_summaries (conversation_id, user_id, recap, profile_updates, commitments, decisions) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (conversation_id) DO NOTHING RETURNING id",
        )
        .bind(conversation_id.0)
        .bind(user_id)
        .bind(&summary.recap)
        .bind(&profile_updates_val)
        .bind(&commitments_val)
        .bind(&decisions_val)
        .fetch_optional(&mut *tx)
        .await?;

        if inserted.is_some() && !summary.profile_updates.is_empty() {
            sqlx::query(
                "INSERT INTO user_profiles (user_id, facts, version, updated_at) \
                 VALUES ($1, $2, 1, now()) \
                 ON CONFLICT (user_id) DO UPDATE SET \
                 facts = user_profiles.facts || EXCLUDED.facts, \
                 version = user_profiles.version + 1, \
                 updated_at = now()",
            )
            .bind(user_id)
            .bind(&profile_updates_val)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        self.memory.refresh(UserId(user_id)).await?;
        Ok(())
    }
}
