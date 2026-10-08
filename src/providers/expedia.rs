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
    integration_registry::RegisterIntegrationRequest,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

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

pub use vox_connections::providers::expedia::{
    DefaultExpediaProviderClient, EXPEDIA_CAPABILITY_LODGING_BOOK,
    EXPEDIA_CAPABILITY_LODGING_BOOK_SHORT, EXPEDIA_CAPABILITY_LODGING_MANAGE,
    EXPEDIA_CAPABILITY_LODGING_MANAGE_SHORT, EXPEDIA_CAPABILITY_LODGING_SEARCH,
    EXPEDIA_CAPABILITY_LODGING_SEARCH_SHORT, EXPEDIA_INTEGRATION_KEY, ExpediaBookingOutcome,
    ExpediaCancellationResult, ExpediaLodgingError, ExpediaProviderClient,
    ExpediaRawBookingRequest, ExpediaRawBookingResponse, MockExpediaProviderClient,
};

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

    /// Canonical provider declaration from the shared connector package.
    pub fn integration_declaration(deployment_external_key: &str) -> RegisterIntegrationRequest {
        vox_connections::providers::expedia::integration_declaration(deployment_external_key)
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
        let connection = self
            .connections
            .get(&context.request_context(), connection_id)
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
            .effective_for_agent(&context.request_context(), agent_external_key)
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
            .effective_for_agent(&context.request_context(), agent_external_key)
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
             WHERE e.user_id = $1 AND p.user_id = $1 AND e.user_context_id = $5 AND p.user_context_id = $5 AND p.connection_id = $2 \
             AND p.capability = $3 \
             AND e.state = 'succeeded' AND e.provider_reference = $4)",
        )
        .bind(context.user_id.0)
        .bind(connection_id)
        .bind(EXPEDIA_CAPABILITY_LODGING_BOOK)
        .bind(itinerary_id)
        .bind(context.id.0)
        .fetch_one(self.db.pool())
        .await?;
        if !owns_booking {
            return Err(ExpediaLodgingError::ProposalNotFound);
        }

        // The legacy signature cannot bind a disclosed cancellation penalty to
        // an exact, single-use approval. Fail closed until that contract exists.
        let _ = reason;
        Err(ExpediaLodgingError::NotApproved)
    }

    /// Authoritatively reconciles an uncertain execution outcome using affiliate_reference_id.
    pub async fn reconcile_booking(
        &self,
        context: &ResolvedUserContext,
        affiliate_reference_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ExpediaBookingOutcome, ExpediaLodgingError> {
        // Locate execution in database
        let row_opt = sqlx::query(
            "SELECT id, state FROM executions WHERE idempotency_key = $1 AND user_id = $2 AND user_context_id = $3",
        )
        .bind(affiliate_reference_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?;

        let Some(row) = row_opt else {
            return Err(ExpediaLodgingError::ExecutionFailed(
                "no execution record found for reference".into(),
            ));
        };

        let execution_id: Uuid = row.get("id");

        let opt_resp = self.client.retrieve_booking(affiliate_reference_id).await?;
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
