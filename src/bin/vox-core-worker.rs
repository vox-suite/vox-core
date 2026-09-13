use vox_core::config::Config;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let _config = Config::from_env().expect("Vox Core configuration is invalid");
    tokio::signal::ctrl_c()
        .await
        .expect("Vox Core Worker shutdown listener failed");
}
