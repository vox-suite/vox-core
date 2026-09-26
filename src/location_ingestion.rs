use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    consent::{ConsentError, ConsentService, DataSource},
    db::Db,
    domain::spans::{NewSpan, SpanPatch, SpanStatus},
    storage::spans::SpanRepository,
};

const PLACES_SEARCH_NEARBY_URL: &str = "https://places.googleapis.com/v1/places:searchNearby";
/// A "visit" worth naming has to be within this many metres of the resolved place,
/// otherwise a same-sounding place a block away could get credited for the stop.
const PLACE_SEARCH_RADIUS_METERS: f64 = 150.0;
const ACTIVITY_MERGE_GAP_MINUTES: i32 = 90;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Driving,
    Walking,
    Cycling,
    Still,
    Unknown,
}

impl ActivityKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Driving => "Driving",
            Self::Walking => "Walking",
            Self::Cycling => "Cycling",
            Self::Still => "Stationary",
            Self::Unknown => "Moving",
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            Self::Driving => "driving",
            Self::Walking => "walking",
            Self::Cycling => "cycling",
            Self::Still => "still",
            Self::Unknown => "movement",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LocationSegment {
    pub activity: ActivityKind,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    /// Present only for a `still` segment long enough to count as a "visit" — a
    /// single point sampled once on arrival, never a continuous trail.
    pub lat: Option<f64>,
    pub lng: Option<f64>,
}

#[derive(Debug, thiserror::Error)]
pub enum LocationIngestionError {
    #[error("segment end must not precede its start")]
    InvalidSegment,
    #[error("location tracking consent has not been granted")]
    ConsentRequired,
    #[error("location storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("consent storage unavailable")]
    Consent(#[from] ConsentError),
}

#[derive(Clone)]
pub struct LocationIngestionService {
    db: Db,
    consent: ConsentService,
    http: reqwest::Client,
    places_api_key: Option<String>,
}

impl LocationIngestionService {
    pub fn new(db: Db, places_api_key: Option<String>) -> Self {
        let consent = ConsentService::new(db.clone());
        Self {
            db,
            consent,
            http: reqwest::Client::new(),
            places_api_key,
        }
    }

    pub async fn submit_segments(
        &self,
        user_id: Uuid,
        segments: Vec<LocationSegment>,
    ) -> Result<Vec<Uuid>, LocationIngestionError> {
        if !self
            .consent
            .is_granted(user_id, DataSource::Location)
            .await?
        {
            return Err(LocationIngestionError::ConsentRequired);
        }
        let newest_ended_at = segments.iter().map(|s| s.ended_at).max();

        let spans = SpanRepository::new(self.db.pool().clone());
        let mut segments = segments;
        segments.sort_by_key(|s| s.started_at);
        let mut ids = Vec::with_capacity(segments.len());
        for segment in segments {
            if segment.ended_at < segment.started_at {
                return Err(LocationIngestionError::InvalidSegment);
            }

            let place_name = match (segment.lat, segment.lng) {
                (Some(lat), Some(lng)) => self.resolve_place_name(lat, lng).await,
                _ => None,
            };

            let (category, title, metadata) = match &place_name {
                Some(name) => (
                    "visit".to_string(),
                    name.clone(),
                    serde_json::json!({ "place_name": name, "lat": segment.lat, "lng": segment.lng }),
                ),
                None => (
                    segment.activity.category().to_string(),
                    segment.activity.label().to_string(),
                    serde_json::json!({}),
                ),
            };

            if place_name.is_none() && segment.activity != ActivityKind::Still {
                if let Some(id) = self.covering_span(user_id, &category, &segment).await? {
                    ids.push(id);
                    continue;
                }
                if let Some(id) = self.extendable_span(user_id, &category, &segment).await? {
                    spans
                        .update(
                            user_id,
                            id,
                            SpanPatch {
                                end_at: Some(Some(segment.ended_at)),
                                ..Default::default()
                            },
                        )
                        .await?;
                    ids.push(id);
                    continue;
                }
            }

            let id = spans
                .record(
                    user_id,
                    NewSpan {
                        title,
                        category: Some(category),
                        source: Some("location".into()),
                        source_ref: Some(format!(
                            "{}:{}",
                            segment.started_at.timestamp_millis(),
                            segment.ended_at.timestamp_millis()
                        )),
                        status: Some(SpanStatus::Done),
                        start_at: Some(segment.started_at),
                        end_at: Some(segment.ended_at),
                        data: Some(metadata),
                        ..Default::default()
                    },
                )
                .await?;

            ids.push(id);
        }

        if let Some(until) = newest_ended_at {
            self.consent
                .advance_sync_cursor(user_id, DataSource::Location, until)
                .await?;
        }

        Ok(ids)
    }

    async fn covering_span(
        &self,
        user_id: Uuid,
        category: &str,
        segment: &LocationSegment,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT id FROM spans WHERE user_id = $1 AND source = 'location' AND category = $2 \
             AND start_at <= $3 AND end_at >= $4 LIMIT 1",
        )
        .bind(user_id)
        .bind(category)
        .bind(segment.started_at)
        .bind(segment.ended_at)
        .fetch_optional(self.db.pool())
        .await
    }

    async fn extendable_span(
        &self,
        user_id: Uuid,
        category: &str,
        segment: &LocationSegment,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT id FROM spans WHERE user_id = $1 AND source = 'location' AND category = $2 \
             AND end_at <= $3 AND end_at >= $3 - make_interval(mins => $4) \
             ORDER BY end_at DESC LIMIT 1",
        )
        .bind(user_id)
        .bind(category)
        .bind(segment.started_at)
        .bind(ACTIVITY_MERGE_GAP_MINUTES)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Best-effort: a missing key, request failure, or no nearby result all just
    /// mean "no place name," never a hard error for the whole batch.
    async fn resolve_place_name(&self, lat: f64, lng: f64) -> Option<String> {
        let api_key = self.places_api_key.as_deref()?;
        let body = serde_json::json!({
            "maxResultCount": 1,
            "locationRestriction": {
                "circle": {
                    "center": { "latitude": lat, "longitude": lng },
                    "radius": PLACE_SEARCH_RADIUS_METERS,
                },
            },
        });

        let response = self
            .http
            .post(PLACES_SEARCH_NEARBY_URL)
            .header("X-Goog-Api-Key", api_key)
            .header("X-Goog-FieldMask", "places.displayName")
            .json(&body)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()?;

        let name = response
            .pointer("/places/0/displayName/text")?
            .as_str()?
            .to_string();
        tracing::info!(lat, lng, place_name = %name, "resolved location visit to a place");
        Some(name)
    }
}
