/**
* Background handler processing finished conversations into summaries.
*/
use crate::{
    agents::{
        AgentError,
        conversation::PromptMessage,
        summarizer::{Summarizing, SummaryPrompt},
    },
    conversations::ConversationId,
    db::Db,
    identity::UserId,
    jev::JevClient,
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
    jev: Option<JevClient>,
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
            jev: None,
        }
    }

    pub fn with_jev(
        db: Db,
        summarizer: Arc<dyn Summarizing>,
        memory: MemoryService,
        jev: Option<JevClient>,
    ) -> Self {
        Self {
            db,
            summarizer,
            memory,
            jev,
        }
    }

    pub async fn handle(&self, conversation_id: ConversationId) -> Result<(), SummaryHandlerError> {
        let row = sqlx::query("SELECT user_id, summary_version FROM conversations WHERE id = $1")
            .bind(conversation_id.0)
            .fetch_optional(self.db.pool())
            .await?
            .ok_or(SummaryHandlerError::NotFound)?;

        let user_id: Uuid = row.get("user_id");
        let summary_version: i32 = row.get("summary_version");
        if summary_version > 0 {
            return Ok(());
        }

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

        if let Some(jev) = &self.jev {
            let mut transcript = String::new();
            for m in &messages {
                transcript.push_str(&format!("{}: {}\n", m.role, m.text));
            }

            if let Ok(prob) = jev.noul(
                serde_json::json!({ "transcript": transcript }),
                "Does this interaction contain new user biographical facts, commitments, tasks, or decisions worth persisting?",
            ).await
                && prob < 0.20 {
                    tracing::info!(
                        conversation_id = %conversation_id.0,
                        prob,
                        "Jev System 1: skipped trivial conversation summarization"
                    );
                    let stub = serde_json::json!({
                        "recap": "Brief interaction with no actionable updates.",
                        "profile_updates": {},
                        "commitments": [],
                        "decisions": [],
                    });
                    self.persist_summary(conversation_id.0, user_id, &stub, false)
                        .await?;
                    return Ok(());
                }
        }

        let summary = self
            .summarizer
            .summarize(SummaryPrompt { messages })
            .await?;

        let profile_updates_val = serde_json::to_value(&summary.profile_updates)
            .unwrap_or_else(|_| serde_json::json!({}));
        let commitments_val =
            serde_json::to_value(&summary.commitments).unwrap_or_else(|_| serde_json::json!([]));
        let decisions_val =
            serde_json::to_value(&summary.decisions).unwrap_or_else(|_| serde_json::json!([]));
        let latest_summary = serde_json::json!({
            "recap": summary.recap,
            "profile_updates": profile_updates_val,
            "commitments": commitments_val,
            "decisions": decisions_val,
        });

        self.persist_summary(
            conversation_id.0,
            user_id,
            &latest_summary,
            !summary.profile_updates.is_empty(),
        )
        .await?;
        self.memory.refresh(UserId(user_id)).await?;
        Ok(())
    }

    async fn persist_summary(
        &self,
        conversation_id: Uuid,
        user_id: Uuid,
        latest_summary: &serde_json::Value,
        apply_profile: bool,
    ) -> Result<(), SummaryHandlerError> {
        let mut tx = self.db.pool().begin().await?;
        let updated = sqlx::query(
            "UPDATE conversations SET \
                 latest_summary = $2, \
                 summary_version = summary_version + 1, \
                 summary_through_sequence = COALESCE((\
                     SELECT MAX(sequence_number) FROM messages WHERE conversation_id = $1\
                 ), 0), \
                 updated_at = now() \
             WHERE id = $1 AND summary_version = 0",
        )
        .bind(conversation_id)
        .bind(latest_summary)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if updated == 1
            && apply_profile
            && let Some(profile_updates) = latest_summary.get("profile_updates")
            && profile_updates.as_object().is_some_and(|o| !o.is_empty())
        {
            sqlx::query(
                "UPDATE users SET \
                 profile_facts = profile_facts || $2, \
                 profile_version = profile_version + 1, \
                 updated_at = now() \
                 WHERE id = $1",
            )
            .bind(user_id)
            .bind(profile_updates)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }
}
