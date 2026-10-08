use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Compatibility entry point: matching verified email never proves control of
/// two existing identities. Explicit authenticated linking remains separate.
pub async fn unify_by_verified_email(
    _tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    _email: &str,
) -> Result<Uuid, sqlx::Error> {
    Ok(user_id)
}
