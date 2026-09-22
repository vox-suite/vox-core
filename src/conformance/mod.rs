/**
* Platform conformance testing and behavioral verification utilities.
*/
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub const FIXTURE_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConformanceSuite {
    pub fixture_version: u32,
    pub scenarios: Vec<Scenario>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Scenario {
    pub id: String,
    pub purpose: String,
    pub steps: Vec<ScenarioStep>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScenarioStep {
    pub command: Command,
    pub expect: Expectation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Expectation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ResultKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorCategory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OutcomeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_count: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    InstallExtension {
        extension_id: String,
        capabilities: Vec<String>,
    },
    SetExtensionEnabled {
        extension_id: String,
        enabled: bool,
    },
    DiscoverCapabilities {
        context_id: String,
    },
    GrantCapability {
        context_id: String,
        agent_id: String,
        capability: String,
    },
    RevokeCapability {
        context_id: String,
        agent_id: String,
        capability: String,
    },
    CreateProposal {
        proposal_id: String,
        context_id: String,
        agent_id: String,
        capability: String,
        input: Value,
        expires_at: u64,
    },
    ApproveProposal {
        proposal_id: String,
        context_id: String,
        input: Value,
        approved_at: u64,
    },
    ExecuteProposal {
        proposal_id: String,
        context_id: String,
        agent_id: String,
        input: Value,
        idempotency_key: String,
        executed_at: u64,
        provider_observation: ProviderObservation,
    },
    ReadOutcome {
        context_id: String,
        action_id: String,
    },
    ReadEvents {
        context_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProviderObservation {
    Succeeded { receipt: String },
    Failed { code: String },
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    ExtensionInstalled,
    ExtensionStateChanged,
    CapabilitiesDiscovered,
    CapabilityGranted,
    CapabilityRevoked,
    ProposalCreated,
    ProposalApproved,
    ProposalExecuted,
    OutcomeRead,
    EventsRead,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    IsolationViolation,
    CapabilityUnavailable,
    GrantRequired,
    ApprovalRequired,
    ApprovalMismatch,
    ApprovalExpired,
    ExtensionDisabled,
    Conflict,
    NotFound,
    InvalidRequest,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticError {
    pub category: ErrorCategory,
    pub message: String,
}

impl SemanticError {
    pub fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticResult {
    pub kind: ResultKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OutcomeStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<SemanticEvent>,
}

impl SemanticResult {
    pub fn new(kind: ResultKind) -> Self {
        Self {
            kind,
            action_id: None,
            outcome: None,
            capabilities: Vec::new(),
            events: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticEvent {
    pub context_id: String,
    pub kind: EventKind,
    pub subject_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    CapabilityGranted,
    CapabilityRevoked,
    ProposalCreated,
    ProposalApproved,
    ActionAttempted,
    OutcomeRecorded,
}

pub trait PlatformAdapter {
    fn apply(&mut self, command: Command) -> Result<SemanticResult, SemanticError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioFailure {
    pub scenario_id: String,
    pub step: usize,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunReport {
    pub fixture_version: u32,
    pub scenarios: usize,
    pub steps: usize,
    pub failures: Vec<ScenarioFailure>,
}

impl RunReport {
    pub fn is_conformant(&self) -> bool {
        self.failures.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("invalid conformance fixture: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("unsupported conformance fixture version {actual}; expected {expected}")]
    UnsupportedVersion { actual: u32, expected: u32 },
    #[error("scenario {scenario_id} step {step} must expect exactly one of result or error")]
    InvalidExpectation { scenario_id: String, step: usize },
}

pub fn parse_suite(input: &str) -> Result<ConformanceSuite, FixtureError> {
    let suite: ConformanceSuite = serde_json::from_str(input)?;
    if suite.fixture_version != FIXTURE_VERSION {
        return Err(FixtureError::UnsupportedVersion {
            actual: suite.fixture_version,
            expected: FIXTURE_VERSION,
        });
    }
    for scenario in &suite.scenarios {
        for (index, step) in scenario.steps.iter().enumerate() {
            if step.expect.result.is_some() == step.expect.error.is_some() {
                return Err(FixtureError::InvalidExpectation {
                    scenario_id: scenario.id.clone(),
                    step: index + 1,
                });
            }
        }
    }
    Ok(suite)
}

pub fn canonical_suite() -> ConformanceSuite {
    parse_suite(include_str!("fixtures/v1.json"))
        .expect("bundled conformance fixture must be valid")
}

pub fn run_suite<A, F>(suite: &ConformanceSuite, mut adapter_factory: F) -> RunReport
where
    A: PlatformAdapter,
    F: FnMut() -> A,
{
    let mut report = RunReport {
        fixture_version: suite.fixture_version,
        scenarios: suite.scenarios.len(),
        steps: 0,
        failures: Vec::new(),
    };
    for scenario in &suite.scenarios {
        let mut adapter = adapter_factory();
        for (index, step) in scenario.steps.iter().enumerate() {
            report.steps += 1;
            let actual = adapter.apply(step.command.clone());
            if let Err(reason) = matches_expectation(&step.expect, &actual) {
                report.failures.push(ScenarioFailure {
                    scenario_id: scenario.id.clone(),
                    step: index + 1,
                    reason,
                });
            }
        }
    }
    report
}

fn matches_expectation(
    expected: &Expectation,
    actual: &Result<SemanticResult, SemanticError>,
) -> Result<(), String> {
    match (expected.result, expected.error, actual) {
        (Some(kind), None, Ok(result)) if kind == result.kind => {
            if let Some(outcome) = expected.outcome
                && result.outcome != Some(outcome)
            {
                return Err(format!(
                    "expected outcome {outcome:?}, got {:?}",
                    result.outcome
                ));
            }
            if let Some(capabilities) = &expected.capabilities {
                let mut expected = capabilities.clone();
                expected.sort();
                let mut actual = result.capabilities.clone();
                actual.sort();
                if expected != actual {
                    return Err(format!(
                        "expected capabilities {expected:?}, got {actual:?}"
                    ));
                }
            }
            if let Some(event_count) = expected.event_count
                && result.events.len() != event_count
            {
                return Err(format!(
                    "expected {event_count} events, got {}",
                    result.events.len()
                ));
            }
            Ok(())
        }
        (Some(kind), None, Ok(result)) => {
            Err(format!("expected result {kind:?}, got {:?}", result.kind))
        }
        (Some(kind), None, Err(error)) => Err(format!(
            "expected result {kind:?}, got error {:?}",
            error.category
        )),
        (None, Some(category), Err(error)) if category == error.category => Ok(()),
        (None, Some(category), Err(error)) => Err(format!(
            "expected error {category:?}, got {:?}",
            error.category
        )),
        (None, Some(category), Ok(result)) => Err(format!(
            "expected error {category:?}, got result {:?}",
            result.kind
        )),
        _ => Err("invalid fixture expectation".into()),
    }
}

#[derive(Clone, Default)]
pub struct ReferencePlatform {
    extensions: HashMap<String, Extension>,
    grants: HashSet<Grant>,
    proposals: HashMap<String, Proposal>,
    actions: HashMap<String, Action>,
    idempotency: HashMap<(String, String), String>,
    events: Vec<SemanticEvent>,
}

#[derive(Clone)]
struct Extension {
    enabled: bool,
    capabilities: HashSet<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Grant {
    context_id: String,
    agent_id: String,
    capability: String,
}

#[derive(Clone)]
struct Proposal {
    context_id: String,
    agent_id: String,
    capability: String,
    input: Value,
    expires_at: u64,
    approved: bool,
}

#[derive(Clone)]
struct Action {
    context_id: String,
    proposal_id: String,
    input: Value,
    outcome: OutcomeStatus,
}

impl ReferencePlatform {
    fn capability_state(&self, capability: &str) -> CapabilityState {
        let mut installed = false;
        for extension in self.extensions.values() {
            if extension.capabilities.contains(capability) {
                installed = true;
                if extension.enabled {
                    return CapabilityState::Enabled;
                }
            }
        }
        if installed {
            CapabilityState::Disabled
        } else {
            CapabilityState::Unavailable
        }
    }

    fn require_enabled_capability(&self, capability: &str) -> Result<(), SemanticError> {
        match self.capability_state(capability) {
            CapabilityState::Enabled => Ok(()),
            CapabilityState::Disabled => Err(SemanticError::new(
                ErrorCategory::ExtensionDisabled,
                "the extension providing this capability is disabled",
            )),
            CapabilityState::Unavailable => Err(SemanticError::new(
                ErrorCategory::CapabilityUnavailable,
                "no installed extension provides this capability",
            )),
        }
    }

    fn has_grant(&self, context_id: &str, agent_id: &str, capability: &str) -> bool {
        self.grants.contains(&Grant {
            context_id: context_id.into(),
            agent_id: agent_id.into(),
            capability: capability.into(),
        })
    }

    fn event(&mut self, context_id: &str, kind: EventKind, subject_id: &str) {
        self.events.push(SemanticEvent {
            context_id: context_id.into(),
            kind,
            subject_id: subject_id.into(),
        });
    }
}

#[derive(Clone, Copy)]
enum CapabilityState {
    Enabled,
    Disabled,
    Unavailable,
}

impl PlatformAdapter for ReferencePlatform {
    fn apply(&mut self, command: Command) -> Result<SemanticResult, SemanticError> {
        match command {
            Command::InstallExtension {
                extension_id,
                capabilities,
            } => {
                if extension_id.trim().is_empty()
                    || capabilities.is_empty()
                    || capabilities.iter().any(|value| value.trim().is_empty())
                {
                    return Err(SemanticError::new(
                        ErrorCategory::InvalidRequest,
                        "extension identifiers and capabilities must be non-empty",
                    ));
                }
                if self.extensions.contains_key(&extension_id) {
                    return Err(SemanticError::new(
                        ErrorCategory::Conflict,
                        "extension is already installed",
                    ));
                }
                self.extensions.insert(
                    extension_id,
                    Extension {
                        enabled: false,
                        capabilities: capabilities.into_iter().collect(),
                    },
                );
                Ok(SemanticResult::new(ResultKind::ExtensionInstalled))
            }
            Command::SetExtensionEnabled {
                extension_id,
                enabled,
            } => {
                let extension = self.extensions.get_mut(&extension_id).ok_or_else(|| {
                    SemanticError::new(ErrorCategory::NotFound, "extension is not installed")
                })?;
                extension.enabled = enabled;
                Ok(SemanticResult::new(ResultKind::ExtensionStateChanged))
            }
            Command::DiscoverCapabilities { context_id: _ } => {
                let mut capabilities: Vec<_> = self
                    .extensions
                    .values()
                    .filter(|extension| extension.enabled)
                    .flat_map(|extension| extension.capabilities.iter().cloned())
                    .collect();
                capabilities.sort();
                capabilities.dedup();
                let mut result = SemanticResult::new(ResultKind::CapabilitiesDiscovered);
                result.capabilities = capabilities;
                Ok(result)
            }
            Command::GrantCapability {
                context_id,
                agent_id,
                capability,
            } => {
                self.require_enabled_capability(&capability)?;
                self.grants.insert(Grant {
                    context_id: context_id.clone(),
                    agent_id,
                    capability,
                });
                self.event(&context_id, EventKind::CapabilityGranted, "grant");
                Ok(SemanticResult::new(ResultKind::CapabilityGranted))
            }
            Command::RevokeCapability {
                context_id,
                agent_id,
                capability,
            } => {
                self.grants.remove(&Grant {
                    context_id: context_id.clone(),
                    agent_id,
                    capability,
                });
                self.event(&context_id, EventKind::CapabilityRevoked, "grant");
                Ok(SemanticResult::new(ResultKind::CapabilityRevoked))
            }
            Command::CreateProposal {
                proposal_id,
                context_id,
                agent_id,
                capability,
                input,
                expires_at,
            } => {
                self.require_enabled_capability(&capability)?;
                if !self.has_grant(&context_id, &agent_id, &capability) {
                    return Err(SemanticError::new(
                        ErrorCategory::GrantRequired,
                        "the agent has no grant for this context and capability",
                    ));
                }
                if self.proposals.contains_key(&proposal_id) {
                    return Err(SemanticError::new(
                        ErrorCategory::Conflict,
                        "proposal identifier already exists",
                    ));
                }
                self.proposals.insert(
                    proposal_id.clone(),
                    Proposal {
                        context_id: context_id.clone(),
                        agent_id,
                        capability,
                        input,
                        expires_at,
                        approved: false,
                    },
                );
                self.event(&context_id, EventKind::ProposalCreated, &proposal_id);
                Ok(SemanticResult::new(ResultKind::ProposalCreated))
            }
            Command::ApproveProposal {
                proposal_id,
                context_id,
                input,
                approved_at,
            } => {
                let proposal = self.proposals.get_mut(&proposal_id).ok_or_else(|| {
                    SemanticError::new(ErrorCategory::NotFound, "proposal does not exist")
                })?;
                if proposal.context_id != context_id {
                    return Err(SemanticError::new(
                        ErrorCategory::IsolationViolation,
                        "proposal belongs to another user context",
                    ));
                }
                if proposal.input != input {
                    return Err(SemanticError::new(
                        ErrorCategory::ApprovalMismatch,
                        "approval input does not exactly match the proposal",
                    ));
                }
                if approved_at > proposal.expires_at {
                    return Err(SemanticError::new(
                        ErrorCategory::ApprovalExpired,
                        "proposal expired before approval",
                    ));
                }
                proposal.approved = true;
                self.event(&context_id, EventKind::ProposalApproved, &proposal_id);
                Ok(SemanticResult::new(ResultKind::ProposalApproved))
            }
            Command::ExecuteProposal {
                proposal_id,
                context_id,
                agent_id,
                input,
                idempotency_key,
                executed_at,
                provider_observation,
            } => {
                let proposal = self.proposals.get(&proposal_id).cloned().ok_or_else(|| {
                    SemanticError::new(ErrorCategory::NotFound, "proposal does not exist")
                })?;
                if proposal.context_id != context_id {
                    return Err(SemanticError::new(
                        ErrorCategory::IsolationViolation,
                        "proposal belongs to another user context",
                    ));
                }
                if proposal.agent_id != agent_id || proposal.input != input {
                    return Err(SemanticError::new(
                        ErrorCategory::ApprovalMismatch,
                        "execution does not exactly match the approved proposal",
                    ));
                }
                if executed_at > proposal.expires_at {
                    return Err(SemanticError::new(
                        ErrorCategory::ApprovalExpired,
                        "proposal expired before execution",
                    ));
                }
                if !proposal.approved {
                    return Err(SemanticError::new(
                        ErrorCategory::ApprovalRequired,
                        "proposal has not been approved",
                    ));
                }
                self.require_enabled_capability(&proposal.capability)?;
                if !self.has_grant(&context_id, &agent_id, &proposal.capability) {
                    return Err(SemanticError::new(
                        ErrorCategory::GrantRequired,
                        "the agent grant was revoked before execution",
                    ));
                }

                let key = (context_id.clone(), idempotency_key);
                if let Some(existing_id) = self.idempotency.get(&key) {
                    let action = self
                        .actions
                        .get(existing_id)
                        .expect("indexed action exists");
                    if action.proposal_id != proposal_id || action.input != input {
                        return Err(SemanticError::new(
                            ErrorCategory::Conflict,
                            "idempotency key was used for a different action",
                        ));
                    }
                    let mut result = SemanticResult::new(ResultKind::ProposalExecuted);
                    result.action_id = Some(existing_id.clone());
                    result.outcome = Some(action.outcome);
                    return Ok(result);
                }

                let action_id = format!("action:{proposal_id}");
                if self.actions.contains_key(&action_id) {
                    return Err(SemanticError::new(
                        ErrorCategory::Conflict,
                        "proposal already produced an action under another idempotency key",
                    ));
                }
                let outcome = match provider_observation {
                    ProviderObservation::Succeeded { receipt } if !receipt.trim().is_empty() => {
                        OutcomeStatus::Succeeded
                    }
                    ProviderObservation::Succeeded { .. } => {
                        return Err(SemanticError::new(
                            ErrorCategory::InvalidRequest,
                            "success requires provider evidence",
                        ));
                    }
                    ProviderObservation::Failed { code } if !code.trim().is_empty() => {
                        OutcomeStatus::Failed
                    }
                    ProviderObservation::Failed { .. } => {
                        return Err(SemanticError::new(
                            ErrorCategory::InvalidRequest,
                            "failure requires a provider error code",
                        ));
                    }
                    ProviderObservation::Unknown => OutcomeStatus::Unknown,
                };
                self.actions.insert(
                    action_id.clone(),
                    Action {
                        context_id: context_id.clone(),
                        proposal_id: proposal_id.clone(),
                        input,
                        outcome,
                    },
                );
                self.idempotency.insert(key, action_id.clone());
                self.event(&context_id, EventKind::ActionAttempted, &action_id);
                self.event(&context_id, EventKind::OutcomeRecorded, &action_id);
                let mut result = SemanticResult::new(ResultKind::ProposalExecuted);
                result.action_id = Some(action_id);
                result.outcome = Some(outcome);
                Ok(result)
            }
            Command::ReadOutcome {
                context_id,
                action_id,
            } => {
                let action = self.actions.get(&action_id).ok_or_else(|| {
                    SemanticError::new(ErrorCategory::NotFound, "action does not exist")
                })?;
                if action.context_id != context_id {
                    return Err(SemanticError::new(
                        ErrorCategory::IsolationViolation,
                        "action belongs to another user context",
                    ));
                }
                let mut result = SemanticResult::new(ResultKind::OutcomeRead);
                result.action_id = Some(action_id);
                result.outcome = Some(action.outcome);
                Ok(result)
            }
            Command::ReadEvents { context_id } => {
                let mut result = SemanticResult::new(ResultKind::EventsRead);
                result.events = self
                    .events
                    .iter()
                    .filter(|event| event.context_id == context_id)
                    .cloned()
                    .collect();
                Ok(result)
            }
        }
    }
}

pub struct JsonBoundary<A> {
    inner: A,
}

impl<A> JsonBoundary<A> {
    pub fn new(inner: A) -> Self {
        Self { inner }
    }
}

#[derive(Deserialize, Serialize)]
struct BoundaryResponse {
    result: Result<SemanticResult, SemanticError>,
}

impl<A: PlatformAdapter> JsonBoundary<A> {
    pub fn apply_json(&mut self, request: &str) -> Result<String, serde_json::Error> {
        let command: Command = serde_json::from_str(request)?;
        serde_json::to_string(&BoundaryResponse {
            result: self.inner.apply(command),
        })
    }
}

impl<A: PlatformAdapter> PlatformAdapter for JsonBoundary<A> {
    fn apply(&mut self, command: Command) -> Result<SemanticResult, SemanticError> {
        let request = serde_json::to_string(&command).map_err(|error| {
            SemanticError::new(ErrorCategory::InvalidRequest, error.to_string())
        })?;
        let response = self.apply_json(&request).map_err(|error| {
            SemanticError::new(ErrorCategory::InvalidRequest, error.to_string())
        })?;
        serde_json::from_str::<BoundaryResponse>(&response)
            .map_err(|error| SemanticError::new(ErrorCategory::InvalidRequest, error.to_string()))?
            .result
    }
}
