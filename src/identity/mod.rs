use crate::db::Db;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_HOST_USER_ID_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct UserId(pub Uuid);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct DeploymentId(pub Uuid);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct HostAppId(pub Uuid);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct HostOrganizationId(pub Uuid);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct UserContextId(pub Uuid);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChannelIdentity {
    pub channel: String,
    pub external_id: String,
}

/// The complete isolation key asserted by an authenticated host app.
///
/// Authentication of the host assertion is intentionally outside this type and
/// is added by the host-trust module. Matching `host_user_id` values never imply
/// that two subjects are the same user context.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserContextSubject {
    pub deployment_id: DeploymentId,
    pub host_app_id: HostAppId,
    pub organization_id: Option<HostOrganizationId>,
    pub host_user_id: String,
}

impl UserContextSubject {
    fn normalized_host_user_id(&self) -> Result<&str, IdentityError> {
        let host_user_id = self.host_user_id.trim();
        if host_user_id.is_empty() || host_user_id.len() > MAX_HOST_USER_ID_BYTES {
            return Err(IdentityError::InvalidContext);
        }
        Ok(host_user_id)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedUserContext {
    pub id: UserContextId,
    pub user_id: UserId,
    pub subject: UserContextSubject,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("invalid user context")]
    InvalidContext,
    #[error("user context scope is not registered")]
    ScopeNotFound,
    #[error("user context access denied")]
    AccessDenied,
    #[error("identity storage unavailable")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct IdentityService {
    db: Db,
}

impl IdentityService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub fn normalize_phone(raw: &str) -> String {
        raw.chars().filter(|c| c.is_ascii_digit()).collect()
    }

