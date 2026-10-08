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
    pub(crate) fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }
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

    /// A bounded query, rather than a configured URL or a startup flag, proves
    /// that this process can currently serve database-backed requests.
    pub async fn is_available(&self) -> bool {
        matches!(
            tokio::time::timeout(
                Duration::from_secs(2),
                sqlx::query("SELECT 1").execute(&self.pool)
            )
            .await,
            Ok(Ok(_))
        )
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unavailable_database_never_reports_ready() {
        let db = Db {
            pool: PgPoolOptions::new()
                .connect_lazy("postgresql://fixture:fixture@127.0.0.1:1/fixture")
                .unwrap(),
        };
        assert!(!db.is_available().await);
    }
}
