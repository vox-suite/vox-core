/**
* User identity management, channel mapping, and caller lookup.
*/
use crate::db::Db;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_HOST_USER_ID_BYTES: usize = 512;
const TRUSTED_CHANNEL_HOST: (&str, &str) = ("vox.standalone.deployment", "vox.standalone.bridge");

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

impl vox_connections::identity::RequestScope for ResolvedUserContext {
    fn request_context(&self) -> vox_connections::identity::RequestContext {
        ResolvedUserContext::request_context(self)
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

        if let Some(context) = self.find_linked_channel_context(subject, host_user_id).await? {
            return Ok(context);
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
    /// The first-party bridge host asserts a phone caller (`+E.164`) or a verified
    /// desktop account (`vox-account:<user id>`). When that identity is already
    /// linked to a signed-in user, resolve to that user's context instead of
    /// creating a separate anonymous user for the channel.
    async fn find_linked_channel_context(
        &self,
        subject: &UserContextSubject,
        host_user_id: &str,
    ) -> Result<Option<ResolvedUserContext>, IdentityError> {
        if subject.organization_id.is_some() {
            return Ok(None);
        }
        let trusted = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(\
                SELECT 1 FROM host_apps h \
                JOIN platform_deployments d ON d.id = h.deployment_id \
                WHERE h.deployment_id = $1 AND h.id = $2 \
                  AND d.external_key = $3 AND h.external_key = $4\
            )",
        )
        .bind(subject.deployment_id.0)
        .bind(subject.host_app_id.0)
        .bind(TRUSTED_CHANNEL_HOST.0)
        .bind(TRUSTED_CHANNEL_HOST.1)
        .fetch_one(self.db.pool())
        .await?;
        if !trusted {
            return Ok(None);
        }

        let linked = if let Some(account) = host_user_id.strip_prefix("vox-account:") {
            let Ok(user_id) = Uuid::parse_str(account) else {
                return Ok(None);
            };
            sqlx::query_as::<_, (Uuid, Uuid)>(
                "SELECT id, user_id FROM user_contexts WHERE user_id = $1",
            )
            .bind(user_id)
            .fetch_optional(self.db.pool())
            .await?
        } else {
            let digits: String = host_user_id.chars().filter(char::is_ascii_digit).collect();
            if !(7..=15).contains(&digits.len()) || host_user_id.trim_start_matches('+') != digits {
                return Ok(None);
            }
            sqlx::query_as::<_, (Uuid, Uuid)>(
                "SELECT c.id, c.user_id FROM channel_identities i \
                 JOIN user_contexts c ON c.user_id = i.user_id \
                 WHERE i.channel = 'phone' AND i.normalized_external_id = $1 \
                   AND i.revoked_at IS NULL LIMIT 1",
            )
            .bind(&digits)
            .fetch_optional(self.db.pool())
            .await?
        };

        Ok(linked.map(|(context_id, user_id)| ResolvedUserContext {
            id: UserContextId(context_id),
            user_id: UserId(user_id),
            subject: UserContextSubject {
                host_user_id: host_user_id.to_owned(),
                ..subject.clone()
            },
        }))
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
}
