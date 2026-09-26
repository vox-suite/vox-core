use crate::db::Db;

pub struct SmsRetentionSweeper {
    db: Db,
}

impl SmsRetentionSweeper {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Purges ingested spans past each user's own retention window for that
    /// span's source (sms, location, ...), keyed off the per-user-per-source
    /// consent row.
    pub async fn purge_expired(&self) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "DELETE FROM spans s \
             USING data_source_consents dsc \
             WHERE s.source = dsc.data_source \
               AND s.user_id = dsc.user_id \
               AND s.created_at < now() - (dsc.retention_days || ' days')::interval",
        )
        .execute(self.db.pool())
        .await?;

        Ok(result.rows_affected())
    }
}
