use vox_core::{
    config::Config,
    http::{AppState, router},
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let config = Config::from_env().expect("Vox Core configuration is invalid");
    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .expect("Vox Core API address is unavailable");
    axum::serve(listener, router(AppState::new(true)))
        .await
        .expect("Vox Core API failed");
}
