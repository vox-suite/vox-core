use uuid::Uuid;

use crate::{
    domain::{
        identity::Actor,
        timeline::{
            IngestTimelineEventInput, NewEventType, TimelineEventType, TimelineEventWithEvidence,
            TimelineGroup, TimelinePage, TimelineQuery,
        },
    },
    storage::timeline::{TimelineRepository, TimelineStorageError},
};

#[derive(Debug, thiserror::Error)]
pub enum TimelineServiceError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found: {0}")]
    NotFound(&'static str),
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("storage error: {0}")]
    Storage(#[from] TimelineStorageError),
}

#[derive(Clone)]
pub struct TimelineService {
    repo: TimelineRepository,
}

impl TimelineService {
    pub fn new(repo: TimelineRepository) -> Self {
        Self { repo }
    }

    pub fn repo(&self) -> &TimelineRepository {
        &self.repo
    }

    pub async fn list_groups(&self) -> Result<Vec<TimelineGroup>, TimelineServiceError> {
        self.repo.list_groups().await.map_err(Into::into)
    }

    pub async fn list_event_types(
        &self,
        actor: &Actor,
        group_id: Option<Uuid>,
        group_value: Option<&str>,
    ) -> Result<Vec<TimelineEventType>, TimelineServiceError> {
        self.repo
            .list_event_types(actor.user_id, group_id, group_value)
            .await
            .map_err(Into::into)
    }

    pub async fn create_event_type(
        &self,
        actor: &Actor,
        input: NewEventType,
    ) -> Result<TimelineEventType, TimelineServiceError> {
        self.repo
            .create_event_type(actor.user_id, input)
            .await
            .map_err(Into::into)
    }

    pub async fn query_events(
        &self,
        actor: &Actor,
        query: TimelineQuery,
    ) -> Result<TimelinePage, TimelineServiceError> {
        self.repo
            .query_events(actor.user_id, query)
            .await
            .map_err(Into::into)
    }

    pub async fn ingest_event(
        &self,
        actor: &Actor,
        input: IngestTimelineEventInput,
    ) -> Result<TimelineEventWithEvidence, TimelineServiceError> {
        self.repo
            .ingest_event(actor.user_id, input)
            .await
            .map_err(Into::into)
    }

    pub async fn ingest_event_for_user(
        &self,
        user_id: Uuid,
        input: IngestTimelineEventInput,
    ) -> Result<TimelineEventWithEvidence, TimelineServiceError> {
        self.repo
            .ingest_event(user_id, input)
            .await
            .map_err(Into::into)
    }
}
