/**
* User identity management, channel mapping, and caller lookup.
*/
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ResourceOwner {
    pub user_context_id: UserContextId,
    pub user_id: UserId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChannelIdentity {
    pub channel: String,
    pub external_id: String,
}

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

impl ResolvedUserContext {
    pub fn owner(&self) -> ResourceOwner {
        ResourceOwner {
            user_context_id: self.id,
            user_id: self.user_id,
        }
    }

    /// Converts to the minimal caller-identity boundary type the
    /// vox-connections crate (connections/capability_grants/integration_registry/
    /// provider clients) takes, instead of this crate's full identity model.
    pub fn request_context(&self) -> vox_connections::identity::RequestContext {
        vox_connections::identity::RequestContext {
            id: vox_connections::identity::UserContextId(self.id.0),
            user_id: vox_connections::identity::UserId(self.user_id.0),
            subject: vox_connections::identity::RequestSubject {
                deployment_id: vox_connections::identity::DeploymentId(
                    self.subject.deployment_id.0,
                ),
            },
        }
    }
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

    pub async fn resolve_context(
        &self,
        subject: &UserContextSubject,
    ) -> Result<ResolvedUserContext, IdentityError> {
        let host_user_id = subject.normalized_host_user_id()?;

        let is_internal_scope = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(\
                SELECT 1 FROM host_apps h \
                JOIN platform_deployments d ON d.id = h.deployment_id \
                WHERE h.deployment_id = $1 AND h.id = $2 \
                  AND (d.external_key, h.external_key) IN (\
                    ('vox.legacy.deployment', 'vox.legacy.channel-host'), \
                    ('vox.standalone.deployment', 'vox.standalone.web')\
                  )\
            )",
        )
        .bind(subject.deployment_id.0)
        .bind(subject.host_app_id.0)
        .fetch_one(self.db.pool())
        .await?;
        if is_internal_scope {
            return Err(IdentityError::AccessDenied);
        }

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
    pub async fn resolve_legacy_owner(
        &self,
        identity: &ChannelIdentity,
    ) -> Result<ResourceOwner, IdentityError> {
        let user_id = self.resolve(identity).await?;
        self.owner_for_user(user_id).await
    }

    pub async fn owner_for_user(&self, user_id: UserId) -> Result<ResourceOwner, IdentityError> {
        let context_id =
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM user_contexts WHERE user_id = $1")
                .bind(user_id.0)
                .fetch_optional(self.db.pool())
                .await?
                .ok_or(IdentityError::AccessDenied)?;
        Ok(ResourceOwner {
            user_context_id: UserContextId(context_id),
            user_id,
        })
    }

    pub async fn resolve(&self, identity: &ChannelIdentity) -> Result<UserId, sqlx::Error> {
        let channel = identity.channel.trim();
        let external_id = identity.external_id.trim();
        // "phone" is stored digits-only (see services/api/routes/phone.rs link_phone)
        // so an inbound call's "+"-prefixed E.164 id must be normalized the same
        // way before it can ever match a number linked from the desktop/web app.
        let stored_id = if channel == "phone" {
            Self::normalize_phone(external_id)
        } else {
            external_id.to_string()
        };

        if let Some(id) = sqlx::query_scalar::<_, Uuid>(
            "SELECT user_id FROM channel_identities WHERE channel = $1 AND normalized_external_id = $2 AND revoked_at IS NULL",
        )
        .bind(channel)
        .bind(&stored_id)
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(UserId(id));
        }

        let mut tx = self.db.pool().begin().await?;

        if channel == "phone" || channel == "whatsapp" {
            let normalized = Self::normalize_phone(external_id);
            if !normalized.is_empty() {
                let other_channel = if channel == "whatsapp" {
                    "phone"
                } else {
                    "whatsapp"
                };
                let existing_user = sqlx::query_scalar::<_, Uuid>(
                    "SELECT user_id FROM channel_identities \
                     WHERE channel = $1 AND revoked_at IS NULL \
                       AND regexp_replace(normalized_external_id, '[^0-9]', '', 'g') = $2 \
                     LIMIT 1",
                )
                .bind(other_channel)
                .bind(&normalized)
                .fetch_optional(&mut *tx)
                .await?;

                if let Some(user_id) = existing_user {
                    sqlx::query(
                        "INSERT INTO channel_identities (user_id, channel, normalized_external_id) \
                         VALUES ($1, $2, $3) \
                         ON CONFLICT (channel, provider_scope, normalized_external_id) WHERE revoked_at IS NULL DO NOTHING",
                    )
                    .bind(user_id)
                    .bind(channel)
                    .bind(&stored_id)
                    .execute(&mut *tx)
                    .await?;

                    tx.commit().await?;
                    return Ok(UserId(user_id));
                }
            }
        }

        let new_user =
            sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *tx)
                .await?;

        sqlx::query(
            "INSERT INTO user_contexts (deployment_id, host_app_id, host_user_id, user_id) \
             SELECT d.id, h.id, $1::text, $2::uuid \
             FROM platform_deployments d \
             JOIN host_apps h ON h.deployment_id = d.id \
             WHERE d.external_key = 'vox.legacy.deployment' \
               AND h.external_key = 'vox.legacy.channel-host'",
        )
        .bind(new_user.to_string())
        .bind(new_user)
        .execute(&mut *tx)
        .await?;

        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO channel_identities (user_id, channel, normalized_external_id) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (channel, provider_scope, normalized_external_id) WHERE revoked_at IS NULL DO NOTHING \
             RETURNING user_id",
        )
        .bind(new_user)
        .bind(channel)
        .bind(&stored_id)
        .fetch_optional(&mut *tx)
        .await?;

        let id = if let Some(id) = inserted {
            id
        } else {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(new_user)
                .execute(&mut *tx)
                .await?;
            sqlx::query_scalar::<_, Uuid>(
                "SELECT user_id FROM channel_identities WHERE channel = $1 AND normalized_external_id = $2 AND revoked_at IS NULL",
            )
            .bind(channel)
            .bind(&stored_id)
            .fetch_one(&mut *tx)
            .await?
        };

        tx.commit().await?;
        Ok(UserId(id))
    }
}
