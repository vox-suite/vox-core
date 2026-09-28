//! Explicit release bootstrap for curated default skills.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("DATABASE_URL")?;
    let db = vox_core::db::Db::connect(&url).await?;
    db.migrate().await?;
    let deployments: Vec<String> =
        sqlx::query_scalar("SELECT external_key FROM platform_deployments ORDER BY external_key")
            .fetch_all(db.pool())
            .await?;
    for deployment in &deployments {
        vox_connections::defaults::publish(db.pool().clone(), deployment).await?;
    }
    println!(
        "Published six reviewed default skills to {} deployments",
        deployments.len()
    );
    Ok(())
}
