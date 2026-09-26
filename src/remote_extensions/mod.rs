/**
* Remote extension governance, remote-only lifecycle, sandboxed capability boundaries, and renewed consent.
*/
use crate::{db::Db, identity::ResolvedUserContext};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use std::net::IpAddr;
use url::{Host, Url};
use uuid::Uuid;

pub mod adapters;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionProtocol {
    Mcp,
    Direct,
}

impl ExtensionProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Direct => "direct",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

impl std::str::FromStr for ExtensionProtocol {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mcp" => Ok(Self::Mcp),
            "direct" => Ok(Self::Direct),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionEffect {
    Read,
    Write,
    Mixed,
}

impl ExtensionEffect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Mixed => "mixed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }

    pub fn is_consequential(&self) -> bool {
        matches!(self, Self::Write | Self::Mixed)
    }
}

impl std::str::FromStr for ExtensionEffect {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "mixed" => Ok(Self::Mixed),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceStatus {
    Pending,
    Passed,
    Failed,
}

impl ConformanceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

impl std::str::FromStr for ConformanceStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "passed" => Ok(Self::Passed),
            "failed" => Ok(Self::Failed),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Installed,
    Active,
    Quarantined,
    Disabled,
    Removed,
}

impl LifecycleState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Active => "active",
            Self::Quarantined => "quarantined",
            Self::Disabled => "disabled",
            Self::Removed => "removed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

