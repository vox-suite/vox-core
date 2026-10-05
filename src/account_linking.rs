use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub async fn unify_by_verified_email(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    email: &str,
) -> Result<Uuid, sqlx::Error> {
    let owners: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE verified_email = $1 OR id = $2 ORDER BY created_at, id FOR UPDATE",
    )
    .bind(email.to_lowercase())
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await?;
    let Some(&primary) = owners.first() else {
        return Ok(user_id);
    };
    for other in owners.iter().skip(1) {
        sqlx::query("SELECT merge_user_accounts($1, $2)")
            .bind(other)
            .bind(primary)
            .execute(&mut **tx)
            .await?;
    }
    Ok(primary)
}
