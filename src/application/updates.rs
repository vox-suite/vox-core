use uuid::Uuid;

use crate::{
    domain::{
        identity::Actor,
        updates::{
            JobActionResponse, JobInputRequest, JobRetryRequest, UpdateItem, UpdatesQuery,
        },
    },
    storage::updates::{UpdatesRepository, UpdatesStorageError},
};

#[derive(Debug, thiserror::Error)]
pub enum UpdatesServiceError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("storage error: {0}")]
    Storage(#[from] UpdatesStorageError),
}

#[derive(Clone)]
pub struct UpdatesService {
    repo: UpdatesRepository,
}

impl UpdatesService {
    pub fn new(repo: UpdatesRepository) -> Self {
        Self { repo }
    }

    pub fn repo(&self) -> &UpdatesRepository {
        &self.repo
    }

    pub async fn list_updates(
        &self,
        actor: &Actor,
        query: UpdatesQuery,
    ) -> Result<Vec<UpdateItem>, UpdatesServiceError> {
        self.repo
            .list_updates(actor.user_id, query)
            .await
            .map_err(Into::into)
    }

    pub async fn get_update(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<UpdateItem, UpdatesServiceError> {
        self.repo
            .get_update(actor.user_id, id)
            .await
            .map_err(Into::into)
    }

    pub async fn mark_read(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<UpdateItem, UpdatesServiceError> {
        self.repo
            .mark_read(actor.user_id, id)
            .await
            .map_err(Into::into)
    }

    pub async fn dismiss(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<UpdateItem, UpdatesServiceError> {
        self.repo
            .dismiss(actor.user_id, id)
            .await
            .map_err(Into::into)
    }

    pub async fn resolve(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<UpdateItem, UpdatesServiceError> {
        self.repo
            .resolve(actor.user_id, id)
            .await
            .map_err(Into::into)
    }

    pub async fn retry_job(
        &self,
        actor: &Actor,
        job_id: Uuid,
        req: JobRetryRequest,
    ) -> Result<JobActionResponse, UpdatesServiceError> {
        self.repo
            .retry_job(actor.user_id, job_id, req)
            .await
            .map_err(Into::into)
    }

    pub async fn provide_job_input(
        &self,
        actor: &Actor,
        job_id: Uuid,
        req: JobInputRequest,
    ) -> Result<JobActionResponse, UpdatesServiceError> {
        self.repo
            .provide_job_input(actor.user_id, job_id, req)
            .await
            .map_err(Into::into)
    }
}
