/**
* Worker process entry point running background schedulers and task consumers.
*/
mod config;
mod health;
mod runtime;

use tokio_util::sync::CancellationToken;
use vox_core::config::Config;

#[tokio::main]
async fn main() {
    let _traces = vox_core::telemetry::init("vox-core-worker");
    let config = Config::from_env().expect("Vox Core worker configuration is invalid");

    let cancellation = CancellationToken::new();
    let shutdown_signal = cancellation.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("Worker shutting down via Ctrl+C");
            shutdown_signal.cancel();
        }
    });

    tracing::info!("Starting Vox Core Worker runtime");
    if let Err(e) = runtime::run_worker(config, cancellation).await {
        tracing::error!("Worker failure: {}", e);
        std::process::exit(1);
    }
}
