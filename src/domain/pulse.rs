use super::charts::ChartType;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    Day,
    Week,
    Month,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementKind {
    EventCount,
    NumericSum,
    NumericAverage,
    NumericMedian,
    NumericP95,
    KnownIntervalDuration,
    RecurringCostProjection,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SourceProfile {
    pub key: String,
    pub schema_id: Option<Uuid>,
    pub connection_id: Option<Uuid>,
    pub source: String,
    pub category: String,
    pub action: String,
    pub timing: String,
    pub currency: String,
    pub count: i64,
    pub dated_count: i64,
    pub first_at: Option<DateTime<Utc>>,
    pub last_at: Option<DateTime<Utc>>,
    pub known_intervals: i64,
    pub fields: BTreeMap<String, String>,
    pub samples: Vec<serde_json::Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Measurement {
    pub id: String,
    pub profile: SourceProfile,
    pub title: String,
    pub description: String,
    pub kind: MeasurementKind,
    pub field: Option<String>,
    pub unit: String,
    pub quality: String,
    pub scale: f64,
    pub buckets: Vec<Bucket>,
    pub dimensions: Vec<String>,
    pub default_dimension: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PulseDefinition {
    pub version: u8,
    pub measurement_id: String,
    pub bucket: Option<Bucket>,
    pub dimension: Option<String>,
    pub period_days: u16,
    #[serde(default)]
    pub offset_days: u16,
    #[serde(default)]
    pub top_n: Option<u16>,
    pub timezone: String,
    pub chart_type: ChartType,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PulsePoint {
    pub label: String,
    pub value: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PulseResult {
    #[serde(default)]
    pub coverage: serde_json::Value,
    pub total: Option<f64>,
    pub source: String,
    pub points: Vec<PulsePoint>,
    pub unit: String,
    pub quality: String,
    pub description: String,
    pub record_count: i64,
    pub undated_count: i64,
    pub computed_at: DateTime<Utc>,
    pub data_as_of: Option<DateTime<Utc>>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PulseSuggestion {
    pub title: String,
    pub reason: String,
    pub definition: PulseDefinition,
    pub measurement: Measurement,
    pub preview: PulseResult,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SavedPulseChart {
    pub id: Uuid,
    pub title: String,
    pub definition: PulseDefinition,
    pub created_at: DateTime<Utc>,
    pub result: Option<PulseResult>,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PulseConnection {
    pub connector_id: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub authorization_state: String,
    pub sync_timeline: bool,
    pub assistant_read: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DiscoveryResponse {
    pub suggestions: Vec<PulseSuggestion>,
    pub connections: Vec<PulseConnection>,
    pub source_count: usize,
    pub record_count: i64,
    pub profiled_days: u16,
    pub computed_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CanvasResponse {
    pub charts: Vec<SavedPulseChart>,
    pub next_cursor: Option<Uuid>,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryInput {
    pub timezone: String,
    #[serde(default)]
    pub refresh: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SavePulseInput {
    pub idempotency_key: Uuid,
    pub title: String,
    pub definition: PulseDefinition,
}
#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PulseCandidate {
    pub title: String,
    pub reason: String,
    pub definition: PulseDefinition,
}

#[derive(Clone, Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UpdatePulseChartInput {
    pub title: Option<String>,
    pub definition: Option<PulseDefinition>,
    pub sort_order: Option<i32>,
    pub is_pinned: Option<bool>,
}
