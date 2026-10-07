use super::pulse::{ComposeMessage, PulseDefinition};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalKind {
    /// Progress is computed from a Pulse measurement.
    Metric,
    /// Progress is the sum of amounts the user logs.
    Saving,
}
impl GoalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metric => "metric",
            Self::Saving => "saving",
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalDirection {
    AtLeast,
    AtMost,
}
impl GoalDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AtLeast => "at_least",
            Self::AtMost => "at_most",
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalPeriod {
    Week,
    Month,
}
impl GoalPeriod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Week => "week",
            Self::Month => "month",
        }
    }
}

#[derive(Clone, Debug)]
pub struct GoalRow {
    pub id: Uuid,
    pub title: String,
    pub kind: GoalKind,
    pub direction: GoalDirection,
    pub period: Option<GoalPeriod>,
    pub target: f64,
    pub unit: String,
    pub definition: Option<PulseDefinition>,
    pub starts_on: NaiveDate,
    pub deadline: Option<NaiveDate>,
    pub created_at: DateTime<Utc>,
    pub saved: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GoalDraft {
    pub timezone: String,
    pub title: String,
    pub kind: GoalKind,
    pub direction: GoalDirection,
    pub period: Option<GoalPeriod>,
    pub target: f64,
    pub unit: String,
    pub definition: Option<PulseDefinition>,
    pub deadline: Option<NaiveDate>,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GoalView {
    pub id: Uuid,
    pub title: String,
    pub kind: GoalKind,
    pub direction: GoalDirection,
    pub period: Option<GoalPeriod>,
    pub target: f64,
    pub unit: String,
    pub current: f64,
    pub percent: f64,
    /// done, ahead, on_track, behind, in_progress, within_limit, over_limit or unavailable.
    pub status: String,
    pub remaining: f64,
    pub starts_on: NaiveDate,
    pub deadline: Option<NaiveDate>,
    /// End of the current week or month for recurring goals.
    pub period_ends_on: Option<NaiveDate>,
    pub days_left: Option<i64>,
    pub per_week_needed: Option<f64>,
    pub projected_on: Option<NaiveDate>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddGoalEntryInput {
    pub timezone: String,
    pub amount: f64,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GoalComposeInput {
    pub timezone: String,
    pub messages: Vec<ComposeMessage>,
    pub current: Option<GoalDraft>,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GoalComposeResponse {
    pub reply: String,
    pub draft: Option<GoalDraft>,
    pub preview: Option<GoalView>,
}

/// What the model proposes; the server turns it into a validated `GoalDraft`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalSuggestion {
    pub title: String,
    pub kind: GoalKind,
    #[serde(default = "default_direction")]
    pub direction: GoalDirection,
    pub period: Option<GoalPeriod>,
    pub target: f64,
    pub unit: Option<String>,
    pub measurement_id: Option<String>,
    pub deadline: Option<NaiveDate>,
}
fn default_direction() -> GoalDirection {
    GoalDirection::AtLeast
}
#[derive(Clone, Debug, Deserialize)]
pub struct GoalComposeOutput {
    pub reply: String,
    pub draft: Option<GoalSuggestion>,
}

#[derive(Clone, Debug, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveNodeGoalInput {
    pub timezone: String,
}
