use crate::{
    config::Config,
    db::Db,
};
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone)]
pub struct TaskExecutorHandler {
    db: Db,
    api_key: String,
    model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum TaskExecutorError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("task not found")]
    NotFound,
    #[error("agent provider error: {0}")]
    Agent(String),
}

impl TaskExecutorHandler {
    pub fn new(db: Db, config: &Config) -> Self {
        Self {
            db,
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
        }
    }

    pub async fn handle(&self, task_id: Uuid) -> Result<(), TaskExecutorError> {
        let task_row = sqlx::query(
            "SELECT user_id, title, raw_instruction, execution_type, status \
             FROM tasks WHERE id = $1",
        )
        .bind(task_id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(TaskExecutorError::NotFound)?;

        let status: String = task_row.get("status");
        if status == "completed" || status == "cancelled" {
            return Ok(());
        }

        let user_id: Uuid = task_row.get("user_id");
        let title: String = task_row.get("title");
        let instruction: String = task_row.get("raw_instruction");

        // Mark task as executing
        sqlx::query("UPDATE tasks SET status = 'executing', updated_at = now() WHERE id = $1")
            .bind(task_id)
            .execute(self.db.pool())
            .await?;

        // Run autonomous execution using Gemini
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
            .preamble("You are a proactive autonomous worker. Fulfill the user's instructions directly and objectively.")
            .build();

        let response = agent
            .prompt(prompt)
            .await
            .map_err(|e| TaskExecutorError::Agent(e.to_string()))?;

        let execution_result = json!({
            "summary": response.trim(),
            "completed_at": chrono::Utc::now().to_rfc3339()
        });

        // Mark task completed
        sqlx::query(
            "UPDATE tasks SET \
             status = 'completed', \
             execution_result = $1, \
             completed_at = now(), \
             updated_at = now() \
             WHERE id = $2",
        )
        .bind(&execution_result)
        .bind(task_id)
        .execute(self.db.pool())
        .await?;

        // Enqueue outbound call to inform the user if they have a phone identity
        self.notify_user_via_call(user_id, task_id, &title, response.trim()).await?;

        Ok(())
    }

    async fn notify_user_via_call(
        &self,
        user_id: Uuid,
        task_id: Uuid,
        title: &str,
        summary: &str,
    ) -> Result<(), TaskExecutorError> {
        let has_phone: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM user_identities WHERE user_id = $1 AND channel = 'phone')",
        )
        .bind(user_id)
        .fetch_one(self.db.pool())
        .await?;

        if !has_phone {
            return Ok(());
        }

        let idempotency_key = format!("task_complete_call:{}:{}", task_id, Uuid::new_v4());
        let payload = json!({
            "reason": format!("Autonomous task completed: {}", title),
            "opening_instruction": format!("Inform the user that their task '{}' has finished: {}", title, summary)
        });

        let mut tx = self.db.pool().begin().await?;

        let action_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO actions (user_id, task_id, kind, payload, state, idempotency_key) \
             VALUES ($1, $2, 'outbound_call', $3, 'pending', $4) \
             RETURNING id",
        )
        .bind(user_id)
        .bind(task_id)
        .bind(payload)
        .bind(idempotency_key)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO jobs (kind, payload_reference_id) \
             VALUES ('dispatch_action', $1)",
        )
        .bind(action_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }
}
