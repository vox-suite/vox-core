use sqlx::{PgPool, Row};

#[derive(Clone)]
pub struct AttachmentRetentionSweeper { pool: PgPool }
impl AttachmentRetentionSweeper {
    pub fn new(pool: PgPool) -> Self { Self { pool } }
    pub async fn sweep_expired(&self) -> Result<usize, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query("SELECT id,user_id,storage_owner_id,source_record_id,object_ref FROM source_attachments WHERE raw_deleted_at IS NULL AND expires_at < now() ORDER BY expires_at LIMIT 100 FOR UPDATE SKIP LOCKED")
            .fetch_all(&mut *tx).await?;
        let mut swept = 0;
        for row in rows {
            let id: uuid::Uuid = row.get("id");
            let user: uuid::Uuid = row.get("user_id");
            let storage_owner: uuid::Uuid = row.get("storage_owner_id");
            let record: uuid::Uuid = row.get("source_record_id");
            let object: String = row.get("object_ref");
            if !object.starts_with(&format!("vox-obj://{storage_owner}/")) {
                tracing::error!(%id, "attachment retention rejected noncanonical owned reference"); continue;
            }
            if let Err(error) = crate::storage::object_storage::delete_object(&object).await {
                tracing::warn!(%id,%error,"attachment deletion failed; retention will retry"); continue;
            }
            sqlx::query("UPDATE source_attachments SET raw_deleted_at=now(),encryption_metadata=encryption_metadata-'encrypted_secret'-'nonce'-'secret_expires_at',updated_at=now() WHERE id=$1 AND user_id=$2")
                .bind(id).bind(user).execute(&mut *tx).await?;
            sqlx::query("UPDATE source_records SET disposition='purged',temporary_content_ref=NULL,raw_deleted_at=COALESCE(raw_deleted_at,now()) WHERE id=$1 AND user_id=$2")
                .bind(record).bind(user).execute(&mut *tx).await?;
            swept += 1;
        }
        sqlx::query("UPDATE source_attachments SET encryption_metadata=encryption_metadata-'encrypted_secret'-'nonce'-'secret_expires_at' WHERE encryption_metadata ? 'secret_expires_at' AND (encryption_metadata->>'secret_expires_at')::timestamptz<=now()")
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(swept)
    }
}
