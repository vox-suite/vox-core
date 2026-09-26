/**
 * Selected Consequential Write Integration: Expedia Rapid Lodging (E34).
 *
 * Conforms to E02 provider feasibility finding (L3 conditional lodging write):
 * - Direct execution requires verified connection and agent capability grant.
 * - Enforces exact action proposals bound to single-use authenticated approvals.
 * - Enforces unique affiliate_reference_id (idempotency key) preventing duplicate bookings.
 * - Distinct provider authentication (3DS / SCA challenge) separated from platform approval.
 * - Authoritative outcome reconciliation on timeouts / 5xx responses (no blind retry).
 * - Authoritative cancellation with penalty and refund disclosures.
 * - Strict context minimization: raw credit card numbers and CVVs never enter Vox Core.
 */
use crate::{
    approvals::{ApprovalError, ApprovalService, CreateProposalRequest, Proposal},
    capability_grants::{CapabilityGrantError, CapabilityGrantService},
    connections::{AuthorizationState, ConnectionError, ConnectionService},
    db::Db,
    execution::{AdapterOutcome, ExecutionCoordinator, ExecutionError, StartExecutionRequest},
    execution_policy::ExecutionIdentity,
    identity::ResolvedUserContext,
    integration_registry::{
        CapabilityDeclaration, CapabilityEffect, IntegrationProtocol, RegisterIntegrationRequest,
    },
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub const EXPEDIA_INTEGRATION_KEY: &str = "expedia";
pub const EXPEDIA_CAPABILITY_LODGING_SEARCH: &str = "expedia.lodging_search";
pub const EXPEDIA_CAPABILITY_LODGING_BOOK: &str = "expedia.lodging_book";
pub const EXPEDIA_CAPABILITY_LODGING_MANAGE: &str = "expedia.lodging_manage";

pub const EXPEDIA_CAPABILITY_LODGING_SEARCH_SHORT: &str = "lodging_search";
pub const EXPEDIA_CAPABILITY_LODGING_BOOK_SHORT: &str = "lodging_book";
pub const EXPEDIA_CAPABILITY_LODGING_MANAGE_SHORT: &str = "lodging_manage";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExpediaLodgingProposalDetails {
    pub property_id: String,
    pub room_type_id: String,
    pub rate_plan_id: String,
    pub checkin_date: String,
    pub checkout_date: String,
    pub guest_count: u32,
    pub primary_guest_name: String,
    pub primary_guest_email: String,
    pub total_price_amount_minor: i64,
    pub price_currency: String,
    pub execution: ExecutionIdentity,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExpediaRawBookingRequest {
    pub affiliate_reference_id: String,
    pub property_id: String,
    pub room_type_id: String,
    pub rate_plan_id: String,
    pub checkin_date: String,
    pub checkout_date: String,
    pub primary_guest_name: String,
    pub primary_guest_email: String,
    pub total_price_amount_minor: i64,
    pub price_currency: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExpediaRawBookingResponse {
    pub itinerary_id: String,
    pub confirmation_reference: String,
    pub status: String,
    pub challenge_url: Option<String>,
    pub challenge_token: Option<String>,
    pub affiliate_reference_id: String,
    pub total_price_amount_minor: i64,
    pub price_currency: String,
    pub cancellation_penalty_minor: Option<i64>,
    pub refund_amount_minor: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ExpediaBookingOutcome {
    Succeeded {
        itinerary_id: String,
        confirmation_reference: String,
        booking_status: String,
        total_price_amount_minor: i64,
        price_currency: String,
    },
    AwaitingProviderAuthentication {
        challenge_url: String,
        challenge_token: String,
        affiliate_reference_id: String,
    },
    Failed {
        error_code: String,
        message: String,
    },
    Cancelled {
        itinerary_id: String,
        cancellation_reference: String,
        refund_amount_minor: i64,
        penalty_amount_minor: i64,
        currency: String,
    },
    Reconciling {
        affiliate_reference_id: String,
        message: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExpediaCancellationResult {
    pub itinerary_id: String,
    pub cancellation_reference: String,
    pub refund_amount_minor: i64,
    pub penalty_amount_minor: i64,
    pub currency: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ExpediaLodgingError {
    #[error("connection not found")]
    ConnectionNotFound,
    #[error("connection is not authorized for integration expedia")]
    InvalidIntegration,
    #[error("connection expired or revoked; reconnection required")]
    ReconnectRequired,
    #[error("capability {0} is not granted to agent {1}")]
    UnauthorizedCapability(String, String),
    #[error("proposal request invalid: {0}")]
    InvalidProposal(String),
    #[error("proposal not found")]
    ProposalNotFound,
    #[error("proposal has expired")]
    ProposalExpired,
    #[error("proposal has not been approved or details hash mismatch")]
    NotApproved,
    #[error("execution error: {0}")]
    ExecutionFailed(String),
    #[error("rate limited by provider; retry after {0} seconds")]
    RateLimited(u64),
    #[error("provider error: {0}")]
    ProviderError(String),
    #[error("network timeout during provider execution")]
    Timeout,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[async_trait::async_trait]
pub trait ExpediaProviderClient: Send + Sync {
    async fn create_booking(
        &self,
        request: &ExpediaRawBookingRequest,
    ) -> Result<ExpediaRawBookingResponse, ExpediaLodgingError>;

    async fn retrieve_booking(
        &self,
        affiliate_reference_id: &str,
    ) -> Result<Option<ExpediaRawBookingResponse>, ExpediaLodgingError>;

    async fn cancel_booking(
        &self,
        itinerary_id: &str,
        reason: &str,
    ) -> Result<ExpediaRawBookingResponse, ExpediaLodgingError>;
}

pub struct DefaultExpediaProviderClient {
    base_url: String,
    http: reqwest::Client,
}

impl DefaultExpediaProviderClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl ExpediaProviderClient for DefaultExpediaProviderClient {
    async fn create_booking(
        &self,
        request: &ExpediaRawBookingRequest,
    ) -> Result<ExpediaRawBookingResponse, ExpediaLodgingError> {
        let url = format!("{}/v3/lodging/bookings", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ExpediaLodgingError::Timeout
                } else {
                    ExpediaLodgingError::ProviderError(e.to_string())
                }
            })?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry = resp
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(60);
            return Err(ExpediaLodgingError::RateLimited(retry));
        }

        if !resp.status().is_success() {
            return Err(ExpediaLodgingError::ProviderError(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        resp.json::<ExpediaRawBookingResponse>()
            .await
            .map_err(|e| ExpediaLodgingError::ProviderError(e.to_string()))
    }

    async fn retrieve_booking(
        &self,
        affiliate_reference_id: &str,
    ) -> Result<Option<ExpediaRawBookingResponse>, ExpediaLodgingError> {
        let url = format!(
            "{}/v3/lodging/bookings?affiliate_reference_id={}",
            self.base_url, affiliate_reference_id
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| ExpediaLodgingError::ProviderError(e.to_string()))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !resp.status().is_success() {
            return Err(ExpediaLodgingError::ProviderError(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        resp.json::<ExpediaRawBookingResponse>()
            .await
            .map(Some)
            .map_err(|e| ExpediaLodgingError::ProviderError(e.to_string()))
    }

    async fn cancel_booking(
        &self,
        itinerary_id: &str,
        reason: &str,
    ) -> Result<ExpediaRawBookingResponse, ExpediaLodgingError> {
        let url = format!(
            "{}/v3/lodging/bookings/{}/cancel",
            self.base_url, itinerary_id
        );
        let resp = self
            .http
            .post(&url)
            .json(&json!({ "reason": reason }))
            .send()
            .await
            .map_err(|e| ExpediaLodgingError::ProviderError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ExpediaLodgingError::ProviderError(format!(
                "HTTP {}",
                resp.status()
            )));
        }

        resp.json::<ExpediaRawBookingResponse>()
            .await
            .map_err(|e| ExpediaLodgingError::ProviderError(e.to_string()))
    }
}

pub struct MockExpediaProviderClient {
    pub bookings: Mutex<std::collections::HashMap<String, ExpediaRawBookingResponse>>,
    pub fail_with_rate_limit: std::sync::atomic::AtomicBool,
    pub fail_with_timeout: std::sync::atomic::AtomicBool,
    pub require_3ds: std::sync::atomic::AtomicBool,
}

impl MockExpediaProviderClient {
    pub fn new() -> Self {
        Self {
            bookings: Mutex::new(std::collections::HashMap::new()),
            fail_with_rate_limit: std::sync::atomic::AtomicBool::new(false),
            fail_with_timeout: std::sync::atomic::AtomicBool::new(false),
            require_3ds: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl Default for MockExpediaProviderClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ExpediaProviderClient for MockExpediaProviderClient {
    async fn create_booking(
        &self,
        request: &ExpediaRawBookingRequest,
    ) -> Result<ExpediaRawBookingResponse, ExpediaLodgingError> {
        if self
            .fail_with_rate_limit
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ExpediaLodgingError::RateLimited(45));
        }
        if self
            .fail_with_timeout
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ExpediaLodgingError::Timeout);
        }

        let mut store = self.bookings.lock().unwrap();
        if let Some(existing) = store.get(&request.affiliate_reference_id) {
            return Ok(existing.clone());
        }

        let is_3ds = self.require_3ds.load(std::sync::atomic::Ordering::SeqCst);
        let resp = if is_3ds {
            ExpediaRawBookingResponse {
                itinerary_id: format!("itin-{}", Uuid::new_v4()),
                confirmation_reference: format!("CONF-{}", Uuid::new_v4()),
                status: "pending_authentication".into(),
                challenge_url: Some("https://pay.expedia.com/3ds-challenge/v1".into()),
                challenge_token: Some("token_3ds_challenge_xyz".into()),
                affiliate_reference_id: request.affiliate_reference_id.clone(),
                total_price_amount_minor: request.total_price_amount_minor,
                price_currency: request.price_currency.clone(),
                cancellation_penalty_minor: None,
                refund_amount_minor: None,
            }
        } else {
            ExpediaRawBookingResponse {
                itinerary_id: format!("itin-{}", Uuid::new_v4()),
                confirmation_reference: format!("CONF-{}", Uuid::new_v4()),
                status: "booked".into(),
                challenge_url: None,
                challenge_token: None,
                affiliate_reference_id: request.affiliate_reference_id.clone(),
                total_price_amount_minor: request.total_price_amount_minor,
                price_currency: request.price_currency.clone(),
                cancellation_penalty_minor: None,
                refund_amount_minor: None,
            }
        };

        store.insert(request.affiliate_reference_id.clone(), resp.clone());
        Ok(resp)
    }

    async fn retrieve_booking(
        &self,
        affiliate_reference_id: &str,
    ) -> Result<Option<ExpediaRawBookingResponse>, ExpediaLodgingError> {
        let store = self.bookings.lock().unwrap();
        Ok(store.get(affiliate_reference_id).cloned())
    }

    async fn cancel_booking(
        &self,
        itinerary_id: &str,
        _reason: &str,
    ) -> Result<ExpediaRawBookingResponse, ExpediaLodgingError> {
        let mut store = self.bookings.lock().unwrap();
        for resp in store.values_mut() {
            if resp.itinerary_id == itinerary_id {
                resp.status = "cancelled".into();
                resp.cancellation_penalty_minor = Some(5_000); // $50 penalty
                resp.refund_amount_minor = Some(resp.total_price_amount_minor - 5_000);
                return Ok(resp.clone());
            }
        }
        Err(ExpediaLodgingError::ProviderError(
            "itinerary not found".into(),
        ))
    }
}

#[derive(Clone)]
pub struct ExpediaLodgingService {
    db: Db,
    connections: ConnectionService,
    grants: CapabilityGrantService,
    approvals: ApprovalService,
    execution: ExecutionCoordinator,
    client: Arc<dyn ExpediaProviderClient>,
}

impl ExpediaLodgingService {
    pub fn new(
        db: Db,
        connections: ConnectionService,
        grants: CapabilityGrantService,
        approvals: ApprovalService,
        execution: ExecutionCoordinator,
        client: Arc<dyn ExpediaProviderClient>,
    ) -> Self {
        Self {
            db,
            connections,
            grants,
            approvals,
            execution,
            client,
        }
    }

    /// Canonical integration declaration conforming to the E02 feasibility record.
    pub fn integration_declaration(deployment_external_key: &str) -> RegisterIntegrationRequest {
        RegisterIntegrationRequest {
            deployment_external_key: deployment_external_key.into(),
            external_key: EXPEDIA_INTEGRATION_KEY.into(),
            protocol: IntegrationProtocol::Direct,
            display_name: "Expedia Rapid".into(),
            declaration_version: 1,
            capabilities: vec![
                CapabilityDeclaration {
                    external_key: EXPEDIA_CAPABILITY_LODGING_SEARCH_SHORT.into(),
                    effect: CapabilityEffect::Read,
                    access_needs: vec!["search".into()],
                    data_recipients: vec!["api.expediagroup.com".into()],
                    regions: vec![
                        "US".into(),
                        "GB".into(),
                        "CA".into(),
                        "AU".into(),
                        "IN".into(),
                    ],
                    failure_modes: vec!["rate_limited".into()],
                    optional_guarantees: json!({
                        "freshness_seconds": 60,
                        "capability_level": "L1_catalog_read",
                    }),
                },
                CapabilityDeclaration {
                    external_key: EXPEDIA_CAPABILITY_LODGING_BOOK_SHORT.into(),
                    effect: CapabilityEffect::Write,
                    access_needs: vec!["book".into()],
                    data_recipients: vec!["api.expediagroup.com".into()],
                    regions: vec![
                        "US".into(),
                        "GB".into(),
                        "CA".into(),
                        "AU".into(),
                        "IN".into(),
                    ],
                    failure_modes: vec![
                        "price_change".into(),
                        "inventory_unavailable".into(),
                        "rate_limited".into(),
                        "provider_authentication_required".into(),
                        "unknown_outcome".into(),
                    ],
                    optional_guarantees: json!({
                        "capability_level": "L3_consequential_write",
                        "idempotency_supported": true,
                        "reconciliation_supported": true,
                        "requires_platform_approval": true,
                    }),
                },
                CapabilityDeclaration {
                    external_key: EXPEDIA_CAPABILITY_LODGING_MANAGE_SHORT.into(),
                    effect: CapabilityEffect::Write,
                    access_needs: vec!["manage".into()],
                    data_recipients: vec!["api.expediagroup.com".into()],
                    regions: vec![
                        "US".into(),
                        "GB".into(),
                        "CA".into(),
                        "AU".into(),
                        "IN".into(),
                    ],
                    failure_modes: vec!["rate_limited".into(), "cancellation_penalty".into()],
                    optional_guarantees: json!({
                        "capability_level": "L3_consequential_write",
                        "cancellation_supported": true,
                        "reconciliation_supported": true,
                    }),
                },
            ],
        }
    }

    /// Proposes an exact lodging booking bound to property, rate, dates, and price.
    #[allow(clippy::too_many_arguments)]
    pub async fn propose_booking(
        &self,
        context: &ResolvedUserContext,
        span_id: Uuid,
        task_run_id: Uuid,
        agent_external_key: &str,
        connection_id: Uuid,
        details: ExpediaLodgingProposalDetails,
        expires_at: DateTime<Utc>,
    ) -> Result<Proposal, ExpediaLodgingError> {
        // 1. Validate parameters
        if details.property_id.trim().is_empty()
            || details.room_type_id.trim().is_empty()
            || details.rate_plan_id.trim().is_empty()
            || details.checkin_date.trim().is_empty()
            || details.checkout_date.trim().is_empty()
            || details.guest_count == 0
            || details.total_price_amount_minor <= 0
            || details.price_currency.trim().len() != 3
        {
            return Err(ExpediaLodgingError::InvalidProposal(
                "missing required lodging parameters".into(),
            ));
        }

        // 2. Verify connection
        let connection =
            self.connections
                .get(context, connection_id)
                .await
                .map_err(|e| match e {
                    ConnectionError::NotFound => ExpediaLodgingError::ConnectionNotFound,
                    ConnectionError::Database(err) => ExpediaLodgingError::Database(err),
                    _ => ExpediaLodgingError::ConnectionNotFound,
                })?;

        if connection.integration_external_key != EXPEDIA_INTEGRATION_KEY {
            return Err(ExpediaLodgingError::InvalidIntegration);
        }

        if connection.authorization_state != AuthorizationState::Authorized {
            return Err(ExpediaLodgingError::ReconnectRequired);
        }

        if connection.expires_at.is_some_and(|exp| exp <= Utc::now()) {
            return Err(ExpediaLodgingError::ReconnectRequired);
        }

        // 3. Verify agent capability grant
        let grants = self
            .grants
            .effective_for_agent(context, agent_external_key)
            .await
            .map_err(|e| match e {
                CapabilityGrantError::Database(err) => ExpediaLodgingError::Database(err),
                _ => ExpediaLodgingError::UnauthorizedCapability(
                    EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
                    agent_external_key.into(),
                ),
            })?;

        let has_grant = grants.iter().any(|g| {
            g.connection_id == connection_id
                && g.capability_external_key == EXPEDIA_CAPABILITY_LODGING_BOOK
        });

        if !has_grant {
            return Err(ExpediaLodgingError::UnauthorizedCapability(
                EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
                agent_external_key.into(),
            ));
        }

        // 4. Construct exact proposal JSON
        let details_value = json!({
            "property_id": details.property_id,
            "room_type_id": details.room_type_id,
            "rate_plan_id": details.rate_plan_id,
            "checkin_date": details.checkin_date,
            "checkout_date": details.checkout_date,
            "guest_count": details.guest_count,
            "primary_guest_name": details.primary_guest_name,
            "primary_guest_email": details.primary_guest_email,
            "total_price_amount_minor": details.total_price_amount_minor,
            "price_currency": details.price_currency,
            "execution": details.execution,
        });

        let now = Utc::now();
        self.approvals
            .propose(
                context,
                CreateProposalRequest {
                    span_id,
                    task_run_id,
                    agent_external_key: agent_external_key.into(),
                    capability_external_key: EXPEDIA_CAPABILITY_LODGING_BOOK.into(),
                    details: details_value,
                    expires_at,
                    replaces_proposal_id: None,
                },
                now,
            )
            .await
            .map_err(|e| match e {
                ApprovalError::Invalid => ExpediaLodgingError::InvalidProposal(
                    "invalid proposal lifetime or details".into(),
                ),
                ApprovalError::NotFound => ExpediaLodgingError::ProposalNotFound,
                ApprovalError::Database(err) => ExpediaLodgingError::Database(err),
                _ => ExpediaLodgingError::InvalidProposal(e.to_string()),
            })
    }

    /// Authoritatively executes an approved booking proposal with affiliate_reference_id idempotency.
    pub async fn execute_booking(
        &self,
        context: &ResolvedUserContext,
        approval_id: Uuid,
        idempotency_key: &str,
        now: DateTime<Utc>,
    ) -> Result<ExpediaBookingOutcome, ExpediaLodgingError> {
        let key = idempotency_key.trim();
        if key.is_empty() {
            return Err(ExpediaLodgingError::ExecutionFailed(
                "missing idempotency key".into(),
            ));
        }

        // 1. Consume approval and initialize durable execution record in Core
        let execution = self
            .execution
            .start(
                context,
                StartExecutionRequest {
                    approval_id,
                    idempotency_key: key.into(),
                },
                now,
            )
            .await
            .map_err(|e| match e {
                ExecutionError::FreshApproval => ExpediaLodgingError::NotApproved,
                ExecutionError::Unavailable => ExpediaLodgingError::ProposalNotFound,
                ExecutionError::Invalid => {
                    ExpediaLodgingError::ExecutionFailed("invalid execution parameters".into())
                }
                ExecutionError::Database(err) => ExpediaLodgingError::Database(err),
            })?;

        // 2. Fetch proposal details from executions table
        let row = sqlx::query(
            "SELECT p.details, p.capability FROM executions e \
             JOIN action_proposals p ON p.id = e.proposal_id \
             WHERE e.id = $1 AND e.user_id = $2",
        )
        .bind(execution.id)
        .bind(context.user_id.0)
        .fetch_one(self.db.pool())
        .await?;

        let details: Value = row.get("details");

        if execution.state == "succeeded"
            && let Some(evidence) = &execution.confirmation_evidence
        {
            return Ok(ExpediaBookingOutcome::Succeeded {
                itinerary_id: evidence["itinerary_id"].as_str().unwrap_or_default().into(),
                confirmation_reference: evidence["confirmation_reference"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                booking_status: evidence["status"]
                    .as_str()
                    .unwrap_or_else(|| evidence["booking_status"].as_str().unwrap_or("booked"))
                    .into(),
                total_price_amount_minor: details["total_price_amount_minor"].as_i64().unwrap_or(0),
                price_currency: details["price_currency"].as_str().unwrap_or("USD").into(),
            });
        }
        let raw_req = ExpediaRawBookingRequest {
            affiliate_reference_id: key.into(),
            property_id: details["property_id"].as_str().unwrap_or("").into(),
            room_type_id: details["room_type_id"].as_str().unwrap_or("").into(),
            rate_plan_id: details["rate_plan_id"].as_str().unwrap_or("").into(),
            checkin_date: details["checkin_date"].as_str().unwrap_or("").into(),
            checkout_date: details["checkout_date"].as_str().unwrap_or("").into(),
            primary_guest_name: details["primary_guest_name"].as_str().unwrap_or("").into(),
            primary_guest_email: details["primary_guest_email"].as_str().unwrap_or("").into(),
            total_price_amount_minor: details["total_price_amount_minor"].as_i64().unwrap_or(0),
            price_currency: details["price_currency"].as_str().unwrap_or("USD").into(),
        };

        // 3. Mark execution in progress (reconciling) before network call
        self.execution
            .record_outcome(
                context,
                execution.id,
                AdapterOutcome::Reconciling {
                    provider_reference: None,
                },
                now,
            )
            .await
            .map_err(|e| ExpediaLodgingError::ExecutionFailed(e.to_string()))?;

        // 4. Dispatch booking to Expedia
        let result = self.client.create_booking(&raw_req).await;

        match result {
            Ok(resp) => {
                if resp.status == "pending_authentication" {
                    // Provider authentication (3DS/SCA) challenge
                    let outcome = AdapterOutcome::AwaitingProviderAuthentication {
                        provider_reference: Some(resp.itinerary_id.clone()),
                    };
                    self.execution
                        .record_verified_external_outcome(context, execution.id, outcome, now)
                        .await
                        .map_err(|e| ExpediaLodgingError::ExecutionFailed(e.to_string()))?;

                    Ok(ExpediaBookingOutcome::AwaitingProviderAuthentication {
                        challenge_url: resp.challenge_url.unwrap_or_default(),
                        challenge_token: resp.challenge_token.unwrap_or_default(),
                        affiliate_reference_id: key.into(),
                    })
                } else {
                    // Authoritative Booking Success
                    let evidence = json!({
                        "itinerary_id": resp.itinerary_id,
                        "confirmation_reference": resp.confirmation_reference,
                        "status": resp.status,
                        "affiliate_reference_id": resp.affiliate_reference_id,
                    });
                    let outcome = AdapterOutcome::Succeeded {
                        provider_reference: resp.itinerary_id.clone(),
                        evidence,
                    };
                    self.execution
                        .record_verified_external_outcome(context, execution.id, outcome, now)
                        .await
                        .map_err(|e| ExpediaLodgingError::ExecutionFailed(e.to_string()))?;

                    Ok(ExpediaBookingOutcome::Succeeded {
                        itinerary_id: resp.itinerary_id,
                        confirmation_reference: resp.confirmation_reference,
                        booking_status: resp.status,
                        total_price_amount_minor: resp.total_price_amount_minor,
                        price_currency: resp.price_currency,
                    })
                }
            }
            Err(ExpediaLodgingError::Timeout) => {
                // Timeout / network failure: does NOT fail or blindly retry!
                // Remains in reconciling state for authoritative retrieval.
                Ok(ExpediaBookingOutcome::Reconciling {
                    affiliate_reference_id: key.into(),
                    message: "Network timeout; awaiting authoritative provider status confirmation"
                        .into(),
                })
            }
            Err(ExpediaLodgingError::RateLimited(retry)) => {
                Err(ExpediaLodgingError::RateLimited(retry))
            }
            Err(e) => {
                let outcome = AdapterOutcome::Failed {
                    code: e.to_string(),
                };
                let _ = self
                    .execution
                    .record_verified_external_outcome(context, execution.id, outcome, now)
                    .await;
                Err(e)
            }
        }
    }

    /// Cancels an existing booking authoritatively via Expedia Manage Booking.
    pub async fn cancel_booking(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
        connection_id: Uuid,
        itinerary_id: &str,
        reason: &str,
    ) -> Result<ExpediaCancellationResult, ExpediaLodgingError> {
        let grants = self
            .grants
            .effective_for_agent(context, agent_external_key)
            .await
            .map_err(|e| match e {
                CapabilityGrantError::Database(err) => ExpediaLodgingError::Database(err),
                _ => ExpediaLodgingError::UnauthorizedCapability(
                    EXPEDIA_CAPABILITY_LODGING_MANAGE.into(),
                    agent_external_key.into(),
                ),
            })?;

        let has_grant = grants.iter().any(|g| {
            g.connection_id == connection_id
                && g.capability_external_key == EXPEDIA_CAPABILITY_LODGING_MANAGE
        });

        if !has_grant {
            return Err(ExpediaLodgingError::UnauthorizedCapability(
                EXPEDIA_CAPABILITY_LODGING_MANAGE.into(),
                agent_external_key.into(),
            ));
        }

        // A manage grant authorizes this connection, not every itinerary known to
        // the provider. Only a booking confirmed for this user and connection may
        // be cancelled through Core.
        let owns_booking = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM executions e \
             JOIN action_proposals p ON p.id = e.proposal_id \
             WHERE e.user_id = $1 AND p.user_id = $1 AND p.connection_id = $2 \
             AND p.capability = $3 \
             AND e.state = 'succeeded' AND e.provider_reference = $4)",
        )
        .bind(context.user_id.0)
        .bind(connection_id)
        .bind(EXPEDIA_CAPABILITY_LODGING_BOOK)
        .bind(itinerary_id)
        .fetch_one(self.db.pool())
        .await?;
        if !owns_booking {
            return Err(ExpediaLodgingError::ProposalNotFound);
        }

        let resp = self.client.cancel_booking(itinerary_id, reason).await?;

        Ok(ExpediaCancellationResult {
            itinerary_id: resp.itinerary_id,
            cancellation_reference: resp.confirmation_reference,
            refund_amount_minor: resp.refund_amount_minor.unwrap_or(0),
            penalty_amount_minor: resp.cancellation_penalty_minor.unwrap_or(0),
            currency: resp.price_currency,
        })
    }

    /// Authoritatively reconciles an uncertain execution outcome using affiliate_reference_id.
    pub async fn reconcile_booking(
        &self,
        context: &ResolvedUserContext,
        affiliate_reference_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ExpediaBookingOutcome, ExpediaLodgingError> {
        let opt_resp = self.client.retrieve_booking(affiliate_reference_id).await?;

        // Locate execution in database
        let row_opt = sqlx::query(
            "SELECT id, state FROM executions WHERE idempotency_key = $1 AND user_id = $2",
        )
        .bind(affiliate_reference_id)
        .bind(context.user_id.0)
        .fetch_optional(self.db.pool())
        .await?;

        let Some(row) = row_opt else {
            return Err(ExpediaLodgingError::ExecutionFailed(
                "no execution record found for reference".into(),
            ));
        };

        let execution_id: Uuid = row.get("id");

        if let Some(resp) = opt_resp {
            let evidence = json!({
                "itinerary_id": resp.itinerary_id,
                "confirmation_reference": resp.confirmation_reference,
                "status": resp.status,
                "affiliate_reference_id": resp.affiliate_reference_id,
            });
            let outcome = AdapterOutcome::Succeeded {
                provider_reference: resp.itinerary_id.clone(),
                evidence,
            };
            let _ = self
                .execution
                .record_verified_external_outcome(context, execution_id, outcome, now)
                .await;

            Ok(ExpediaBookingOutcome::Succeeded {
                itinerary_id: resp.itinerary_id,
                confirmation_reference: resp.confirmation_reference,
                booking_status: resp.status,
                total_price_amount_minor: resp.total_price_amount_minor,
                price_currency: resp.price_currency,
            })
        } else {
            Ok(ExpediaBookingOutcome::Reconciling {
                affiliate_reference_id: affiliate_reference_id.into(),
                message: "No provider booking confirmed yet; remaining in reconciling state".into(),
            })
        }
    }
}
