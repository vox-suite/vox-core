/**
 * Amazon Provider Integration: Creators API catalog discovery and labelled purchase handoff (E35).
 *
 * Conforms to E02 provider feasibility finding (L1 catalog read, L0 labelled handoff):
 * - Creators API supports catalog search, item metadata, and pricing across 22 official locales.
 * - Consumer purchase is NOT supported by official Amazon APIs; direct execution is declared
 *   unavailable and produces an explicit labelled handoff.
 * - Handoff is NEVER reported as purchase completion.
 */
use crate::{
    capability_grants::{CapabilityGrantError, CapabilityGrantService},
    connections::{AuthorizationState, ConnectionError, ConnectionService},
    db::Db,
    identity::ResolvedUserContext,
    integration_registry::{
        CapabilityDeclaration, CapabilityEffect, IntegrationProtocol, RegisterIntegrationRequest,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

pub const AMAZON_INTEGRATION_KEY: &str = "amazon";
pub const AMAZON_CAPABILITY_CATALOG_SEARCH: &str = "amazon.catalog_search";
pub const AMAZON_CAPABILITY_ITEM_LOOKUP: &str = "amazon.item_lookup";
pub const AMAZON_CAPABILITY_PURCHASE_HANDOFF: &str = "amazon.purchase_handoff";

pub const AMAZON_CAPABILITY_CATALOG_SEARCH_SHORT: &str = "catalog_search";
pub const AMAZON_CAPABILITY_ITEM_LOOKUP_SHORT: &str = "item_lookup";
pub const AMAZON_CAPABILITY_PURCHASE_HANDOFF_SHORT: &str = "purchase_handoff";

/// Official 22 marketplace locales documented by Amazon Creators API.
pub const AMAZON_OFFICIAL_LOCALES: &[&str] = &[
    "US", "CA", "BR", "MX", "GB", "DE", "FR", "ES", "IT", "NL", "PL", "SE", "TR", "AE", "SA", "EG",
    "IN", "JP", "SG", "AU", "BE", "IE",
];

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AmazonCatalogItem {
    pub asin: String,
    pub title: String,
    pub detail_page_url: String,
    pub price_amount_minor: i64,
    pub currency: String,
    pub availability: String,
    pub image_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AmazonCatalogSearchResponse {
    pub query: String,
    pub locale: String,
    pub items: Vec<AmazonCatalogItem>,
    pub total_results: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AmazonHandoffRequest {
    pub asin: String,
    pub locale: String,
    pub quantity: u32,
    pub partner_tag: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AmazonHandoffResponse {
    pub provider: String,
    pub action: String,
    pub asin: String,
    pub locale: String,
    pub handoff_url: String,
    pub status: String,
    pub completed: bool,
    pub disclaimer: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AmazonError {
    #[error("connection not found")]
    ConnectionNotFound,
    #[error("connection is not authorized for integration amazon")]
    InvalidIntegration,
    #[error("access expired or revoked; reconnection required")]
    ReconnectRequired,
    #[error("capability {0} is not granted to agent {1}")]
    UnauthorizedCapability(String, String),
    #[error("unsupported locale: {0}. Supported locales are: {1}")]
    UnsupportedLocale(String, String),
    #[error("unsupported direct execution: {0}")]
    UnsupportedDirectExecution(String),
    #[error("rate limited by Amazon API; retry after {0} seconds")]
    RateLimited(u64),
    #[error("provider error: {0}")]
    ProviderError(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[async_trait::async_trait]
pub trait AmazonProviderClient: Send + Sync {
    async fn search_catalog(
        &self,
        query: &str,
        locale: &str,
    ) -> Result<AmazonCatalogSearchResponse, AmazonError>;

    async fn get_item(
        &self,
        asin: &str,
        locale: &str,
    ) -> Result<Option<AmazonCatalogItem>, AmazonError>;
}

pub struct DefaultAmazonProviderClient {
    pub base_url: String,
    pub http: reqwest::Client,
}

impl DefaultAmazonProviderClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl AmazonProviderClient for DefaultAmazonProviderClient {
    async fn search_catalog(
        &self,
        query: &str,
        locale: &str,
    ) -> Result<AmazonCatalogSearchResponse, AmazonError> {
        let domain_tld = locale_to_tld(locale);
        let url = format!("{}/catalog/search", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[("q", query), ("locale", locale), ("tld", domain_tld)])
            .send()
            .await
            .map_err(|e| AmazonError::ProviderError(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(AmazonError::RateLimited(60));
        }

        if !resp.status().is_success() {
            return Err(AmazonError::ProviderError(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        resp.json::<AmazonCatalogSearchResponse>()
            .await
            .map_err(|e| AmazonError::ProviderError(e.to_string()))
    }

    async fn get_item(
        &self,
        asin: &str,
        locale: &str,
    ) -> Result<Option<AmazonCatalogItem>, AmazonError> {
        let domain_tld = locale_to_tld(locale);
        let url = format!("{}/catalog/items/{}", self.base_url, asin);
        let resp = self
            .http
            .get(&url)
            .query(&[("locale", locale), ("tld", domain_tld)])
            .send()
            .await
            .map_err(|e| AmazonError::ProviderError(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !resp.status().is_success() {
            return Err(AmazonError::ProviderError(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        let item = resp
            .json::<AmazonCatalogItem>()
            .await
            .map_err(|e| AmazonError::ProviderError(e.to_string()))?;
        Ok(Some(item))
    }
}

pub struct MockAmazonProviderClient {
    pub items: std::sync::Mutex<Vec<AmazonCatalogItem>>,
    pub fail_with_rate_limit: std::sync::atomic::AtomicBool,
}

impl MockAmazonProviderClient {
    pub fn new(items: Vec<AmazonCatalogItem>) -> Self {
        Self {
            items: std::sync::Mutex::new(items),
            fail_with_rate_limit: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl AmazonProviderClient for MockAmazonProviderClient {
    async fn search_catalog(
        &self,
        query: &str,
        locale: &str,
    ) -> Result<AmazonCatalogSearchResponse, AmazonError> {
        if self
            .fail_with_rate_limit
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(AmazonError::RateLimited(45));
        }
        let lock = self.items.lock().unwrap();
        let q_lower = query.to_lowercase();
        let matched: Vec<AmazonCatalogItem> = lock
            .iter()
            .filter(|it| {
                it.title.to_lowercase().contains(&q_lower)
                    || it.asin.to_lowercase().contains(&q_lower)
            })
            .cloned()
            .collect();
        let total = matched.len();
        Ok(AmazonCatalogSearchResponse {
            query: query.into(),
            locale: locale.into(),
            items: matched,
            total_results: total,
        })
    }

    async fn get_item(
        &self,
        asin: &str,
        _locale: &str,
    ) -> Result<Option<AmazonCatalogItem>, AmazonError> {
        if self
            .fail_with_rate_limit
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(AmazonError::RateLimited(45));
        }
        let lock = self.items.lock().unwrap();
        Ok(lock.iter().find(|it| it.asin == asin).cloned())
    }
}

#[derive(Clone)]
pub struct AmazonService {
    _db: Db,
    connections: ConnectionService,
    grants: CapabilityGrantService,
    client: Arc<dyn AmazonProviderClient>,
}

impl AmazonService {
    pub fn new(
        _db: Db,
        connections: ConnectionService,
        grants: CapabilityGrantService,
        client: Arc<dyn AmazonProviderClient>,
    ) -> Self {
        Self {
            _db,
            connections,
            grants,
            client,
        }
    }

    /// Returns the canonical integration declaration for Amazon matching E02 provider feasibility.
    pub fn integration_declaration(deployment_external_key: &str) -> RegisterIntegrationRequest {
        RegisterIntegrationRequest {
            deployment_external_key: deployment_external_key.into(),
            external_key: AMAZON_INTEGRATION_KEY.into(),
            protocol: IntegrationProtocol::Direct,
            display_name: "Amazon".into(),
            declaration_version: 1,
            capabilities: vec![
                CapabilityDeclaration {
                    external_key: AMAZON_CAPABILITY_CATALOG_SEARCH_SHORT.into(),
                    effect: CapabilityEffect::Read,
                    access_needs: vec!["creators_api_read".into()],
                    data_recipients: vec!["webservices.amazon.com".into()],
                    regions: AMAZON_OFFICIAL_LOCALES
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    failure_modes: vec!["rate_limited".into(), "invalid_locale".into()],
                    optional_guarantees: json!({
                        "capability_level": "L1_catalog_read",
                        "supported_locales_count": 22,
                    }),
                },
                CapabilityDeclaration {
                    external_key: AMAZON_CAPABILITY_ITEM_LOOKUP_SHORT.into(),
                    effect: CapabilityEffect::Read,
                    access_needs: vec!["creators_api_read".into()],
                    data_recipients: vec!["webservices.amazon.com".into()],
                    regions: AMAZON_OFFICIAL_LOCALES
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    failure_modes: vec!["rate_limited".into(), "not_found".into()],
                    optional_guarantees: json!({
                        "capability_level": "L1_catalog_read",
                    }),
                },
                CapabilityDeclaration {
                    external_key: AMAZON_CAPABILITY_PURCHASE_HANDOFF_SHORT.into(),
                    effect: CapabilityEffect::Write,
                    access_needs: vec![],
                    data_recipients: vec!["amazon.com".into()],
                    regions: AMAZON_OFFICIAL_LOCALES
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    failure_modes: vec!["unsupported_direct_execution".into()],
                    optional_guarantees: json!({
                        "capability_level": "L0_labelled_handoff_only",
                        "direct_execution_supported": false,
                        "checkout_managed_by": "amazon",
                    }),
                },
            ],
        }
    }

    /// Searches Amazon product catalog in an official locale.
    pub async fn search_catalog(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        query: &str,
        locale: &str,
    ) -> Result<AmazonCatalogSearchResponse, AmazonError> {
        let norm_locale = locale.trim().to_uppercase();
        if !AMAZON_OFFICIAL_LOCALES.contains(&norm_locale.as_str()) {
            return Err(AmazonError::UnsupportedLocale(
                locale.into(),
                AMAZON_OFFICIAL_LOCALES.join(", "),
            ));
        }

        self.verify_access(
            context,
            agent_external_key,
            connection_id,
            AMAZON_CAPABILITY_CATALOG_SEARCH,
        )
        .await?;
        self.client.search_catalog(query, &norm_locale).await
    }

    /// Looks up item details by ASIN in an official locale.
    pub async fn get_item(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        asin: &str,
        locale: &str,
    ) -> Result<Option<AmazonCatalogItem>, AmazonError> {
        let norm_locale = locale.trim().to_uppercase();
        if !AMAZON_OFFICIAL_LOCALES.contains(&norm_locale.as_str()) {
            return Err(AmazonError::UnsupportedLocale(
                locale.into(),
                AMAZON_OFFICIAL_LOCALES.join(", "),
            ));
        }

        self.verify_access(
            context,
            agent_external_key,
            connection_id,
            AMAZON_CAPABILITY_ITEM_LOOKUP,
        )
        .await?;
        self.client.get_item(asin, &norm_locale).await
    }

    /// Creates an explicit labelled handoff to Amazon product detail page.
    ///
    /// GUARANTEE: Handoff is NEVER reported as purchase completion.
    /// Direct consumer purchase is unavailable in official APIs and strictly rejected.
    pub async fn create_purchase_handoff(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        request: AmazonHandoffRequest,
    ) -> Result<AmazonHandoffResponse, AmazonError> {
        let norm_locale = request.locale.trim().to_uppercase();
        if !AMAZON_OFFICIAL_LOCALES.contains(&norm_locale.as_str()) {
            return Err(AmazonError::UnsupportedLocale(
                request.locale,
                AMAZON_OFFICIAL_LOCALES.join(", "),
            ));
        }

        self.verify_access(
            context,
            agent_external_key,
            connection_id,
            AMAZON_CAPABILITY_PURCHASE_HANDOFF,
        )
        .await?;

        let tld = locale_to_tld(&norm_locale);
        let tag_query = match &request.partner_tag {
            Some(tag) if !tag.trim().is_empty() => format!("?tag={}", tag.trim()),
            _ => String::new(),
        };

        let handoff_url = format!(
            "https://www.amazon.{}/dp/{}{}",
            tld,
            request.asin.trim(),
            tag_query
        );

        Ok(AmazonHandoffResponse {
            provider: AMAZON_INTEGRATION_KEY.into(),
            action: "view_item_and_checkout".into(),
            asin: request.asin.trim().into(),
            locale: norm_locale,
            handoff_url,
            status: "handoff_created".into(),
            completed: false, // Invariant: handoff is never reported as purchase completion
            disclaimer: "Checkout, payment, delivery, and order management are completed directly on Amazon. Vox does not place consumer orders directly.".into(),
        })
    }

    /// Enforces that direct execution is unsupported for Amazon consumer purchasing.
    pub async fn execute_purchase(
        &self,
        _context: &ResolvedUserContext,
        _agent_external_key: &str,
        _connection_id: Uuid,
        _asin: &str,
    ) -> Result<(), AmazonError> {
        Err(AmazonError::UnsupportedDirectExecution(
            "Amazon consumer purchase is unsupported and must use labelled handoff. Direct consumer purchase APIs are not offered by Amazon Creators API.".into(),
        ))
    }

    async fn verify_access(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        capability: &str,
    ) -> Result<(), AmazonError> {
        let connection =
            self.connections
                .get(context, connection_id)
                .await
                .map_err(|e| match e {
                    ConnectionError::NotFound => AmazonError::ConnectionNotFound,
                    ConnectionError::Database(err) => AmazonError::Database(err),
                    _ => AmazonError::ConnectionNotFound,
                })?;

        if connection.integration_external_key != AMAZON_INTEGRATION_KEY {
            return Err(AmazonError::InvalidIntegration);
        }

        if connection.authorization_state != AuthorizationState::Authorized {
            return Err(AmazonError::ReconnectRequired);
        }

        if connection
            .expires_at
            .is_some_and(|exp| exp <= chrono::Utc::now())
        {
            return Err(AmazonError::ReconnectRequired);
        }

        let grants = self
            .grants
            .effective_for_agent(context, agent_external_key)
            .await
            .map_err(|e| match e {
                CapabilityGrantError::Database(err) => AmazonError::Database(err),
                _ => AmazonError::UnauthorizedCapability(
                    capability.into(),
                    agent_external_key.into(),
                ),
            })?;

        let has_grant = grants
            .iter()
            .any(|g| g.connection_id == connection_id && g.capability_external_key == capability);

        if !has_grant {
            return Err(AmazonError::UnauthorizedCapability(
                capability.into(),
                agent_external_key.into(),
            ));
        }

        Ok(())
    }
}

fn locale_to_tld(locale: &str) -> &'static str {
    match locale {
        "US" => "com",
        "GB" => "co.uk",
        "CA" => "ca",
        "DE" => "de",
        "FR" => "fr",
        "ES" => "es",
        "IT" => "it",
        "JP" => "co.jp",
        "IN" => "in",
        "AU" => "com.au",
        "BR" => "com.br",
        "MX" => "com.mx",
        "NL" => "nl",
        "PL" => "pl",
        "SE" => "se",
        "TR" => "com.tr",
        "AE" => "ae",
        "SA" => "sa",
        "EG" => "eg",
        "SG" => "sg",
        "BE" => "com.be",
        "IE" => "ie",
        _ => "com",
    }
}
