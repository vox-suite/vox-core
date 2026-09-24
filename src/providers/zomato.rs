/**
 * Zomato Provider Integration: Restaurant discovery and labelled consumer order handoff (E37).
 *
 * Conforms to E02 provider feasibility finding (L0 labelled handoff for consumer use):
 * - Merchant/POS APIs are strictly distinguished and NEVER presented as consumer ordering.
 * - Consumer food ordering is unsupported for direct third-party execution; it produces an
 *   explicit labelled handoff to Zomato mobile app or web.
 * - Opening Zomato or generating a cart/order link is NEVER reported as order completion.
 * - Supports restaurant discovery, cart handoff, order tracking handoff, and reorder handoff.
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

pub const ZOMATO_INTEGRATION_KEY: &str = "zomato";
pub const ZOMATO_CAPABILITY_RESTAURANT_SEARCH: &str = "zomato.restaurant_search";
pub const ZOMATO_CAPABILITY_RESTAURANT_VIEW: &str = "zomato.restaurant_view";
pub const ZOMATO_CAPABILITY_ORDER_HANDOFF: &str = "zomato.consumer_order_handoff";

pub const ZOMATO_CAPABILITY_RESTAURANT_SEARCH_SHORT: &str = "restaurant_search";
pub const ZOMATO_CAPABILITY_RESTAURANT_VIEW_SHORT: &str = "restaurant_view";
pub const ZOMATO_CAPABILITY_ORDER_HANDOFF_SHORT: &str = "consumer_order_handoff";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ZomatoRestaurant {
    pub res_id: String,
    pub name: String,
    pub cuisines: Vec<String>,
    pub locality: String,
    pub city: String,
    pub rating: f32,
    pub average_cost_for_two_minor: i64,
    pub currency: String,
    pub web_url: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ZomatoSearchResponse {
    pub query: String,
    pub city: String,
    pub restaurants: Vec<ZomatoRestaurant>,
    pub total_results: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum ZomatoHandoffType {
    ViewRestaurant,
    CartAndCheckout,
    TrackOrder,
    Reorder,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ZomatoHandoffRequest {
    pub res_id: Option<String>,
    pub order_id: Option<String>,
    pub handoff_type: ZomatoHandoffType,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ZomatoHandoffResponse {
    pub provider: String,
    pub action: String,
    pub handoff_url: String,
    pub status: String,
    pub completed: bool,
    pub disclaimer: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ZomatoError {
    #[error("connection not found")]
    ConnectionNotFound,
    #[error("connection is not authorized for integration zomato")]
    InvalidIntegration,
    #[error("access expired or revoked; reconnection required")]
    ReconnectRequired,
    #[error("capability {0} is not granted to agent {1}")]
    UnauthorizedCapability(String, String),
    #[error("unsupported direct execution: {0}")]
    UnsupportedDirectExecution(String),
    #[error("missing required identifier for handoff type")]
    MissingParameter,
    #[error("rate limited by Zomato API; retry after {0} seconds")]
    RateLimited(u64),
    #[error("provider error: {0}")]
    ProviderError(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[async_trait::async_trait]
pub trait ZomatoProviderClient: Send + Sync {
    async fn search_restaurants(
        &self,
        query: &str,
        city: &str,
    ) -> Result<ZomatoSearchResponse, ZomatoError>;

    async fn get_restaurant(&self, res_id: &str) -> Result<Option<ZomatoRestaurant>, ZomatoError>;
}

pub struct DefaultZomatoProviderClient {
    pub base_url: String,
    pub http: reqwest::Client,
}

impl DefaultZomatoProviderClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl ZomatoProviderClient for DefaultZomatoProviderClient {
    async fn search_restaurants(
        &self,
        query: &str,
        city: &str,
    ) -> Result<ZomatoSearchResponse, ZomatoError> {
        let url = format!("{}/restaurants/search", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[("q", query), ("city", city)])
            .send()
            .await
            .map_err(|e| ZomatoError::ProviderError(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ZomatoError::RateLimited(60));
        }

        if !resp.status().is_success() {
            return Err(ZomatoError::ProviderError(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        resp.json::<ZomatoSearchResponse>()
            .await
            .map_err(|e| ZomatoError::ProviderError(e.to_string()))
    }

    async fn get_restaurant(&self, res_id: &str) -> Result<Option<ZomatoRestaurant>, ZomatoError> {
        let url = format!("{}/restaurants/{}", self.base_url, res_id);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| ZomatoError::ProviderError(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !resp.status().is_success() {
            return Err(ZomatoError::ProviderError(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        let r = resp
            .json::<ZomatoRestaurant>()
            .await
            .map_err(|e| ZomatoError::ProviderError(e.to_string()))?;
        Ok(Some(r))
    }
}

pub struct MockZomatoProviderClient {
    pub restaurants: std::sync::Mutex<Vec<ZomatoRestaurant>>,
    pub fail_with_rate_limit: std::sync::atomic::AtomicBool,
}

impl MockZomatoProviderClient {
    pub fn new(restaurants: Vec<ZomatoRestaurant>) -> Self {
        Self {
            restaurants: std::sync::Mutex::new(restaurants),
            fail_with_rate_limit: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl ZomatoProviderClient for MockZomatoProviderClient {
    async fn search_restaurants(
        &self,
        query: &str,
        city: &str,
    ) -> Result<ZomatoSearchResponse, ZomatoError> {
        if self
            .fail_with_rate_limit
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ZomatoError::RateLimited(45));
        }
        let lock = self.restaurants.lock().unwrap();
        let q_lower = query.to_lowercase();
        let c_lower = city.to_lowercase();
        let matched: Vec<ZomatoRestaurant> = lock
            .iter()
            .filter(|r| {
                (r.name.to_lowercase().contains(&q_lower)
                    || r.cuisines
                        .iter()
                        .any(|c| c.to_lowercase().contains(&q_lower)))
                    && (c_lower.is_empty() || r.city.to_lowercase().contains(&c_lower))
            })
            .cloned()
            .collect();
        let total = matched.len();
        Ok(ZomatoSearchResponse {
            query: query.into(),
            city: city.into(),
            restaurants: matched,
            total_results: total,
        })
    }

    async fn get_restaurant(&self, res_id: &str) -> Result<Option<ZomatoRestaurant>, ZomatoError> {
        if self
            .fail_with_rate_limit
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ZomatoError::RateLimited(45));
        }
        let lock = self.restaurants.lock().unwrap();
        Ok(lock.iter().find(|r| r.res_id == res_id).cloned())
    }
}

#[derive(Clone)]
pub struct ZomatoService {
    _db: Db,
    connections: ConnectionService,
    grants: CapabilityGrantService,
    client: Arc<dyn ZomatoProviderClient>,
}

impl ZomatoService {
    pub fn new(
        _db: Db,
        connections: ConnectionService,
        grants: CapabilityGrantService,
        client: Arc<dyn ZomatoProviderClient>,
    ) -> Self {
        Self {
            _db,
            connections,
            grants,
            client,
        }
    }

    /// Returns the canonical integration declaration for Zomato matching E02 provider feasibility.
    pub fn integration_declaration(deployment_external_key: &str) -> RegisterIntegrationRequest {
        RegisterIntegrationRequest {
            deployment_external_key: deployment_external_key.into(),
            external_key: ZOMATO_INTEGRATION_KEY.into(),
            protocol: IntegrationProtocol::Direct,
            display_name: "Zomato".into(),
            declaration_version: 1,
            capabilities: vec![
                CapabilityDeclaration {
                    external_key: ZOMATO_CAPABILITY_RESTAURANT_SEARCH_SHORT.into(),
                    effect: CapabilityEffect::Read,
                    access_needs: vec!["restaurant_content".into()],
                    data_recipients: vec!["api.zomato.com".into()],
                    regions: vec!["IN".into()],
                    failure_modes: vec!["rate_limited".into()],
                    optional_guarantees: json!({
                        "capability_level": "L1_catalog_read",
                    }),
                },
                CapabilityDeclaration {
                    external_key: ZOMATO_CAPABILITY_RESTAURANT_VIEW_SHORT.into(),
                    effect: CapabilityEffect::Read,
                    access_needs: vec!["restaurant_content".into()],
                    data_recipients: vec!["api.zomato.com".into()],
                    regions: vec!["IN".into()],
                    failure_modes: vec!["rate_limited".into(), "not_found".into()],
                    optional_guarantees: json!({
                        "capability_level": "L1_catalog_read",
                    }),
                },
                CapabilityDeclaration {
                    external_key: ZOMATO_CAPABILITY_ORDER_HANDOFF_SHORT.into(),
                    effect: CapabilityEffect::Write,
                    access_needs: vec![],
                    data_recipients: vec!["zomato.com".into()],
                    regions: vec!["IN".into()],
                    failure_modes: vec!["unsupported_direct_execution".into()],
                    optional_guarantees: json!({
                        "capability_level": "L0_labelled_handoff_only",
                        "direct_execution_supported": false,
                        "order_managed_by": "zomato",
                    }),
                },
            ],
        }
    }

    /// Searches restaurants in a specified locality/city.
    pub async fn search_restaurants(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        query: &str,
        city: &str,
    ) -> Result<ZomatoSearchResponse, ZomatoError> {
        self.verify_access(
            context,
            agent_external_key,
            connection_id,
            ZOMATO_CAPABILITY_RESTAURANT_SEARCH,
        )
        .await?;
        self.client.search_restaurants(query, city).await
    }

    /// Looks up restaurant details by ID.
    pub async fn get_restaurant(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        res_id: &str,
    ) -> Result<Option<ZomatoRestaurant>, ZomatoError> {
        self.verify_access(
            context,
            agent_external_key,
            connection_id,
            ZOMATO_CAPABILITY_RESTAURANT_VIEW,
        )
        .await?;
        self.client.get_restaurant(res_id).await
    }

    /// Creates an explicit labelled handoff for food ordering or tracking on Zomato.
    ///
    /// GUARANTEE: Handoff is NEVER reported as order completion.
    /// Direct consumer ordering is unavailable in official APIs and strictly rejected.
    pub async fn create_order_handoff(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        request: ZomatoHandoffRequest,
    ) -> Result<ZomatoHandoffResponse, ZomatoError> {
        self.verify_access(
            context,
            agent_external_key,
            connection_id,
            ZOMATO_CAPABILITY_ORDER_HANDOFF,
        )
        .await?;

        let (action, handoff_url) = match request.handoff_type {
            ZomatoHandoffType::ViewRestaurant => {
                let id = request
                    .res_id
                    .as_deref()
                    .ok_or(ZomatoError::MissingParameter)?;
                (
                    "view_restaurant",
                    format!("https://www.zomato.com/restaurant/{}", id.trim()),
                )
            }
            ZomatoHandoffType::CartAndCheckout => {
                let id = request
                    .res_id
                    .as_deref()
                    .ok_or(ZomatoError::MissingParameter)?;
                (
                    "cart_and_checkout",
                    format!("https://www.zomato.com/order/{}", id.trim()),
                )
            }
            ZomatoHandoffType::TrackOrder => {
                let order_id = request
                    .order_id
                    .as_deref()
                    .ok_or(ZomatoError::MissingParameter)?;
                (
                    "track_order",
                    format!("https://www.zomato.com/order/track/{}", order_id.trim()),
                )
            }
            ZomatoHandoffType::Reorder => ("reorder", "https://www.zomato.com/user/orders".into()),
        };

        Ok(ZomatoHandoffResponse {
            provider: ZOMATO_INTEGRATION_KEY.into(),
            action: action.into(),
            handoff_url,
            status: "handoff_created".into(),
            completed: false, // Invariant: handoff is never reported as order completion
            disclaimer: "Order placement, payment, and delivery tracking take place directly in the Zomato app or website. Vox does not place consumer orders directly.".into(),
        })
    }

    /// Enforces that direct execution is unsupported for Zomato consumer ordering.
    /// Merchant/POS APIs are NOT consumer ordering APIs.
    pub async fn execute_order(
        &self,
        _context: &ResolvedUserContext,
        _agent_external_key: &str,
        _connection_id: Uuid,
        _items: serde_json::Value,
    ) -> Result<(), ZomatoError> {
        Err(ZomatoError::UnsupportedDirectExecution(
            "Zomato developer APIs are merchant POS integration APIs, not consumer ordering APIs. Direct consumer order creation is unavailable and must use labelled handoff.".into(),
        ))
    }

    async fn verify_access(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        capability: &str,
    ) -> Result<(), ZomatoError> {
        let connection =
            self.connections
                .get(context, connection_id)
                .await
                .map_err(|e| match e {
                    ConnectionError::NotFound => ZomatoError::ConnectionNotFound,
                    ConnectionError::Database(err) => ZomatoError::Database(err),
                    _ => ZomatoError::ConnectionNotFound,
                })?;

        if connection.integration_external_key != ZOMATO_INTEGRATION_KEY {
            return Err(ZomatoError::InvalidIntegration);
        }

        if connection.authorization_state != AuthorizationState::Authorized {
            return Err(ZomatoError::ReconnectRequired);
        }

        if connection
            .expires_at
            .is_some_and(|exp| exp <= chrono::Utc::now())
        {
            return Err(ZomatoError::ReconnectRequired);
        }

        let grants = self
            .grants
            .effective_for_agent(context, agent_external_key)
            .await
            .map_err(|e| match e {
                CapabilityGrantError::Database(err) => ZomatoError::Database(err),
                _ => ZomatoError::UnauthorizedCapability(
                    capability.into(),
                    agent_external_key.into(),
                ),
            })?;

        let has_grant = grants
            .iter()
            .any(|g| g.connection_id == connection_id && g.capability_external_key == capability);

        if !has_grant {
            return Err(ZomatoError::UnauthorizedCapability(
                capability.into(),
                agent_external_key.into(),
            ));
        }

        Ok(())
    }
}
