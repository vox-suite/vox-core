use crate::db::Db;

pub struct SmsRetentionSweeper {
    db: Db,
}

impl SmsRetentionSweeper {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Purges `device_timeline_entries` past each user's own retention window
    /// for that entry's source — covers every data source sharing this table
    /// (sms, location, ...), not just sms, since they all key off the same
    /// per-user-per-source consent row.
    pub async fn purge_expired(&self) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "DELETE FROM device_timeline_entries dte \
             USING data_source_consents dsc \
             WHERE dte.source = dsc.data_source \
               AND dte.user_id = dsc.user_id \
               AND dte.created_at < now() - (dsc.retention_days || ' days')::interval",
        )
        .execute(self.db.pool())
        .await?;

        Ok(result.rows_affected())
    }
}
