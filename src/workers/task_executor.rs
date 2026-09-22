/**
 * Worker executor consuming and running pending background tasks.
 */

use crate::{
    config::Config,
    db::Db,
    identity::{ResourceOwner, UserContextId, UserId},
    jev::JevClient,
    outbound::OutboundCallService,
};
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
use serde_json::json;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct TaskExecutorHandler {
    db: Db,
    api_key: String,
    model: String,
    jev: Option<JevClient>,
    outbound: Option<Arc<OutboundCallService>>,
}

#[derive(Debug, thiserror::Error)]
pub enum TaskExecutorError {
    #[error("task database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("task execution agent failure: {0}")]
    Agent(String),
    #[error("task not found")]
    NotFound,
}

impl TaskExecutorHandler {
    pub fn new(db: Db, config: &Config) -> Self {
        Self {
            db,
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
            jev: None,
            outbound: None,
        }
    }

    pub fn with_jev(db: Db, config: &Config, jev: Option<JevClient>) -> Self {
        Self {
            db,
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
            jev,
            outbound: None,
        }
    }

    pub fn with_outbound(mut self, outbound: Arc<OutboundCallService>) -> Self {
        self.outbound = Some(outbound);
        self
    }

    pub async fn handle(&self, task_id: Uuid) -> Result<(), TaskExecutorError> {
        let task_row = sqlx::query(
            "SELECT t.user_id, COALESCE(t.user_context_id, c.id) AS user_context_id, \
                    t.title, t.raw_instruction, t.execution_type, t.status \
             FROM tasks t \
             JOIN user_contexts c ON c.user_id = t.user_id \
             WHERE t.id = $1",
        )
        .bind(task_id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(TaskExecutorError::NotFound)?;

        let status: String = task_row.get("status");
        if status == "completed" || status == "cancelled" {
            return Ok(());
        }

        let owner = ResourceOwner {
            user_context_id: UserContextId(task_row.get("user_context_id")),
            user_id: UserId(task_row.get("user_id")),
        };
        let title: String = task_row.get("title");
        let instruction: String = task_row.get("raw_instruction");

        sqlx::query(
            "UPDATE tasks SET status = 'executing', \
                    user_context_id = COALESCE(user_context_id, $1), updated_at = now() \
             WHERE id = $2",
        )
        .bind(owner.user_context_id.0)
        .bind(task_id)
        .execute(self.db.pool())
        .await?;

        let client = gemini::Client::new(&self.api_key)
            .map_err(|e| TaskExecutorError::Agent(e.to_string()))?;

        let prompt = format!(
            "You are an autonomous AI worker executing a background task for the user.\n\
             Task title: {}\n\
             Instructions: {}\n\n\
             Synthesize the steps, evaluate feasibility, and provide a clear, concise outcome summary \
             (maximum 3-4 sentences) explaining what was done and the result.",
            title, instruction
        );

        let agent = client
            .agent(&self.model)
            .preamble("You are Vox's background task execution engine. You process tasks autonomously and summarize the final result clearly.")
            .build();

        let response = agent
            .prompt(prompt)
            .await
            .map_err(|e| TaskExecutorError::Agent(e.to_string()))?;

        let execution_result = json!({
            "outcome": "success",
            "summary": response.trim(),
            "completed_at": chrono::Utc::now().to_rfc3339(),
        });

        sqlx::query(
            "UPDATE tasks SET status = 'completed', \
             execution_result = $1, \
             completed_at = now(), \
             updated_at = now() \
             WHERE id = $2",
        )
        .bind(&execution_result)
        .bind(task_id)
        .execute(self.db.pool())
        .await?;

        self.notify_user_via_call(owner, task_id, &title, response.trim())
            .await?;

        Ok(())
    }

    async fn notify_user_via_call(
        &self,
        owner: ResourceOwner,
        task_id: Uuid,
        title: &str,
        summary: &str,
    ) -> Result<(), TaskExecutorError> {
        let has_phone: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM user_identities WHERE user_id = $1 AND channel = 'phone')",
        )
        .bind(owner.user_id.0)
        .fetch_one(self.db.pool())
        .await?;

        if !has_phone {
            return Ok(());
        }

        if let Some(jev) = &self.jev {
            let state = json!({
                "task_title": title,
                "task_summary": summary,
            });
            if let Ok(urgency) = jev.noul(
                state,
                "Does this task result represent an urgent emergency, critical real-world deadline, or explicit user demand to be phoned immediately?",
            ).await
                && urgency < 0.70 {
                    tracing::info!(
                        task_id = %task_id,
                        urgency,
                        "Jev System 1: task outcome is non-urgent, suppressing live outbound phone call"
                    );
                    return Ok(());
                }
        }

        if let Some(outbound) = &self.outbound {
            let reason = format!("Task notification: {}", title);
            let opening = format!(
                "Inform the user that their task '{}' has finished: {}",
                title, summary
            );
            let _ = outbound
                .initiate_call_for_user(owner, &reason, &opening, None, Some(task_id))
                .await;
        }

        Ok(())
    }
}
