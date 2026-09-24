/**
* Domain models for users, channels, and resource ownership.
*/
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum UserStatus {
    Provisional,
    #[default]
    Active,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub status: UserStatus,
    pub display_name: Option<String>,
    pub preferences: serde_json::Value,
    pub profile_facts: serde_json::Value,
    pub persona: serde_json::Value,
    pub profile_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthIdentity {
    pub id: Uuid,
    pub user_id: Uuid,
    pub issuer: String,
    pub subject: String,
    pub profile_metadata: serde_json::Value,
    pub verified_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelIdentity {
    pub id: Uuid,
    pub user_id: Uuid,
    pub channel: String,
    pub provider_scope: String,
    pub normalized_external_id: String,
    pub verified_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub auth_identity_id: Option<Uuid>,
    pub device_id: Option<Uuid>,
    pub token_hash: String,
    pub family_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PrincipalKind {
    User,
    Device,
    Service,
    Admin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Actor {
    pub user_id: Uuid,
    pub principal_id: Uuid,
    pub principal_kind: PrincipalKind,
    pub grants: Vec<String>,
}

impl Actor {
    pub fn device_id(&self) -> Option<Uuid> {
        if self.principal_kind == PrincipalKind::Device {
            Some(self.principal_id)
        } else {
            None
        }
    }

    pub fn user(user_id: Uuid) -> Self {
        Self {
            user_id,
            principal_id: user_id,
            principal_kind: PrincipalKind::User,
            grants: vec!["consumer".to_string()],
        }
    }

    pub fn device(user_id: Uuid, device_id: Uuid) -> Self {
        Self {
            user_id,
            principal_id: device_id,
            principal_kind: PrincipalKind::Device,
            grants: vec!["device".to_string()],
        }
    }

    pub fn service(user_id: Uuid) -> Self {
        Self {
            user_id,
            principal_id: user_id,
            principal_kind: PrincipalKind::Service,
            grants: vec!["service".to_string()],
        }
    }

    pub fn admin(user_id: Uuid) -> Self {
        Self {
            user_id,
            principal_id: user_id,
            principal_kind: PrincipalKind::Admin,
            grants: vec!["admin".to_string()],
        }
    }
}
