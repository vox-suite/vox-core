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
            "SELECT c.id FROM conversations c \
             WHERE c.channel = 'whatsapp' AND c.status = 'active' \
             AND ( \
                 SELECT COALESCE(MAX(m.created_at), c.started_at) \
                 FROM messages m WHERE m.conversation_id = c.id \
             ) < now() - INTERVAL '30 minutes' \
             LIMIT 25",
        )
        .fetch_all(self.db.pool())
        .await?;

        let mut completed_count = 0;
        for row in rows {
            let conv_id: Uuid = row.get("id");
            let mut tx = self.db.pool().begin().await?;

            sqlx::query(
                "UPDATE conversations SET status = 'completed', completed_at = now() \
                 WHERE id = $1 AND status = 'active'",
            )
            .bind(conv_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "INSERT INTO jobs (kind, payload_reference_id) \
                 VALUES ('summarize_conversation', $1) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(conv_id)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            completed_count += 1;
        }

        Ok(completed_count)
    }
}
