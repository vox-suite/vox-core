/**
* Periodic sweeper polling WhatsApp messages and incoming events.
*/
use crate::db::Db;
use sqlx::Row;
use uuid::Uuid;

pub struct WhatsAppSweeper {
    db: Db,
}

impl WhatsAppSweeper {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn sweep_inactive_conversations(&self) -> Result<usize, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT c.id, c.user_id, c.user_context_id FROM conversations c \
             WHERE c.channel = 'whatsapp' AND c.state = 'active' \
             AND ( \
                 SELECT COALESCE(MAX(m.created_at), c.created_at) \
                 FROM messages m WHERE m.conversation_id = c.id \
             ) < now() - INTERVAL '30 minutes' \
             LIMIT 25",
        )
        .fetch_all(self.db.pool())
        .await?;

        let mut completed_count = 0;
        for row in rows {
            let conv_id: Uuid = row.get("id");
            let user_id: Option<Uuid> = row.get("user_id");
            let user_context_id: Option<Uuid> = row.get("user_context_id");
            let mut tx = self.db.pool().begin().await?;

            sqlx::query(
                "UPDATE conversations SET state = 'completed', completed_at = now(), \
                 updated_at = now() \
                 WHERE id = $1 AND state = 'active'",
            )
            .bind(conv_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "INSERT INTO jobs (kind, payload_reference_id, user_id, user_context_id) \
                 SELECT 'summarize_conversation', $1, $2, $3 \
                 WHERE NOT EXISTS ( \
                     SELECT 1 FROM jobs WHERE kind = 'summarize_conversation' AND payload_reference_id = $1 \
                 )",
            )
            .bind(conv_id)
            .bind(user_id)
            .bind(user_context_id)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            completed_count += 1;
        }

        Ok(completed_count)
    }
}
