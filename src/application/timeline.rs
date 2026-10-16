use uuid::Uuid;

use crate::{
    domain::{
        identity::Actor,
        timeline::{
            IngestTimelineEventInput, NewEventType, TimelineEventType, TimelineEventWithEvidence,
            TimelineGroup, TimelinePage, TimelineQuery, UpdateTimelineEventInput,
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
        if query.merchant.as_ref().is_some_and(|s| s.len() > 500)
            || query.category.as_ref().is_some_and(|s| s.len() > 100)
            || matches!((query.start_at,query.end_at),(Some(a),Some(b)) if a>=b)
        {
            return Err(TimelineServiceError::Invalid(
                "Invalid timeline filter".into(),
            ));
        }
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

    pub async fn update_event(
        &self,
        actor: &Actor,
        event_id: Uuid,
        mut input: UpdateTimelineEventInput,
    ) -> Result<(), TimelineServiceError> {
        if let Some(title) = &input.title {
            let title = title.trim();
            if title.is_empty() || title.chars().count() > 500 {
                return Err(TimelineServiceError::Invalid("title".into()));
            }
            input.title = Some(title.to_string());
        }
        if let (Some(start), Some(end)) = (input.occurred_at, input.ended_at)
            && end < start
        {
            return Err(TimelineServiceError::Invalid("ended_at".into()));
        }
        if self
            .repo
            .update_event(actor.user_id, event_id, input)
            .await?
        {
            Ok(())
        } else {
            Err(TimelineServiceError::NotFound("timeline event"))
        }
    }

    pub async fn delete_event(
        &self,
        actor: &Actor,
        event_id: Uuid,
    ) -> Result<(), TimelineServiceError> {
        if self.repo.delete_event(actor.user_id, event_id).await? {
            Ok(())
        } else {
            Err(TimelineServiceError::NotFound("timeline event"))
        }
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
