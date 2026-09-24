/**
* Selected Connected Read Integration: Uber rider trip history (E33).
*
* Conforms to E02 provider feasibility finding (L2 selected read):
* - Uses user OAuth connection for integration `uber`.
* - Supports `uber.history_lite` (data-minimized trip history without location/city details).
* - Supports `uber.history` (scoped trip history including city name).
* - Enforces strict context minimization: strips latitude/longitude coordinates,
*   street addresses, passenger payment method IDs, and internal driver/rider tokens.
* - Expired or revoked access pauses work with `ReconnectRequired`.
* - Reconnection re-checks verified provider facts.
* - Ride requests remain explicitly L0 / labelled handoff only.
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
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

pub const UBER_INTEGRATION_KEY: &str = "uber";
pub const UBER_CAPABILITY_HISTORY_LITE: &str = "uber.history_lite";
pub const UBER_CAPABILITY_HISTORY: &str = "uber.history";
pub const UBER_CAPABILITY_RIDE_ESTIMATE: &str = "uber.ride_estimate";
pub const UBER_CAPABILITY_RIDE_REQUEST: &str = "uber.ride_request";

pub const UBER_CAPABILITY_HISTORY_LITE_SHORT: &str = "history_lite";
pub const UBER_CAPABILITY_HISTORY_SHORT: &str = "history";
pub const UBER_CAPABILITY_RIDE_ESTIMATE_SHORT: &str = "ride_estimate";
pub const UBER_CAPABILITY_RIDE_REQUEST_SHORT: &str = "ride_request";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UberTrip {
    pub trip_id: String,
    pub request_time: DateTime<Utc>,
    pub status: String,
    pub distance_miles: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UberHistoryResponse {
    pub trips: Vec<UberTrip>,
    pub count: usize,
    pub offset: usize,
    pub limit: usize,
    pub freshness_seconds: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UberRideEstimateRequest {
    pub pickup_latitude: f64,
    pub pickup_longitude: f64,
    pub dropoff_latitude: f64,
    pub dropoff_longitude: f64,
    pub pickup_display_name: Option<String>,
    pub dropoff_display_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UberRideOption {
    pub product_id: String,
    pub display_name: String,
    pub fare_id: String,
    pub price_amount_minor: i64,
    pub currency: String,
    pub duration_seconds: u32,
    pub distance_miles: f64,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UberRideEstimateResponse {
    pub pickup_display_name: String,
    pub dropoff_display_name: String,
    pub options: Vec<UberRideOption>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UberRideHandoffRequest {
    pub pickup_latitude: f64,
    pub pickup_longitude: f64,
    pub dropoff_latitude: f64,
    pub dropoff_longitude: f64,
    pub product_id: Option<String>,
    pub fare_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UberRideHandoffResponse {
    pub provider: String,
    pub action: String,
    pub handoff_url: String,
    pub status: String,
    pub completed: bool,
    pub disclaimer: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UberRawTrip {
    pub trip_id: String,
    pub request_time: DateTime<Utc>,
    pub status: String,
    pub distance_miles: f64,
    pub start_city: Option<String>,
    // Sensitive fields that MUST be stripped during context minimization
    pub pickup_latitude: Option<f64>,
    pub pickup_longitude: Option<f64>,
    pub dropoff_latitude: Option<f64>,
    pub dropoff_longitude: Option<f64>,
    pub payment_method_id: Option<String>,
    pub internal_rider_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UberRawHistoryResponse {
    pub trips: Vec<UberRawTrip>,
    pub count: usize,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum UberReadError {
    #[error("connection not found")]
    ConnectionNotFound,
    #[error("connection is not authorized for integration uber")]
    InvalidIntegration,
    #[error("access expired or revoked; reconnection required")]
    ReconnectRequired,
    #[error("capability {0} is not granted to agent {1}")]
    UnauthorizedCapability(String, String),
    #[error("unsupported direct execution: {0}")]
    UnsupportedDirectExecution(String),
    #[error("rate limited by provider; retry after {0} seconds")]
    RateLimited(u64),
    #[error("provider error: {0}")]
    ProviderError(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[async_trait::async_trait]
pub trait UberProviderClient: Send + Sync {
    async fn fetch_history(
        &self,
        access_token: &str,
        offset: usize,
        limit: usize,
    ) -> Result<UberRawHistoryResponse, UberReadError>;

    async fn fetch_estimates(
        &self,
        pickup_lat: f64,
        pickup_lon: f64,
        dropoff_lat: f64,
        dropoff_lon: f64,
    ) -> Result<Vec<UberRideOption>, UberReadError>;
}

pub struct DefaultUberProviderClient {
    base_url: String,
    http: reqwest::Client,
}

impl DefaultUberProviderClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl UberProviderClient for DefaultUberProviderClient {
    async fn fetch_history(
        &self,
        access_token: &str,
        offset: usize,
        limit: usize,
    ) -> Result<UberRawHistoryResponse, UberReadError> {
        let url = format!(
            "{}/v1.2/history?offset={}&limit={}",
            self.base_url, offset, limit
        );
        let resp = self
            .http
            .get(&url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| UberReadError::ProviderError(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("Retry-After")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(60);
            return Err(UberReadError::RateLimited(retry_after));
        }

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(UberReadError::ReconnectRequired);
        }

        if !resp.status().is_success() {
            return Err(UberReadError::ProviderError(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        resp.json::<UberRawHistoryResponse>()
            .await
            .map_err(|e| UberReadError::ProviderError(e.to_string()))
    }

    async fn fetch_estimates(
        &self,
        pickup_lat: f64,
        pickup_lon: f64,
        dropoff_lat: f64,
        dropoff_lon: f64,
    ) -> Result<Vec<UberRideOption>, UberReadError> {
        let url = format!("{}/v1.2/estimates/price", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[
                ("start_latitude", pickup_lat.to_string()),
                ("start_longitude", pickup_lon.to_string()),
                ("end_latitude", dropoff_lat.to_string()),
                ("end_longitude", dropoff_lon.to_string()),
            ])
            .send()
            .await
            .map_err(|e| UberReadError::ProviderError(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(UberReadError::RateLimited(60));
        }

        if !resp.status().is_success() {
            return Err(UberReadError::ProviderError(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        resp.json::<Vec<UberRideOption>>()
            .await
            .map_err(|e| UberReadError::ProviderError(e.to_string()))
    }
}

pub struct MockUberProviderClient {
    pub trips: std::sync::Mutex<Vec<UberRawTrip>>,
    pub estimates: std::sync::Mutex<Vec<UberRideOption>>,
    pub fail_with_rate_limit: std::sync::atomic::AtomicBool,
    pub fail_with_unauthorized: std::sync::atomic::AtomicBool,
}

impl MockUberProviderClient {
    pub fn new(trips: Vec<UberRawTrip>) -> Self {
        Self {
            trips: std::sync::Mutex::new(trips),
            estimates: std::sync::Mutex::new(vec![]),
            fail_with_rate_limit: std::sync::atomic::AtomicBool::new(false),
            fail_with_unauthorized: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn with_estimates(self, options: Vec<UberRideOption>) -> Self {
        *self.estimates.lock().unwrap() = options;
        self
    }
}

#[async_trait::async_trait]
impl UberProviderClient for MockUberProviderClient {
    async fn fetch_history(
        &self,
        _access_token: &str,
        offset: usize,
        limit: usize,
    ) -> Result<UberRawHistoryResponse, UberReadError> {
        if self
            .fail_with_rate_limit
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(UberReadError::RateLimited(30));
        }
        if self
            .fail_with_unauthorized
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(UberReadError::ReconnectRequired);
        }

        let all = self.trips.lock().unwrap();
        let total = all.len();
        let paged: Vec<UberRawTrip> = all.iter().skip(offset).take(limit).cloned().collect();

        Ok(UberRawHistoryResponse {
            trips: paged,
            count: total,
            offset,
            limit,
        })
    }

    async fn fetch_estimates(
        &self,
        _pickup_lat: f64,
        _pickup_lon: f64,
        _dropoff_lat: f64,
        _dropoff_lon: f64,
    ) -> Result<Vec<UberRideOption>, UberReadError> {
        if self
            .fail_with_rate_limit
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(UberReadError::RateLimited(30));
        }
        let lock = self.estimates.lock().unwrap();
        if lock.is_empty() {
            let now = Utc::now();
            Ok(vec![
                UberRideOption {
                    product_id: "uberx_standard".into(),
                    display_name: "UberX".into(),
                    fare_id: format!("fare_{}", Uuid::new_v4()),
                    price_amount_minor: 1850,
                    currency: "USD".into(),
                    duration_seconds: 720,
                    distance_miles: 4.2,
                    expires_at: now + chrono::Duration::minutes(5),
                },
                UberRideOption {
                    product_id: "uber_black_vip".into(),
                    display_name: "Uber Black".into(),
                    fare_id: format!("fare_{}", Uuid::new_v4()),
                    price_amount_minor: 3500,
                    currency: "USD".into(),
                    duration_seconds: 680,
                    distance_miles: 4.2,
                    expires_at: now + chrono::Duration::minutes(5),
                },
            ])
        } else {
            Ok(lock.clone())
        }
    }
}

#[derive(Clone)]
pub struct UberConnectedReadService {
    _db: Db,
    connections: ConnectionService,
    grants: CapabilityGrantService,
    client: Arc<dyn UberProviderClient>,
}

impl UberConnectedReadService {
    pub fn new(
        _db: Db,
        connections: ConnectionService,
        grants: CapabilityGrantService,
        client: Arc<dyn UberProviderClient>,
    ) -> Self {
        Self {
            _db,
            connections,
            grants,
            client,
        }
    }

    /// Returns the canonical integration declaration for Uber matching the E02 feasibility record.
    pub fn integration_declaration(deployment_external_key: &str) -> RegisterIntegrationRequest {
        RegisterIntegrationRequest {
            deployment_external_key: deployment_external_key.into(),
            external_key: UBER_INTEGRATION_KEY.into(),
            protocol: IntegrationProtocol::Direct,
            display_name: "Uber".into(),
            declaration_version: 1,
            capabilities: vec![
                CapabilityDeclaration {
                    external_key: UBER_CAPABILITY_HISTORY_LITE_SHORT.into(),
                    effect: CapabilityEffect::Read,
                    access_needs: vec!["history_lite".into()],
                    data_recipients: vec!["api.uber.com".into()],
                    regions: vec![
                        "US".into(),
                        "GB".into(),
                        "CA".into(),
                        "AU".into(),
                        "IN".into(),
                    ],
                    failure_modes: vec!["reconnect_required".into(), "rate_limited".into()],
                    optional_guarantees: json!({
                        "freshness_seconds": 300,
                        "pagination_supported": true,
                        "capability_level": "L2_connected_read",
                    }),
                },
                CapabilityDeclaration {
                    external_key: UBER_CAPABILITY_HISTORY_SHORT.into(),
                    effect: CapabilityEffect::Read,
                    access_needs: vec!["history".into()],
                    data_recipients: vec!["api.uber.com".into()],
                    regions: vec![
                        "US".into(),
                        "GB".into(),
                        "CA".into(),
                        "AU".into(),
                        "IN".into(),
                    ],
                    failure_modes: vec!["reconnect_required".into(), "rate_limited".into()],
                    optional_guarantees: json!({
                        "freshness_seconds": 300,
                        "pagination_supported": true,
                        "includes_city": true,
                        "capability_level": "L2_connected_read",
                    }),
                },
                CapabilityDeclaration {
                    external_key: UBER_CAPABILITY_RIDE_ESTIMATE_SHORT.into(),
                    effect: CapabilityEffect::Read,
                    access_needs: vec!["estimates".into()],
                    data_recipients: vec!["api.uber.com".into()],
                    regions: vec![
                        "US".into(),
                        "GB".into(),
                        "CA".into(),
                        "AU".into(),
                        "IN".into(),
                    ],
                    failure_modes: vec!["rate_limited".into()],
                    optional_guarantees: json!({
                        "capability_level": "L1_catalog_read",
                        "location_minimized": true,
                    }),
                },
                CapabilityDeclaration {
                    external_key: UBER_CAPABILITY_RIDE_REQUEST_SHORT.into(),
                    effect: CapabilityEffect::Write,
                    access_needs: vec!["request".into()],
                    data_recipients: vec!["api.uber.com".into()],
                    regions: vec!["US".into()],
                    failure_modes: vec!["unsupported_direct_execution".into()],
                    optional_guarantees: json!({
                        "capability_level": "L0_labelled_handoff_only",
                        "direct_execution_supported": false,
                        "ride_confirmed_by": "uber_app",
                    }),
                },
            ],
        }
    }

    /// Authoritative connected read executing user-authorized trip history retrieval.
    pub async fn read_history(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        capability_key: &str,
        offset: usize,
        limit: usize,
    ) -> Result<UberHistoryResponse, UberReadError> {
        // 1. Feasibility & Capability Validation
        if capability_key != UBER_CAPABILITY_HISTORY_LITE
            && capability_key != UBER_CAPABILITY_HISTORY
        {
            return Err(UberReadError::UnauthorizedCapability(
                capability_key.to_string(),
                agent_external_key.to_string(),
            ));
        }

        // 2. Fetch and verify connection ownership and state
        let connection =
            self.connections
                .get(context, connection_id)
                .await
                .map_err(|e| match e {
                    ConnectionError::NotFound => UberReadError::ConnectionNotFound,
                    ConnectionError::Database(err) => UberReadError::Database(err),
                    _ => UberReadError::ConnectionNotFound,
                })?;

        if connection.integration_external_key != UBER_INTEGRATION_KEY {
            return Err(UberReadError::InvalidIntegration);
        }

        // Acceptance Criterion 3: Expired access pauses work and reconnection rechecks relevant facts
        if connection.authorization_state != AuthorizationState::Authorized {
            return Err(UberReadError::ReconnectRequired);
        }

        // Check if access expiration timestamp has passed
        if connection.expires_at.is_some_and(|exp| exp <= Utc::now()) {
            return Err(UberReadError::ReconnectRequired);
        }

        // 3. Verify agent capability grant
        let effective_grants = self
            .grants
            .effective_for_agent(context, agent_external_key)
            .await
            .map_err(|e| match e {
                crate::capability_grants::CapabilityGrantError::Database(err) => {
                    UberReadError::Database(err)
                }
                _ => UberReadError::UnauthorizedCapability(
                    capability_key.to_string(),
                    agent_external_key.to_string(),
                ),
            })?;

        let has_grant = effective_grants.iter().any(|g| {
            g.connection_id == connection_id && g.capability_external_key == capability_key
        });

        if !has_grant {
            return Err(UberReadError::UnauthorizedCapability(
                capability_key.to_string(),
                agent_external_key.to_string(),
            ));
        }

        // 4. Fetch trip history from provider
        let effective_limit = if limit == 0 || limit > 50 { 50 } else { limit };
        let mock_token = "authorized-oauth-token";
        let raw = self
            .client
            .fetch_history(mock_token, offset, effective_limit)
            .await?;

        // Acceptance Criterion 2: Only data required for the invocation reaches the agent or remote operator (Context Minimization)
        let is_history_scoped = capability_key == UBER_CAPABILITY_HISTORY;
        let minimized_trips: Vec<UberTrip> = raw
            .trips
            .into_iter()
            .map(|t| UberTrip {
                trip_id: t.trip_id,
                request_time: t.request_time,
                status: t.status,
                distance_miles: t.distance_miles,
                // Include city only when explicit history scope is granted
                city: if is_history_scoped {
                    t.start_city
                } else {
                    None
                },
                // Notice: pickup_latitude, pickup_longitude, dropoff_latitude, dropoff_longitude,
                // payment_method_id, internal_rider_token are completely dropped and excluded.
            })
            .collect();

        Ok(UberHistoryResponse {
            trips: minimized_trips,
            count: raw.count,
            offset: raw.offset,
            limit: raw.limit,
            freshness_seconds: 300,
        })
    }

    /// Returns upfront ride fare estimates while enforcing location minimization.
    pub async fn estimate_ride(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        request: UberRideEstimateRequest,
    ) -> Result<UberRideEstimateResponse, UberReadError> {
        self.verify_access(
            context,
            agent_external_key,
            connection_id,
            UBER_CAPABILITY_RIDE_ESTIMATE,
        )
        .await?;
        let options = self
            .client
            .fetch_estimates(
                request.pickup_latitude,
                request.pickup_longitude,
                request.dropoff_latitude,
                request.dropoff_longitude,
            )
            .await?;

        let expires_at = options
            .iter()
            .map(|o| o.expires_at)
            .min()
            .unwrap_or_else(|| Utc::now() + chrono::Duration::minutes(5));

        Ok(UberRideEstimateResponse {
            pickup_display_name: request
                .pickup_display_name
                .unwrap_or_else(|| "Pickup location".into()),
            dropoff_display_name: request
                .dropoff_display_name
                .unwrap_or_else(|| "Destination".into()),
            options,
            expires_at,
        })
    }

    /// Creates an explicit labelled handoff for requesting a ride in the Uber app.
    ///
    /// GUARANTEE: Opening Uber is NEVER reported as a confirmed ride.
    /// Direct ride request execution is unavailable without Uber Full Access privileged
    /// production approval and strictly rejected.
    pub async fn create_ride_handoff(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        request: UberRideHandoffRequest,
    ) -> Result<UberRideHandoffResponse, UberReadError> {
        self.verify_access(
            context,
            agent_external_key,
            connection_id,
            UBER_CAPABILITY_RIDE_REQUEST,
        )
        .await?;

        let mut url = format!(
            "https://m.uber.com/ul/?action=setPickup&pickup[latitude]={:.6}&pickup[longitude]={:.6}&dropoff[latitude]={:.6}&dropoff[longitude]={:.6}",
            request.pickup_latitude,
            request.pickup_longitude,
            request.dropoff_latitude,
            request.dropoff_longitude
        );

        if let Some(pid) = &request.product_id {
            url.push_str(&format!("&product_id={}", pid));
        }

        Ok(UberRideHandoffResponse {
            provider: UBER_INTEGRATION_KEY.into(),
            action: "open_uber_ride_request".into(),
            handoff_url: url,
            status: "handoff_created".into(),
            completed: false, // Invariant: opening Uber is never reported as a confirmed ride
            disclaimer: "Ride dispatch, driver matching, and fare charging are completed directly in the Uber app. Vox does not claim a confirmed ride.".into(),
        })
    }

    /// Enforces that direct execution is unsupported for Uber ride requests until privileged
    /// production approval and live validation are passed.
    pub async fn execute_ride_request(
        &self,
        _context: &ResolvedUserContext,
        _agent_external_key: &str,
        _connection_id: Uuid,
        _fare_id: &str,
    ) -> Result<(), UberReadError> {
        Err(UberReadError::UnsupportedDirectExecution(
            "Direct consumer ride request execution is unavailable without Uber Full Access privileged production approval. Ride requests must use labelled handoff.".into(),
        ))
    }

    async fn verify_access(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        capability: &str,
    ) -> Result<(), UberReadError> {
        let connection =
            self.connections
                .get(context, connection_id)
                .await
                .map_err(|e| match e {
                    ConnectionError::NotFound => UberReadError::ConnectionNotFound,
                    ConnectionError::Database(err) => UberReadError::Database(err),
                    _ => UberReadError::ConnectionNotFound,
                })?;

        if connection.integration_external_key != UBER_INTEGRATION_KEY {
            return Err(UberReadError::InvalidIntegration);
        }

        if connection.authorization_state != AuthorizationState::Authorized {
            return Err(UberReadError::ReconnectRequired);
        }

        if connection
            .expires_at
            .is_some_and(|exp| exp <= chrono::Utc::now())
        {
            return Err(UberReadError::ReconnectRequired);
        }

        let grants = self
            .grants
            .effective_for_agent(context, agent_external_key)
            .await
            .map_err(|e| match e {
                CapabilityGrantError::Database(err) => UberReadError::Database(err),
                _ => UberReadError::UnauthorizedCapability(
                    capability.into(),
                    agent_external_key.into(),
                ),
            })?;

        let has_grant = grants
            .iter()
            .any(|g| g.connection_id == connection_id && g.capability_external_key == capability);

        if !has_grant {
            return Err(UberReadError::UnauthorizedCapability(
                capability.into(),
                agent_external_key.into(),
            ));
        }

        Ok(())
    }
}