    /// Resolve a trusted host subject into exactly one isolated user context.
    ///
    /// The deployment, host app, and optional organization must already be
    /// registered. Callers must authenticate the host assertion before crossing
    /// this seam; arbitrary request payloads are not authority.
    pub async fn resolve_context(
        &self,
        subject: &UserContextSubject,
    ) -> Result<ResolvedUserContext, IdentityError> {
        let host_user_id = subject.normalized_host_user_id()?;

        let scope_exists = if let Some(organization_id) = subject.organization_id {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(\
                    SELECT 1 FROM host_organizations \
                    WHERE deployment_id = $1 AND host_app_id = $2 AND id = $3\
                )",
            )
            .bind(subject.deployment_id.0)
            .bind(subject.host_app_id.0)
            .bind(organization_id.0)
            .fetch_one(self.db.pool())
            .await?
        } else {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(\
                    SELECT 1 FROM host_apps \
                    WHERE deployment_id = $1 AND id = $2\
                )",
            )
            .bind(subject.deployment_id.0)
            .bind(subject.host_app_id.0)
            .fetch_one(self.db.pool())
            .await?
        };

        if !scope_exists {
            return Err(IdentityError::ScopeNotFound);
        }

        if let Some(context) = self.find_context(subject, host_user_id).await? {
            return Ok(context);
        }

        let mut tx = self.db.pool().begin().await?;
        let new_user =
            sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *tx)
                .await?;

        let inserted_context = sqlx::query_as::<_, (Uuid, Uuid)>(
            "INSERT INTO user_contexts (\
                deployment_id, host_app_id, organization_id, host_user_id, user_id\
             ) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT DO NOTHING \
             RETURNING id, user_id",
        )
        .bind(subject.deployment_id.0)
        .bind(subject.host_app_id.0)
        .bind(subject.organization_id.map(|id| id.0))
        .bind(host_user_id)
        .bind(new_user)
        .fetch_optional(&mut *tx)
        .await?;

        let (context_id, user_id) = if let Some(inserted) = inserted_context {
            sqlx::query(
                "INSERT INTO user_profiles (user_id, facts, version, updated_at) \
                 VALUES ($1, '{}'::jsonb, 1, now()) \
                 ON CONFLICT (user_id) DO NOTHING",
            )
            .bind(new_user)
            .execute(&mut *tx)
            .await?;
            inserted
        } else {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(new_user)
                .execute(&mut *tx)
                .await?;
            sqlx::query_as::<_, (Uuid, Uuid)>(
                "SELECT id, user_id FROM user_contexts \
                 WHERE deployment_id = $1 \
                   AND host_app_id = $2 \
                   AND organization_id IS NOT DISTINCT FROM $3 \
                   AND host_user_id = $4",
            )
            .bind(subject.deployment_id.0)
            .bind(subject.host_app_id.0)
            .bind(subject.organization_id.map(|id| id.0))
            .bind(host_user_id)
            .fetch_one(&mut *tx)
            .await?
        };

        tx.commit().await?;
        Ok(ResolvedUserContext {
            id: UserContextId(context_id),
            user_id: UserId(user_id),
            subject: UserContextSubject {
                host_user_id: host_user_id.to_owned(),
                ..subject.clone()
            },
        })
    }

    /// Fail closed unless `user_id` belongs to the exact canonical context.
    pub async fn authorize_context(
        &self,
        context_id: UserContextId,
        user_id: UserId,
    ) -> Result<(), IdentityError> {
        let authorized = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(\
                SELECT 1 FROM user_contexts WHERE id = $1 AND user_id = $2\
            )",
        )
        .bind(context_id.0)
        .bind(user_id.0)
        .fetch_one(self.db.pool())
        .await?;

        if authorized {
            Ok(())
        } else {
            Err(IdentityError::AccessDenied)
        }
    }

    async fn find_context(
        &self,
        subject: &UserContextSubject,
        host_user_id: &str,
    ) -> Result<Option<ResolvedUserContext>, IdentityError> {
        let context = sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT id, user_id FROM user_contexts \
             WHERE deployment_id = $1 \
               AND host_app_id = $2 \
               AND organization_id IS NOT DISTINCT FROM $3 \
               AND host_user_id = $4",
        )
        .bind(subject.deployment_id.0)
        .bind(subject.host_app_id.0)
        .bind(subject.organization_id.map(|id| id.0))
        .bind(host_user_id)
        .fetch_optional(self.db.pool())
        .await?;

        Ok(context.map(|(context_id, user_id)| ResolvedUserContext {
            id: UserContextId(context_id),
            user_id: UserId(user_id),
            subject: UserContextSubject {
                host_user_id: host_user_id.to_owned(),
                ..subject.clone()
            },
        }))
    }

    pub async fn resolve(&self, identity: &ChannelIdentity) -> Result<UserId, sqlx::Error> {
        let channel = identity.channel.trim();
        let external_id = identity.external_id.trim();

        // 1. Direct match on (channel, external_id)
        if let Some(id) = sqlx::query_scalar::<_, Uuid>(
            "SELECT user_id FROM user_identities WHERE channel = $1 AND external_id = $2",
        )
        .bind(channel)
        .bind(external_id)
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(UserId(id));
        }

        let mut tx = self.db.pool().begin().await?;

        // 2. Cross-channel match: Link Phone and WhatsApp if digits match
        if channel == "phone" || channel == "whatsapp" {
            let normalized = Self::normalize_phone(external_id);
            if !normalized.is_empty() {
                let other_channel = if channel == "whatsapp" {
                    "phone"
                } else {
                    "whatsapp"
                };
                let existing_user = sqlx::query_scalar::<_, Uuid>(
                    "SELECT user_id FROM user_identities \
                     WHERE channel = $1 AND regexp_replace(external_id, '[^0-9]', '', 'g') = $2 \
                     LIMIT 1",
                )
                .bind(other_channel)
                .bind(&normalized)
                .fetch_optional(&mut *tx)
                .await?;

                if let Some(user_id) = existing_user {
                    sqlx::query(
                        "INSERT INTO user_identities (user_id, channel, external_id) \
                         VALUES ($1, $2, $3) \
                         ON CONFLICT (channel, external_id) DO NOTHING",
                    )
                    .bind(user_id)
                    .bind(channel)
                    .bind(external_id)
                    .execute(&mut *tx)
                    .await?;

                    tx.commit().await?;
                    return Ok(UserId(user_id));
                }
            }
        }

        // 3. New user
        let new_user =
            sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *tx)
                .await?;

        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO user_identities (user_id, channel, external_id) VALUES ($1, $2, $3) \
             ON CONFLICT (channel, external_id) DO NOTHING RETURNING user_id",
        )
        .bind(new_user)
        .bind(channel)
        .bind(external_id)
        .fetch_optional(&mut *tx)
        .await?;

        let id = if let Some(id) = inserted {
            sqlx::query(
                "INSERT INTO user_profiles (user_id, facts, version, updated_at) \
                 VALUES ($1, '{}'::jsonb, 1, now()) \
                 ON CONFLICT (user_id) DO NOTHING",
            )
            .bind(new_user)
            .execute(&mut *tx)
            .await?;
            id
        } else {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(new_user)
                .execute(&mut *tx)
                .await?;
            sqlx::query_scalar::<_, Uuid>(
                "SELECT user_id FROM user_identities WHERE channel = $1 AND external_id = $2",
            )
            .bind(channel)
            .bind(external_id)
            .fetch_one(&mut *tx)
            .await?
        };

        tx.commit().await?;
        Ok(UserId(id))
    }
}
