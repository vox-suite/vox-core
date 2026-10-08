//! Host configuration bridge for MCP account authorization.
pub use vox_connections::connected_apps::*;

pub fn from_config(db: crate::db::Db, config: &crate::config::Config) -> ConnectedAppsService {
    ConnectedAppsService::from_options(
        db.pool().clone(),
        ConnectedAppsOptions {
            credential_key: config.credential_key.clone(),
            redirect_uris: config.mcp_oauth_redirect_uris.clone(),
            oauth_clients: config.mcp_oauth_clients.clone(),
        },
    )
}
