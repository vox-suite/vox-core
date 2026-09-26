/**
* Domain models for collections: groupings of spans such as trips and events.
*/
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    Trip,
    Event,
    Course,
    Area,
    #[default]
    Custom,
}

impl CollectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trip => "trip",
            Self::Event => "event",
            Self::Course => "course",
            Self::Area => "area",
            Self::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "trip" => Self::Trip,
            "event" => Self::Event,
            "course" => Self::Course,
            "area" => Self::Area,
            _ => Self::Custom,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CollectionStatus {
    #[default]
    Active,
    Paused,
    Completed,
    Archived,
}

impl CollectionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "archived" => Self::Archived,
            _ => Self::Active,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub description: String,
    pub kind: CollectionKind,
    pub status: CollectionStatus,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub metadata: serde_json::Value,
    pub span_count: i64,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
