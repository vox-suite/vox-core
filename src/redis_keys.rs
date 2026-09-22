/**
 * Canonical Redis key helpers for Vox Core.
 *
 * Redis holds only rebuildable, non-secret projections. PostgreSQL remains
 * authoritative. Host-app assertion nonces stay in-process memory (not Redis).
 */

use crate::identity::UserId;
use uuid::Uuid;

/// Prefix shared by every Core Redis key. Admin SCAN defaults to `{PREFIX}*`.
pub const PREFIX: &str = "vox:";

/// Minimal per-user cache: name + channel identities as JSON.
///
/// Where: `MemoryService` upserts on name/identity sync; voice opening and
/// name helpers read it.
/// Why: one small record per user so greetings and lookups avoid Postgres
/// without storing the full LLM context projection.
///
/// Value shape:
/// `{"name":"Rahul","channels":[{"channel":"phone","external_id":"+9198..."}]}`
pub fn user(user_id: UserId) -> String {
    format!("{PREFIX}user:{}", user_id.0)
}

pub fn user_uuid(user_id: Uuid) -> String {
    format!("{PREFIX}user:{user_id}")
}

/// Channel identity → user id index into [`user`].
///
/// Where: written alongside [`user`] when syncing identities; read by
/// `cached_opening` / `get_user_by_channel` for ≤100 ms voice greetings.
/// Why: opening must resolve a caller by phone/channel without scanning every
/// `vox:user:*` key or hitting Postgres.
///
/// Value: user UUID string.
pub fn user_by_channel(channel: &str, external_id: &str) -> String {
    format!(
        "{PREFIX}channel:{}:{}",
        channel.trim().to_ascii_lowercase(),
        external_id.trim()
    )
}

/// SCAN pattern for all minimal user records (full replace / admin).
pub const USER_SCAN: &str = "vox:user:*";

/// SCAN pattern for all channel→user indexes (full replace / admin).
pub const CHANNEL_SCAN: &str = "vox:channel:*";
