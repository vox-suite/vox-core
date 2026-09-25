use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    consent::{ConsentError, ConsentService, DataSource},
    db::Db,
};

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
}

impl LocationIngestionService {
    pub fn new(db: Db) -> Self {
        let consent = ConsentService::new(db.clone());
        Self { db, consent }
    }

    pub async fn submit_segments(
        &self,
        user_id: Uuid,
        segments: Vec<LocationSegment>,
    ) -> Result<Vec<Uuid>, LocationIngestionError> {
        if !self.consent.is_granted(user_id, DataSource::Location).await? {
            return Err(LocationIngestionError::ConsentRequired);
        }

        let mut ids = Vec::with_capacity(segments.len());
        for segment in segments {
            if segment.ended_at < segment.started_at {
                return Err(LocationIngestionError::InvalidSegment);
            }

            let duration_minutes = (segment.ended_at - segment.started_at).num_minutes().max(1);
            let title = format!("{} for {} min", segment.activity.label(), duration_minutes);

            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO device_timeline_entries (user_id, source, category, title, kind, start_at, end_at) \
                 VALUES ($1, 'location', $2, $3, 'completed', $4, $5) RETURNING id",
            )
            .bind(user_id)
            .bind(segment.activity.category())
            .bind(&title)
            .bind(segment.started_at)
            .bind(segment.ended_at)
            .fetch_one(self.db.pool())
            .await?;

            ids.push(id);
        }

        Ok(ids)
    }
}
