/**
* Application service for spans: validation, ownership, and change notifications.
*/
use uuid::Uuid;

use crate::{
    domain::{
        ConcurrencyOutcome,
        identity::Actor,
        spans::{NewSpan, Span, SpanPatch, SpanQuery},
    },
    realtime::UserEventHub,
    storage::spans::SpanRepository,
};

#[derive(Debug, thiserror::Error)]
pub enum SpanServiceError {
    #[error("invalid span: {0}")]
    Invalid(&'static str),
    #[error("referenced collection or parent span not found")]
    ReferenceNotFound,
    #[error("span not found")]
    NotFound,
    #[error("optimistic concurrency conflict")]
    VersionConflict,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct SpanService {
    repo: SpanRepository,
    user_events: UserEventHub,
}

impl SpanService {
    pub fn new(repo: SpanRepository, user_events: UserEventHub) -> Self {
        Self { repo, user_events }
    }

    pub async fn create_span(
        &self,
        actor: &Actor,
        input: NewSpan,
    ) -> Result<Span, SpanServiceError> {
        if input.title.trim().is_empty() {
            return Err(SpanServiceError::Invalid("title must not be empty"));
        }
        if let (Some(start), Some(end)) = (input.start_at, input.end_at)
            && end < start
        {
            return Err(SpanServiceError::Invalid(
                "end_at must not precede start_at",
            ));
        }
        let span = self
            .repo
            .create(actor.user_id, input)
            .await
            .map_err(missing_reference)?;
        self.notify(actor.user_id, "span_created", span.id);
        Ok(span)
    }

    pub async fn update_span(
        &self,
        actor: &Actor,
        id: Uuid,
        patch: SpanPatch,
    ) -> Result<Span, SpanServiceError> {
        match self.repo.update(actor.user_id, id, patch).await? {
            ConcurrencyOutcome::Success(span) => {
                self.notify(actor.user_id, "span_updated", span.id);
                Ok(span)
            }
            ConcurrencyOutcome::Conflict => Err(SpanServiceError::VersionConflict),
            ConcurrencyOutcome::NotFound => Err(SpanServiceError::NotFound),
        }
    }

    pub async fn get_span(&self, actor: &Actor, id: Uuid) -> Result<Option<Span>, sqlx::Error> {
        self.repo.get_by_id(actor.user_id, id).await
    }

    pub async fn list_spans(
        &self,
        actor: &Actor,
        query: &SpanQuery,
    ) -> Result<Vec<Span>, SpanServiceError> {
        if let (Some(from), Some(to)) = (query.from, query.to)
            && to < from
        {
            return Err(SpanServiceError::Invalid("to must not precede from"));
        }
        Ok(self.repo.list(actor.user_id, query).await?)
    }

    pub async fn delete_span(&self, actor: &Actor, id: Uuid) -> Result<bool, sqlx::Error> {
        let deleted = self.repo.delete(actor.user_id, id).await?;
        if deleted {
            self.notify(actor.user_id, "span_deleted", id);
        }
        Ok(deleted)
    }

    fn notify(&self, user_id: Uuid, kind: &str, span_id: Uuid) {
        self.user_events.notify(
            user_id,
            serde_json::json!({"type": kind, "span_id": span_id}),
        );
    }
}

fn missing_reference(err: sqlx::Error) -> SpanServiceError {
    match &err {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23503") => {
            SpanServiceError::ReferenceNotFound
        }
        _ => SpanServiceError::Database(err),
    }
}
