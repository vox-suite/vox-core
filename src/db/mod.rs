/**
* Database connection pool management and migration execution.
*/
pub mod jobs;

use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

#[derive(Clone)]
pub struct Db {
    pool: PgPool,
}

impl Db {
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        Self::connect_with_pool(
            database_url,
            crate::config::DEFAULT_DB_MAX_CONNECTIONS,
            crate::config::DEFAULT_DB_ACQUIRE_TIMEOUT_SECS,
        )
        .await
    }

    pub async fn connect_with_pool(
        database_url: &str,
        max_connections: u32,
        acquire_timeout_secs: u64,
    ) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(acquire_timeout_secs))
            .connect(database_url)
            .await?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!().run(&self.pool).await
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}
