//! Read-only release preflight. Never applies migrations or publishes defaults.
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is required")?;
    let connection = tokio::time::timeout(
        Duration::from_secs(8),
        vox_core::db::Db::connect_with_pool(&url, 1, 4),
    )
    .await;
    match connection {
        Ok(Ok(db)) if db.is_available().await => {
            println!("Database preflight passed");
            Ok(())
        }
        _ => Err("Database preflight failed; no migrations or release changes were made".into()),
    }
}