impl std::str::FromStr for LifecycleState {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "installed" => Ok(Self::Installed),
            "active" => Ok(Self::Active),
            "quarantined" => Ok(Self::Quarantined),
            "disabled" => Ok(Self::Disabled),
            "removed" => Ok(Self::Removed),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentStatus {
    Consented,
    ConsentRequired,
}

impl ConsentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Consented => "consented",
            Self::ConsentRequired => "consent_required",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

impl std::str::FromStr for ConsentStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "consented" => Ok(Self::Consented),
            "consent_required" => Ok(Self::ConsentRequired),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExtensionOperator {
    pub operator_id: String,
    pub operator_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExtensionCapability {
    pub external_key: String,
    pub display_name: String,
    pub effect: ExtensionEffect,
    #[serde(default)]
    pub consequential: bool,
    #[serde(default)]
    pub data_recipients: Vec<String>,
    #[serde(default)]
    pub access_needs: Vec<String>,
    #[serde(default)]
    pub optional_guarantees: Value,
}

impl ExtensionCapability {
    pub fn optional_guarantees(&self) -> Value {
        self.optional_guarantees.clone()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InstallExtensionRequest {
    pub external_key: String,
    pub display_name: String,
    pub protocol: ExtensionProtocol,
    pub endpoint_url: String,
    pub operator: ExtensionOperator,
    pub capabilities: Vec<ExtensionCapability>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UpdateExtensionRequest {
    pub endpoint_url: Option<String>,
    pub operator: Option<ExtensionOperator>,
    pub capabilities: Option<Vec<ExtensionCapability>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RemoteExtension {
    pub id: Uuid,
    pub external_key: String,
    pub display_name: String,
    pub protocol: ExtensionProtocol,
    pub endpoint_url: String,
    pub operator: ExtensionOperator,
    pub current_version: i32,
    pub conformance_status: ConformanceStatus,
    pub operator_enabled: bool,
    pub consent_status: ConsentStatus,
    pub lifecycle_state: LifecycleState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub capabilities: Vec<ExtensionCapability>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RemoteExtensionVersion {
    pub version: i32,
    pub endpoint_url: String,
    pub operator: ExtensionOperator,
    pub capabilities: Vec<ExtensionCapability>,
    pub conformance_status: ConformanceStatus,
    pub conformance_report: Value,
    pub consent_granted_at: Option<DateTime<Utc>>,
    pub quarantined_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AuthorizedEndpoint {
    pub extension_id: Uuid,
    pub endpoint_url: String,
    pub protocol: ExtensionProtocol,
    pub capability: ExtensionCapability,
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteExtensionError {
    #[error("invalid extension manifest or endpoint")]
    Invalid,
    #[error("extension not found")]
    NotFound,
    #[error("extension is not active (state: {0:?})")]
    NotActive(LifecycleState),
    #[error("consequential capability requires passed conformance and operator enablement")]
    ConsequentialUnavailable,
    #[error("renewed consent required due to operator or data recipient changes")]
    ConsentRequired,
    #[error("extension key already exists")]
    Conflict,
    #[error("extension version is quarantined")]
    Quarantined,
    #[error("untrusted code upload is strictly prohibited; only remote endpoints are supported")]
    LocalCodeProhibited,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct RemoteExtensionService {
    db: Db,
    allow_local_endpoints: bool,
}

pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 198 && (b == 18 || b == 19))
                || (a == 192 && b == 0 && c == 0))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(mapped));
            }
            let first = ip.segments()[0];
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

impl RemoteExtensionService {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            allow_local_endpoints: false,
        }
    }

    /// For isolated integration tests with a loopback mock server only.
    pub fn with_local_endpoints_for_testing(mut self) -> Self {
        self.allow_local_endpoints = true;
        self
    }

    fn validate_endpoint_url(&self, url_str: &str) -> Result<String, RemoteExtensionError> {
        let trimmed = url_str.trim();
        let parsed = Url::parse(trimmed).map_err(|_| RemoteExtensionError::Invalid)?;
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(RemoteExtensionError::Invalid);
        }
        let host = parsed.host().ok_or(RemoteExtensionError::Invalid)?;
        let local_name = match host {
            Host::Domain(name) => {
                name.eq_ignore_ascii_case("localhost")
                    || name.ends_with(".localhost")
                    || name.ends_with(".local")
                    || name.ends_with(".internal")
            }
            _ => false,
        };
        let local_ip = match host {
            Host::Ipv4(ip) => !public_ip(IpAddr::V4(ip)),
            Host::Ipv6(ip) => !public_ip(IpAddr::V6(ip)),
            _ => false,
        };
        if (local_name || local_ip) && !self.allow_local_endpoints {
            return Err(RemoteExtensionError::Invalid);
        }
        if parsed.scheme() == "https"
            || (self.allow_local_endpoints && parsed.scheme() == "http" && (local_name || local_ip))
        {
            Ok(trimmed.to_string())
        } else {
            Err(RemoteExtensionError::Invalid)
        }
    }

    fn validate_key(key: &str) -> Result<String, RemoteExtensionError> {
        let trimmed = key.trim();
        if trimmed.is_empty() || trimmed.len() > 255 {
            return Err(RemoteExtensionError::Invalid);
        }
        Ok(trimmed.to_string())
    }

    pub async fn install(
        &self,
        context: &ResolvedUserContext,
        request: InstallExtensionRequest,
    ) -> Result<RemoteExtension, RemoteExtensionError> {
        let external_key = Self::validate_key(&request.external_key)?;
        let display_name = Self::validate_key(&request.display_name)?;
        let endpoint_url = self.validate_endpoint_url(&request.endpoint_url)?;

        if request.operator.operator_id.trim().is_empty()
            || request.operator.operator_name.trim().is_empty()
        {
            return Err(RemoteExtensionError::Invalid);
        }

        let mut tx = self.db.pool().begin().await?;

        let existing = sqlx::query(
            "SELECT id, current_version, lifecycle_state FROM remote_extensions \
             WHERE user_context_id = $1 AND external_key = $2 FOR UPDATE",
        )
        .bind(context.id.0)
        .bind(&external_key)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(row) = existing {
            let lifecycle_state: String = row.get("lifecycle_state");
            if lifecycle_state != "removed" {
                return Err(RemoteExtensionError::Conflict);
            }

            // Removal is a soft delete that keeps the row (and its version history) as
            // evidence, so reinstalling revives it as a fresh, unproven version rather
            // than colliding with the (user, external_key) unique constraint.
            let extension_id: Uuid = row.get("id");
            let next_version: i32 = row.get::<i32, _>("current_version") + 1;
            let capabilities_json = serde_json::to_value(&request.capabilities)
                .map_err(|_| RemoteExtensionError::Invalid)?;

            sqlx::query(
                "INSERT INTO remote_extension_versions (
                    extension_id, version, endpoint_url, operator_id, operator_name,
                    capabilities, conformance_status, conformance_report, consent_granted_at
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, 'pending', '{}'::jsonb, now()
                )",
            )
            .bind(extension_id)
            .bind(next_version)
            .bind(&endpoint_url)
            .bind(&request.operator.operator_id)
            .bind(&request.operator.operator_name)
            .bind(&capabilities_json)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE remote_extensions \
                 SET display_name = $2, protocol = $3, endpoint_url = $4, \
                     operator_id = $5, operator_name = $6, support_email = $7, terms_url = $8, \
                     current_version = $9, conformance_status = 'pending', operator_enabled = false, \
                     consent_status = 'consented', lifecycle_state = 'installed', updated_at = now() \
                 WHERE id = $1",
            )
            .bind(extension_id)
            .bind(&display_name)
            .bind(request.protocol.as_str())
            .bind(&endpoint_url)
            .bind(&request.operator.operator_id)
            .bind(&request.operator.operator_name)
            .bind(&request.operator.support_email)
            .bind(&request.operator.terms_url)
            .bind(next_version)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;
            return self.get(context, extension_id).await;
        }

        let extension_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO remote_extensions (
                user_context_id, external_key, display_name, protocol, endpoint_url,
                operator_id, operator_name, support_email, terms_url, current_version,
                conformance_status, operator_enabled, consent_status, lifecycle_state
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, 1, 'pending', false, 'consented', 'installed'
            ) RETURNING id",
        )
        .bind(context.id.0)
        .bind(&external_key)
        .bind(&display_name)
        .bind(request.protocol.as_str())
        .bind(&endpoint_url)
        .bind(&request.operator.operator_id)
        .bind(&request.operator.operator_name)
        .bind(&request.operator.support_email)
        .bind(&request.operator.terms_url)
        .fetch_one(&mut *tx)
        .await?;

        let capabilities_json = serde_json::to_value(&request.capabilities)
            .map_err(|_| RemoteExtensionError::Invalid)?;

        sqlx::query(
            "INSERT INTO remote_extension_versions (
                extension_id, version, endpoint_url, operator_id, operator_name,
                capabilities, conformance_status, conformance_report, consent_granted_at
            ) VALUES (
                $1, 1, $2, $3, $4, $5, 'pending', '{}'::jsonb, now()
            )",
        )
        .bind(extension_id)
        .bind(&endpoint_url)
        .bind(&request.operator.operator_id)
        .bind(&request.operator.operator_name)
        .bind(&capabilities_json)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(RemoteExtension {
            id: extension_id,
            external_key,
            display_name,
            protocol: request.protocol,
            endpoint_url,
            operator: request.operator,
            current_version: 1,
            conformance_status: ConformanceStatus::Pending,
            operator_enabled: false,
            consent_status: ConsentStatus::Consented,
            lifecycle_state: LifecycleState::Installed,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            capabilities: request.capabilities,
        })
    }

    pub async fn record_conformance(
        &self,
        context: &ResolvedUserContext,
        extension_id: Uuid,
        version: i32,
        passed: bool,
        report: Value,
    ) -> Result<RemoteExtension, RemoteExtensionError> {
        let mut tx = self.db.pool().begin().await?;

        let row = sqlx::query(
            "SELECT current_version, operator_enabled, consent_status, lifecycle_state \
             FROM remote_extensions WHERE id = $1 AND user_context_id = $2 FOR UPDATE",
        )
        .bind(extension_id)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RemoteExtensionError::NotFound)?;

        let current_version: i32 = row.get("current_version");
        let operator_enabled: bool = row.get("operator_enabled");
        let consent_status_str: String = row.get("consent_status");
        let mut lifecycle_state_str: String = row.get("lifecycle_state");

        let status_str = if passed { "passed" } else { "failed" };

        sqlx::query(
            "INSERT INTO remote_extension_conformance_runs (extension_id, version, status, report) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(extension_id)
        .bind(version)
        .bind(status_str)
        .bind(&report)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE remote_extension_versions \
             SET conformance_status = $3, conformance_report = $4 \
             WHERE extension_id = $1 AND version = $2",
        )
        .bind(extension_id)
        .bind(version)
        .bind(status_str)
        .bind(&report)
        .execute(&mut *tx)
        .await?;

        if version == current_version {
            if passed
                && operator_enabled
                && consent_status_str == "consented"
                && lifecycle_state_str != "quarantined"
                && lifecycle_state_str != "removed"
            {
                lifecycle_state_str = "active".into();
            }

            sqlx::query(
                "UPDATE remote_extensions \
                 SET conformance_status = $2, lifecycle_state = $3, updated_at = now() \
                 WHERE id = $1",
            )
            .bind(extension_id)
            .bind(status_str)
            .bind(&lifecycle_state_str)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        self.get(context, extension_id).await
    }

    pub async fn set_operator_enabled(
        &self,
        context: &ResolvedUserContext,
        extension_id: Uuid,
        enabled: bool,
    ) -> Result<RemoteExtension, RemoteExtensionError> {
        let mut tx = self.db.pool().begin().await?;

        let row = sqlx::query(
            "SELECT conformance_status, consent_status, lifecycle_state \
             FROM remote_extensions WHERE id = $1 AND user_context_id = $2 FOR UPDATE",
        )
        .bind(extension_id)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RemoteExtensionError::NotFound)?;

        let conformance_status: String = row.get("conformance_status");
        let consent_status: String = row.get("consent_status");
        let current_state: String = row.get("lifecycle_state");

        if current_state == "quarantined" {
            return Err(RemoteExtensionError::Quarantined);
        }
        if current_state == "removed" {
            return Err(RemoteExtensionError::NotActive(LifecycleState::Removed));
        }

        let new_state = if enabled {
            if conformance_status == "passed" && consent_status == "consented" {
                "active"
            } else {
                "installed"
            }
        } else {
            "disabled"
        };

        sqlx::query(
            "UPDATE remote_extensions \
             SET operator_enabled = $2, lifecycle_state = $3, updated_at = now() \
             WHERE id = $1",
        )
        .bind(extension_id)
        .bind(enabled)
        .bind(new_state)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.get(context, extension_id).await
    }

    pub async fn update(
        &self,
        context: &ResolvedUserContext,
        extension_id: Uuid,
        request: UpdateExtensionRequest,
    ) -> Result<RemoteExtension, RemoteExtensionError> {
        let mut tx = self.db.pool().begin().await?;

        let row = sqlx::query(
            "SELECT e.current_version, e.endpoint_url, e.operator_id, e.operator_name, \
                    e.operator_enabled, e.lifecycle_state, v.capabilities \
             FROM remote_extensions e \
             JOIN remote_extension_versions v ON v.extension_id = e.id AND v.version = e.current_version \
             WHERE e.id = $1 AND e.user_context_id = $2 FOR UPDATE",
        )
        .bind(extension_id)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RemoteExtensionError::NotFound)?;

        let current_version: i32 = row.get("current_version");
        let old_endpoint: String = row.get("endpoint_url");
        let old_operator_id: String = row.get("operator_id");
        let old_operator_name: String = row.get("operator_name");
        let old_capabilities: Vec<ExtensionCapability> =
            serde_json::from_value(row.get("capabilities")).unwrap_or_default();

        let new_endpoint = match request.endpoint_url.as_deref() {
            Some(url) => self.validate_endpoint_url(url)?,
            None => old_endpoint,
        };

        let new_operator = request.operator.unwrap_or(ExtensionOperator {
            operator_id: old_operator_id.clone(),
            operator_name: old_operator_name.clone(),
            support_email: None,
            terms_url: None,
        });

        let new_capabilities = request.capabilities.unwrap_or(old_capabilities.clone());

        // Check if operator changed
        let operator_changed = new_operator.operator_id != old_operator_id
            || new_operator.operator_name != old_operator_name;

        // Check if data recipients expanded
        let old_recipients: std::collections::HashSet<String> = old_capabilities
            .iter()
            .flat_map(|c| c.data_recipients.clone())
            .collect();
        let new_recipients: std::collections::HashSet<String> = new_capabilities
            .iter()
            .flat_map(|c| c.data_recipients.clone())
            .collect();
        let data_recipients_expanded = !new_recipients.is_subset(&old_recipients);

        let consent_required = operator_changed || data_recipients_expanded;
        let consent_status = if consent_required {
            "consent_required"
        } else {
            "consented"
        };
        let lifecycle_state = "installed"; // New version requires conformance testing

        let next_version = current_version + 1;
        let capabilities_json =
            serde_json::to_value(&new_capabilities).map_err(|_| RemoteExtensionError::Invalid)?;

        sqlx::query(
            "INSERT INTO remote_extension_versions (
                extension_id, version, endpoint_url, operator_id, operator_name,
                capabilities, conformance_status, conformance_report, consent_granted_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, 'pending', '{}'::jsonb, $7
            )",
        )
        .bind(extension_id)
        .bind(next_version)
        .bind(&new_endpoint)
        .bind(&new_operator.operator_id)
        .bind(&new_operator.operator_name)
        .bind(&capabilities_json)
        .bind(if consent_required {
            None
        } else {
            Some(Utc::now())
        })
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE remote_extensions \
             SET current_version = $2, endpoint_url = $3, operator_id = $4, operator_name = $5, \
                 conformance_status = 'pending', consent_status = $6, lifecycle_state = $7, updated_at = now() \
             WHERE id = $1",
        )
        .bind(extension_id)
        .bind(next_version)
        .bind(&new_endpoint)
        .bind(&new_operator.operator_id)
        .bind(&new_operator.operator_name)
        .bind(consent_status)
        .bind(lifecycle_state)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.get(context, extension_id).await
    }

    pub async fn renew_consent(
        &self,
        context: &ResolvedUserContext,
        extension_id: Uuid,
        version: i32,
    ) -> Result<RemoteExtension, RemoteExtensionError> {
        let mut tx = self.db.pool().begin().await?;

        let row = sqlx::query(
            "SELECT current_version, conformance_status, operator_enabled, lifecycle_state \
             FROM remote_extensions WHERE id = $1 AND user_context_id = $2 FOR UPDATE",
        )
        .bind(extension_id)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RemoteExtensionError::NotFound)?;

        let current_version: i32 = row.get("current_version");
        let conformance: String = row.get("conformance_status");
        let operator_enabled: bool = row.get("operator_enabled");
        let lifecycle_state: String = row.get("lifecycle_state");

        if version != current_version {
            return Err(RemoteExtensionError::Invalid);
        }
        if lifecycle_state == "quarantined" {
            return Err(RemoteExtensionError::Quarantined);
        }
        if lifecycle_state == "removed" {
            return Err(RemoteExtensionError::NotActive(LifecycleState::Removed));
        }

        sqlx::query(
            "UPDATE remote_extension_versions \
             SET consent_granted_at = now() \
             WHERE extension_id = $1 AND version = $2",
        )
        .bind(extension_id)
        .bind(version)
        .execute(&mut *tx)
        .await?;

        let new_state = if operator_enabled
            && conformance == "passed"
            && lifecycle_state != "quarantined"
            && lifecycle_state != "removed"
        {
            "active"
        } else {
            "installed"
        };

        sqlx::query(
            "UPDATE remote_extensions \
             SET consent_status = 'consented', lifecycle_state = $2, updated_at = now() \
             WHERE id = $1",
        )
        .bind(extension_id)
        .bind(new_state)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.get(context, extension_id).await
    }

    pub async fn quarantine(
        &self,
        context: &ResolvedUserContext,
        extension_id: Uuid,
        version: i32,
    ) -> Result<RemoteExtension, RemoteExtensionError> {
        let mut tx = self.db.pool().begin().await?;

        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM remote_extensions WHERE id = $1 AND user_context_id = $2)",
        )
        .bind(extension_id)
        .bind(context.id.0)
        .fetch_one(&mut *tx)
        .await?;

        if !exists {
            return Err(RemoteExtensionError::NotFound);
        }

        sqlx::query(
            "UPDATE remote_extension_versions \
             SET quarantined_at = now() \
             WHERE extension_id = $1 AND version = $2",
        )
        .bind(extension_id)
        .bind(version)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "UPDATE remote_extensions \
             SET lifecycle_state = 'quarantined', updated_at = now() \
             WHERE id = $1",
        )
        .bind(extension_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.get(context, extension_id).await
    }

    pub async fn remove(
        &self,
        context: &ResolvedUserContext,
        extension_id: Uuid,
    ) -> Result<RemoteExtension, RemoteExtensionError> {
        let mut tx = self.db.pool().begin().await?;

        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM remote_extensions WHERE id = $1 AND user_context_id = $2)",
        )
        .bind(extension_id)
        .bind(context.id.0)
        .fetch_one(&mut *tx)
        .await?;

        if !exists {
            return Err(RemoteExtensionError::NotFound);
        }

        sqlx::query(
            "UPDATE remote_extensions \
             SET lifecycle_state = 'removed', updated_at = now() \
             WHERE id = $1",
        )
        .bind(extension_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        self.get(context, extension_id).await
    }

    pub async fn authorize_call(
        &self,
        context: &ResolvedUserContext,
        extension_id: Uuid,
        capability_key: &str,
    ) -> Result<AuthorizedEndpoint, RemoteExtensionError> {
        let row = sqlx::query(
            "SELECT e.endpoint_url, e.protocol, e.conformance_status, e.operator_enabled, \
                    e.consent_status, e.lifecycle_state, v.capabilities \
             FROM remote_extensions e \
             JOIN remote_extension_versions v ON v.extension_id = e.id AND v.version = e.current_version \
             WHERE e.id = $1 AND e.user_context_id = $2",
        )
        .bind(extension_id)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(RemoteExtensionError::NotFound)?;

        let state_str: String = row.get("lifecycle_state");
        let lifecycle_state =
            LifecycleState::parse(&state_str).ok_or(RemoteExtensionError::Invalid)?;

        if lifecycle_state == LifecycleState::Quarantined {
            return Err(RemoteExtensionError::Quarantined);
        }
        if lifecycle_state != LifecycleState::Active {
            return Err(RemoteExtensionError::NotActive(lifecycle_state));
        }

        let consent_str: String = row.get("consent_status");
        if consent_str == "consent_required" {
            return Err(RemoteExtensionError::ConsentRequired);
        }

        let capabilities: Vec<ExtensionCapability> =
            serde_json::from_value(row.get("capabilities")).unwrap_or_default();

        let cap = capabilities
            .into_iter()
            .find(|c| c.external_key == capability_key)
            .ok_or(RemoteExtensionError::Invalid)?;

        if cap.consequential || cap.effect.is_consequential() {
            let conformance: String = row.get("conformance_status");
            let operator_enabled: bool = row.get("operator_enabled");
            if conformance != "passed" || !operator_enabled {
                return Err(RemoteExtensionError::ConsequentialUnavailable);
            }
        }

        let protocol_str: String = row.get("protocol");
        let protocol = ExtensionProtocol::parse(&protocol_str).unwrap_or(ExtensionProtocol::Mcp);

        Ok(AuthorizedEndpoint {
            extension_id,
            endpoint_url: row.get("endpoint_url"),
            protocol,
            capability: cap,
        })
    }

    pub async fn get(
        &self,
        context: &ResolvedUserContext,
        extension_id: Uuid,
    ) -> Result<RemoteExtension, RemoteExtensionError> {
        let row = sqlx::query(
            "SELECT e.id, e.external_key, e.display_name, e.protocol, e.endpoint_url, \
                    e.operator_id, e.operator_name, e.support_email, e.terms_url, e.current_version, \
                    e.conformance_status, e.operator_enabled, e.consent_status, e.lifecycle_state, \
                    e.created_at, e.updated_at, v.capabilities \
             FROM remote_extensions e \
             LEFT JOIN remote_extension_versions v ON v.extension_id = e.id AND v.version = e.current_version \
             WHERE e.id = $1 AND e.user_context_id = $2",
        )
        .bind(extension_id)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(RemoteExtensionError::NotFound)?;

        map_extension(row)
    }

    pub async fn list(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<RemoteExtension>, RemoteExtensionError> {
        let rows = sqlx::query(
            "SELECT e.id, e.external_key, e.display_name, e.protocol, e.endpoint_url, \
                    e.operator_id, e.operator_name, e.support_email, e.terms_url, e.current_version, \
                    e.conformance_status, e.operator_enabled, e.consent_status, e.lifecycle_state, \
                    e.created_at, e.updated_at, v.capabilities \
             FROM remote_extensions e \
             LEFT JOIN remote_extension_versions v ON v.extension_id = e.id AND v.version = e.current_version \
             WHERE e.user_context_id = $1 \
             ORDER BY e.created_at DESC",
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;

        rows.into_iter().map(map_extension).collect()
    }
}

fn map_extension(row: sqlx::postgres::PgRow) -> Result<RemoteExtension, RemoteExtensionError> {
    let protocol_str: String = row.get("protocol");
    let conformance_str: String = row.get("conformance_status");
    let consent_str: String = row.get("consent_status");
    let lifecycle_str: String = row.get("lifecycle_state");

    let capabilities: Vec<ExtensionCapability> = row
        .try_get("capabilities")
        .ok()
        .and_then(|v: serde_json::Value| serde_json::from_value(v).ok())
        .unwrap_or_default();

    Ok(RemoteExtension {
        id: row.get("id"),
        external_key: row.get("external_key"),
        display_name: row.get("display_name"),
        protocol: ExtensionProtocol::parse(&protocol_str).unwrap_or(ExtensionProtocol::Mcp),
        endpoint_url: row.get("endpoint_url"),
        operator: ExtensionOperator {
            operator_id: row.get("operator_id"),
            operator_name: row.get("operator_name"),
            support_email: row.get("support_email"),
            terms_url: row.get("terms_url"),
        },
        current_version: row.get("current_version"),
        conformance_status: ConformanceStatus::parse(&conformance_str)
            .unwrap_or(ConformanceStatus::Pending),
        operator_enabled: row.get("operator_enabled"),
        consent_status: ConsentStatus::parse(&consent_str).unwrap_or(ConsentStatus::Consented),
        lifecycle_state: LifecycleState::parse(&lifecycle_str).unwrap_or(LifecycleState::Installed),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        capabilities,
    })
}
