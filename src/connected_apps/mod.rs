/**
* Connected apps: provider-verified OAuth connections to remote MCP servers
* and the tools they expose to the user's agent.
*
* Flow: `begin` discovers the app's authorization server, registers Vox as a
* client (dynamically, or with a configured client), and returns the provider
* login URL. The provider redirects back to the host with a code, and
* `complete` verifies the single-use state, exchanges the code with PKCE,
* opens an MCP session with the new token and records the tools the server
* reports. Only then does the extension become active for the agent.
*/
use crate::{
    config::Config,
    db::Db,
    identity::{ResolvedUserContext, UserId},
    remote_extensions::{RemoteExtension, RemoteExtensionError, RemoteExtensionService},
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::Row;
use std::{collections::HashMap, time::Duration};
use url::Url;
use uuid::Uuid;

pub mod crypto;
pub mod mcp;
pub mod oauth;
pub mod tools;

use crypto::{CredentialCipher, pkce_challenge, random_token, sha256_hex};
use mcp::McpSession;
use oauth::{ConfiguredClient, TokenRequest, TokenSet};

const SESSION_TTL_MINUTES: i64 = 10;
const PENDING_ACTION_TTL_MINUTES: i64 = 15;
const MCP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum ConnectedAppError {
    #[error("connected app request is invalid")]
    Invalid,
    #[error("connected app not found")]
    NotFound,
    #[error("connected apps are not configured on this server")]
    NotConfigured,
    #[error("this app needs an OAuth client configured for Vox")]
    ClientNotConfigured,
    #[error("the app rejected the credentials")]
    Unauthorized,
    #[error("the sign-in link expired or was already used")]
    Expired,
    #[error("credential encryption failed")]
    Crypto,
    #[error("{0}")]
    Provider(String),
    #[error(transparent)]
    Extension(#[from] RemoteExtensionError),
    #[error("connected app storage unavailable")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone, Debug, Serialize)]
pub struct AuthorizationStart {
    pub authorization_url: String,
    pub expires_at: DateTime<Utc>,
}

/// One tool from one connected app, as its MCP server described it.
#[derive(Clone, Debug)]
pub struct ConnectedTool {
    pub extension_id: Uuid,
    pub app_key: String,
    pub app_name: String,
    pub tool: Value,
}

#[derive(Clone)]
pub struct ConnectedAppsService {
    db: Db,
    extensions: RemoteExtensionService,
    cipher: Option<CredentialCipher>,
    redirect_uris: Vec<String>,
    configured: HashMap<String, ConfiguredClient>,
    allow_local: bool,
}

impl ConnectedAppsService {
    pub fn from_config(db: Db, config: &Config) -> Self {
        let cipher = config.credential_key.as_deref().and_then(|key| {
            CredentialCipher::from_hex_key(key)
                .inspect_err(|_| tracing::error!("VOX_CREDENTIAL_KEY must be a 32-byte hex key"))
                .ok()
        });
        let configured = config
            .mcp_oauth_clients
            .as_deref()
            .and_then(|raw| {
                serde_json::from_str::<HashMap<String, ConfiguredClient>>(raw)
                    .inspect_err(|err| tracing::error!(%err, "VOX_MCP_OAUTH_CLIENTS is invalid"))
                    .ok()
            })
            .unwrap_or_default();
        Self {
            extensions: RemoteExtensionService::new(db.clone()),
            db,
            cipher,
            redirect_uris: config.mcp_oauth_redirect_uris.clone(),
            configured,
            allow_local: false,
        }
    }

    pub fn new(
        db: Db,
        cipher: CredentialCipher,
        redirect_uris: Vec<String>,
        configured: HashMap<String, ConfiguredClient>,
    ) -> Self {
        Self {
            extensions: RemoteExtensionService::new(db.clone()),
            db,
            cipher: Some(cipher),
            redirect_uris,
            configured,
            allow_local: false,
        }
    }

    /// Loopback mock servers in integration tests only.
    pub fn with_local_endpoints_for_testing(mut self) -> Self {
        self.allow_local = true;
        self.extensions = self.extensions.with_local_endpoints_for_testing();
        self
    }

    /// MCP endpoint hosts that have an OAuth client configured out of band.
    pub fn configured_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self.configured.keys().cloned().collect();
        hosts.sort();
        hosts
    }

    fn cipher(&self) -> Result<&CredentialCipher, ConnectedAppError> {
        self.cipher.as_ref().ok_or(ConnectedAppError::NotConfigured)
    }

    fn configured_for(&self, endpoint: &str) -> Option<&ConfiguredClient> {
        let host = Url::parse(endpoint).ok()?.host_str()?.to_ascii_lowercase();
        self.configured.get(&host)
    }

    pub async fn begin(
        &self,
        context: &ResolvedUserContext,
        extension_id: Uuid,
        redirect_uri: &str,
    ) -> Result<AuthorizationStart, ConnectedAppError> {
        let cipher = self.cipher()?;
        if !self
            .redirect_uris
            .iter()
            .any(|allowed| allowed == redirect_uri)
        {
            return Err(ConnectedAppError::Invalid);
        }
        let row = sqlx::query(
            "SELECT endpoint_url, protocol, lifecycle_state FROM remote_extensions \
             WHERE id = $1 AND user_context_id = $2",
        )
        .bind(extension_id)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ConnectedAppError::NotFound)?;
        let endpoint: String = row.get("endpoint_url");
        let protocol: String = row.get("protocol");
        let state: String = row.get("lifecycle_state");
        if protocol != "mcp" || state == "removed" || state == "quarantined" {
            return Err(ConnectedAppError::Invalid);
        }

        let configured = self.configured_for(&endpoint);
        let discovery = oauth::discover(&endpoint, configured, self.allow_local).await?;
        let client_id = match configured {
            Some(client) => client.client_id.clone(),
            None => {
                self.dynamic_client(&discovery.metadata, redirect_uri)
                    .await?
                    .client_id
            }
        };

        let session_id = Uuid::new_v4();
        let verifier = random_token()?;
        let oauth_state = random_token()?;
        let expires_at = Utc::now() + ChronoDuration::minutes(SESSION_TTL_MINUTES);
        sqlx::query(
            "INSERT INTO mcp_authorization_sessions (
                id, extension_id, user_context_id, state_hash, code_verifier_ciphertext,
                issuer, token_endpoint, client_id, redirect_uri, resource, expires_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(session_id)
        .bind(extension_id)
        .bind(context.id.0)
        .bind(sha256_hex(&oauth_state))
        .bind(cipher.seal(session_id.as_bytes(), &verifier)?)
        .bind(&discovery.metadata.issuer)
        .bind(&discovery.metadata.token_endpoint)
        .bind(&client_id)
        .bind(redirect_uri)
        .bind(&discovery.resource)
        .bind(expires_at)
        .execute(self.db.pool())
        .await?;

        let mut url = Url::parse(&discovery.metadata.authorization_endpoint)
            .map_err(|_| ConnectedAppError::Provider("invalid authorization endpoint".into()))?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", &client_id)
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("state", &oauth_state)
                .append_pair("code_challenge", &pkce_challenge(&verifier))
                .append_pair("code_challenge_method", "S256");
            if !discovery.scopes.is_empty() {
                query.append_pair("scope", &discovery.scopes.join(" "));
            }
            if configured.is_none_or(|c| c.send_resource) {
                query.append_pair("resource", &discovery.resource);
            }
            if let Some(client) = configured {
                for (key, value) in &client.authorize_params {
                    query.append_pair(key, value);
                }
            }
        }
        Ok(AuthorizationStart {
            authorization_url: url.to_string(),
            expires_at,
        })
    }

    async fn dynamic_client(
        &self,
        metadata: &oauth::AuthServerMetadata,
        redirect_uri: &str,
    ) -> Result<oauth::RegisteredClient, ConnectedAppError> {
        if let Some(existing) = self.stored_client(&metadata.issuer, redirect_uri).await? {
            return Ok(existing);
        }
        let registered = oauth::register(metadata, redirect_uri, self.allow_local).await?;
        let id = Uuid::new_v4();
        let secret = match &registered.client_secret {
            Some(secret) => Some(self.cipher()?.seal(id.as_bytes(), secret)?),
            None => None,
        };
        // A concurrent registration may have won; keep the stored one.
        sqlx::query(
            "INSERT INTO mcp_oauth_clients (id, issuer, redirect_uri, client_id, \
             client_secret_ciphertext, token_endpoint_auth_method) VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (issuer, redirect_uri) DO NOTHING",
        )
        .bind(id)
        .bind(&metadata.issuer)
        .bind(redirect_uri)
        .bind(&registered.client_id)
        .bind(secret)
        .bind(&registered.auth_method)
        .execute(self.db.pool())
        .await?;
        self.stored_client(&metadata.issuer, redirect_uri)
            .await?
            .ok_or(ConnectedAppError::ClientNotConfigured)
    }

    async fn stored_client(
        &self,
        issuer: &str,
        redirect_uri: &str,
    ) -> Result<Option<oauth::RegisteredClient>, ConnectedAppError> {
        let Some(row) = sqlx::query(
            "SELECT id, client_id, client_secret_ciphertext, token_endpoint_auth_method \
             FROM mcp_oauth_clients WHERE issuer = $1 AND redirect_uri = $2",
        )
        .bind(issuer)
        .bind(redirect_uri)
        .fetch_optional(self.db.pool())
        .await?
        else {
            return Ok(None);
        };
        let id: Uuid = row.get("id");
        let secret: Option<Vec<u8>> = row.get("client_secret_ciphertext");
        let client_secret = match secret {
            Some(stored) => Some(self.cipher()?.open(id.as_bytes(), &stored)?),
            None => None,
        };
        Ok(Some(oauth::RegisteredClient {
            client_id: row.get("client_id"),
            client_secret,
            auth_method: row.get("token_endpoint_auth_method"),
        }))
    }

    /// Client credentials for token calls, from configuration or the stored
    /// dynamic registration.
    async fn client_credentials(
        &self,
        endpoint: &str,
        issuer: &str,
        client_id: &str,
        redirect_uri: Option<&str>,
    ) -> Result<(Option<String>, String, bool), ConnectedAppError> {
        if let Some(client) = self.configured_for(endpoint) {
            let metadata = oauth::AuthServerMetadata {
                issuer: issuer.to_string(),
                authorization_endpoint: String::new(),
                token_endpoint: String::new(),
                registration_endpoint: None,
                token_endpoint_auth_methods: Vec::new(),
            };
            return Ok((
                client.client_secret.clone(),
                oauth::configured_auth_method(client, &metadata),
                client.send_resource,
            ));
        }
        let row = match redirect_uri {
            Some(redirect) => self.stored_client(issuer, redirect).await?,
            None => {
                let id: Option<(String,)> = sqlx::query_as(
                    "SELECT redirect_uri FROM mcp_oauth_clients WHERE issuer = $1 AND client_id = $2",
                )
                .bind(issuer)
                .bind(client_id)
                .fetch_optional(self.db.pool())
                .await?;
                match id {
                    Some((redirect,)) => self.stored_client(issuer, &redirect).await?,
                    None => None,
                }
            }
        };
        let client = row.ok_or(ConnectedAppError::ClientNotConfigured)?;
        Ok((client.client_secret, client.auth_method, true))
    }

    pub async fn complete(
        &self,
        context: &ResolvedUserContext,
        state: &str,
        code: &str,
    ) -> Result<RemoteExtension, ConnectedAppError> {
        let cipher = self.cipher()?;
        if state.is_empty() || code.is_empty() {
            return Err(ConnectedAppError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;
        let session = sqlx::query(
            "SELECT s.id, s.extension_id, s.code_verifier_ciphertext, s.issuer, s.token_endpoint, \
                    s.client_id, s.redirect_uri, s.resource, s.expires_at, s.consumed_at, \
                    e.endpoint_url, e.current_version \
             FROM mcp_authorization_sessions s JOIN remote_extensions e ON e.id = s.extension_id \
             WHERE s.state_hash = $1 AND s.user_context_id = $2 FOR UPDATE OF s",
        )
        .bind(sha256_hex(state))
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ConnectedAppError::Expired)?;
        let consumed: Option<DateTime<Utc>> = session.get("consumed_at");
        let expires_at: DateTime<Utc> = session.get("expires_at");
        if consumed.is_some() || expires_at < Utc::now() {
            return Err(ConnectedAppError::Expired);
        }
        let session_id: Uuid = session.get("id");
        sqlx::query("UPDATE mcp_authorization_sessions SET consumed_at = now() WHERE id = $1")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        let extension_id: Uuid = session.get("extension_id");
        let endpoint: String = session.get("endpoint_url");
        let version: i32 = session.get("current_version");
        let issuer: String = session.get("issuer");
        let token_endpoint: String = session.get("token_endpoint");
        let client_id: String = session.get("client_id");
        let redirect_uri: String = session.get("redirect_uri");
        let resource: String = session.get("resource");
        let verifier = cipher.open(
            session_id.as_bytes(),
            &session.get::<Vec<u8>, _>("code_verifier_ciphertext"),
        )?;

        let (secret, auth_method, send_resource) = self
            .client_credentials(&endpoint, &issuer, &client_id, Some(&redirect_uri))
            .await?;
        let request = TokenRequest {
            token_endpoint: &token_endpoint,
            client_id: &client_id,
            client_secret: secret.as_deref(),
            auth_method: &auth_method,
            resource: send_resource.then_some(resource.as_str()),
        };
        let tokens =
            oauth::exchange_code(&request, code, &verifier, &redirect_uri, self.allow_local)
                .await?;

        // The connection only counts once the app itself accepts the token.
        let probe = async {
            let mut mcp = McpSession::open(
                &endpoint,
                &tokens.access_token,
                MCP_TIMEOUT,
                self.allow_local,
            )
            .await?;
            let tools = mcp.list_tools().await?;
            Ok::<_, ConnectedAppError>((mcp.server_info.clone(), tools))
        }
        .await;
        let (server_info, tools) = match probe {
            Ok(result) => result,
            Err(err) => {
                let _ = self
                    .extensions
                    .record_conformance(
                        context,
                        extension_id,
                        version,
                        false,
                        json!({"source": "oauth_connect", "error": err.to_string()}),
                    )
                    .await;
                return Err(err);
            }
        };

        self.store_tokens(
            extension_id,
            &issuer,
            &token_endpoint,
            &client_id,
            &resource,
            &tokens,
            Some((&server_info, &tools)),
        )
        .await?;
        self.extensions
            .record_conformance(
                context,
                extension_id,
                version,
                true,
                json!({
                    "source": "oauth_connect",
                    "server_info": server_info,
                    "tool_count": tools.len(),
                    "tools": tools.iter().filter_map(|t| t.get("name")).collect::<Vec<_>>(),
                }),
            )
            .await?;
        Ok(self
            .extensions
            .set_operator_enabled(context, extension_id, true)
            .await?)
    }

    #[allow(clippy::too_many_arguments)]
    async fn store_tokens(
        &self,
        extension_id: Uuid,
        issuer: &str,
        token_endpoint: &str,
        client_id: &str,
        resource: &str,
        tokens: &TokenSet,
        discovered: Option<(&Value, &Vec<Value>)>,
    ) -> Result<(), ConnectedAppError> {
        let cipher = self.cipher()?;
        let access = cipher.seal(&aad(extension_id, "access"), &tokens.access_token)?;
        let refresh = match &tokens.refresh_token {
            Some(token) => Some(cipher.seal(&aad(extension_id, "refresh"), token)?),
            None => None,
        };
        let expires_at = tokens
            .expires_in
            .map(|seconds| Utc::now() + ChronoDuration::seconds(seconds));
        match discovered {
            Some((server_info, tools)) => {
                sqlx::query(
                    "INSERT INTO remote_extension_credentials (
                        extension_id, issuer, token_endpoint, client_id, resource,
                        access_token_ciphertext, refresh_token_ciphertext, scope, expires_at,
                        server_info, tools
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                    ON CONFLICT (extension_id) DO UPDATE SET
                        issuer = EXCLUDED.issuer, token_endpoint = EXCLUDED.token_endpoint,
                        client_id = EXCLUDED.client_id, resource = EXCLUDED.resource,
                        access_token_ciphertext = EXCLUDED.access_token_ciphertext,
                        refresh_token_ciphertext = EXCLUDED.refresh_token_ciphertext,
                        scope = EXCLUDED.scope, expires_at = EXCLUDED.expires_at,
                        server_info = EXCLUDED.server_info, tools = EXCLUDED.tools,
                        connected_at = now(), updated_at = now()",
                )
                .bind(extension_id)
                .bind(issuer)
                .bind(token_endpoint)
                .bind(client_id)
                .bind(resource)
                .bind(access)
                .bind(refresh)
                .bind(&tokens.scope)
                .bind(expires_at)
                .bind(server_info)
                .bind(serde_json::to_value(tools).unwrap_or(Value::Array(vec![])))
                .execute(self.db.pool())
                .await?;
            }
            None => {
                // A refresh may omit the refresh token; keep the old one then.
                sqlx::query(
                    "UPDATE remote_extension_credentials SET access_token_ciphertext = $2,
                        refresh_token_ciphertext = COALESCE($3, refresh_token_ciphertext),
                        expires_at = $4, updated_at = now() WHERE extension_id = $1",
                )
                .bind(extension_id)
                .bind(access)
                .bind(refresh)
                .bind(expires_at)
                .execute(self.db.pool())
                .await?;
            }
        }
        Ok(())
    }

    /// Delete stored credentials. Called when the user removes the app.
    pub async fn forget(&self, extension_id: Uuid) -> Result<(), ConnectedAppError> {
        sqlx::query("DELETE FROM remote_extension_credentials WHERE extension_id = $1")
            .bind(extension_id)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// The user's live connections with the tools each app reported.
    pub async fn connections(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<Value>, ConnectedAppError> {
        let rows = sqlx::query(
            "SELECT c.extension_id, c.tools, c.connected_at \
             FROM remote_extension_credentials c JOIN remote_extensions e ON e.id = c.extension_id \
             WHERE e.user_context_id = $1 AND e.lifecycle_state = 'active'",
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let tools: Value = row.get("tools");
                let summary: Vec<Value> = tools
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .map(|tool| {
                                json!({
                                    "name": tool.get("name"),
                                    "title": tool.get("title").or_else(|| tool.pointer("/annotations/title")),
                                    "description": tool.get("description"),
                                    "read_only": tool.pointer("/annotations/readOnlyHint").and_then(Value::as_bool).unwrap_or(false),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                json!({
                    "extension_id": row.get::<Uuid, _>("extension_id"),
                    "connected_at": row.get::<DateTime<Utc>, _>("connected_at"),
                    "tools": summary,
                })
            })
            .collect())
    }

    /// Every tool of every app the person has connected, across all of their
    /// contexts, so an app connected on the web is usable on a call.
    pub async fn tools_for_user(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ConnectedTool>, ConnectedAppError> {
        let rows = sqlx::query(
            "SELECT e.id, e.external_key, e.display_name, c.tools \
             FROM remote_extensions e \
             JOIN user_contexts uc ON uc.id = e.user_context_id \
             JOIN remote_extension_credentials c ON c.extension_id = e.id \
             WHERE uc.user_id = $1 AND e.lifecycle_state = 'active' \
             ORDER BY e.display_name",
        )
        .bind(user_id.0)
        .fetch_all(self.db.pool())
        .await?;
        let mut tools = Vec::new();
        for row in rows {
            let listed: Value = row.get("tools");
            for tool in listed.as_array().cloned().unwrap_or_default() {
                tools.push(ConnectedTool {
                    extension_id: row.get("id"),
                    app_key: row.get("external_key"),
                    app_name: row.get("display_name"),
                    tool,
                });
            }
        }
        Ok(tools)
    }

    /// Call one tool on a connected app for this user, refreshing an expired
    /// access token once if needed.
    pub async fn call_tool(
        &self,
        user_id: UserId,
        extension_id: Uuid,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Value, ConnectedAppError> {
        let cipher = self.cipher()?;
        let row = sqlx::query(
            "SELECT e.endpoint_url, c.issuer, c.token_endpoint, c.client_id, c.resource, \
                    c.access_token_ciphertext, c.refresh_token_ciphertext, c.expires_at \
             FROM remote_extensions e \
             JOIN user_contexts uc ON uc.id = e.user_context_id \
             JOIN remote_extension_credentials c ON c.extension_id = e.id \
             WHERE e.id = $1 AND uc.user_id = $2 AND e.lifecycle_state = 'active'",
        )
        .bind(extension_id)
        .bind(user_id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ConnectedAppError::NotFound)?;
        let endpoint: String = row.get("endpoint_url");
        let mut access_token = cipher.open(
            &aad(extension_id, "access"),
            &row.get::<Vec<u8>, _>("access_token_ciphertext"),
        )?;
        let expires_at: Option<DateTime<Utc>> = row.get("expires_at");
        let expiring = expires_at.is_some_and(|at| at < Utc::now() + ChronoDuration::seconds(60));
        if expiring {
            access_token = self.refresh_access(extension_id, &row, &endpoint).await?;
        }
        match self
            .call_once(&endpoint, &access_token, tool_name, arguments.clone())
            .await
        {
            Err(ConnectedAppError::Unauthorized) if !expiring => {
                let token = self.refresh_access(extension_id, &row, &endpoint).await?;
                self.call_once(&endpoint, &token, tool_name, arguments)
                    .await
            }
            other => other,
        }
    }

    async fn call_once(
        &self,
        endpoint: &str,
        access_token: &str,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Value, ConnectedAppError> {
        let mut mcp =
            McpSession::open(endpoint, access_token, MCP_TIMEOUT, self.allow_local).await?;
        mcp.call_tool(tool_name, arguments).await
    }

    async fn refresh_access(
        &self,
        extension_id: Uuid,
        row: &sqlx::postgres::PgRow,
        endpoint: &str,
    ) -> Result<String, ConnectedAppError> {
        let cipher = self.cipher()?;
        let stored: Option<Vec<u8>> = row.get("refresh_token_ciphertext");
        let refresh_token = stored
            .map(|s| cipher.open(&aad(extension_id, "refresh"), &s))
            .transpose()?
            .ok_or(ConnectedAppError::Unauthorized)?;
        let issuer: String = row.get("issuer");
        let token_endpoint: String = row.get("token_endpoint");
        let client_id: String = row.get("client_id");
        let resource: String = row.get("resource");
        let (secret, auth_method, send_resource) = self
            .client_credentials(endpoint, &issuer, &client_id, None)
            .await?;
        let request = TokenRequest {
            token_endpoint: &token_endpoint,
            client_id: &client_id,
            client_secret: secret.as_deref(),
            auth_method: &auth_method,
            resource: send_resource.then_some(resource.as_str()),
        };
        let tokens = oauth::refresh(&request, &refresh_token, self.allow_local).await?;
        self.store_tokens(
            extension_id,
            &issuer,
            &token_endpoint,
            &client_id,
            &resource,
            &tokens,
            None,
        )
        .await?;
        Ok(tokens.access_token)
    }

    /// Consequential actions run only after the user confirms them in a later
    /// turn than the one that proposed them. Returns true when the action may
    /// run now.
    pub async fn confirm_or_propose(
        &self,
        user_id: UserId,
        extension_id: Uuid,
        tool_name: &str,
        arguments: &Value,
        turn: Uuid,
    ) -> Result<bool, ConnectedAppError> {
        let arguments_hash = sha256_hex(&canonical_json(arguments));
        let pending: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, proposed_turn FROM connected_app_pending_actions \
             WHERE user_id = $1 AND extension_id = $2 AND tool_name = $3 \
               AND arguments_hash = $4 AND expires_at > now() \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(user_id.0)
        .bind(extension_id)
        .bind(tool_name)
        .bind(&arguments_hash)
        .fetch_optional(self.db.pool())
        .await?;
        match pending {
            Some((id, proposed)) if proposed != turn => {
                sqlx::query("DELETE FROM connected_app_pending_actions WHERE id = $1")
                    .bind(id)
                    .execute(self.db.pool())
                    .await?;
                Ok(true)
            }
            Some(_) => Ok(false),
            None => {
                sqlx::query(
                    "INSERT INTO connected_app_pending_actions \
                     (user_id, extension_id, tool_name, arguments_hash, proposed_turn, expires_at) \
                     VALUES ($1, $2, $3, $4, $5, now() + make_interval(mins => $6))",
                )
                .bind(user_id.0)
                .bind(extension_id)
                .bind(tool_name)
                .bind(&arguments_hash)
                .bind(turn)
                .bind(PENDING_ACTION_TTL_MINUTES as i32)
                .execute(self.db.pool())
                .await?;
                Ok(false)
            }
        }
    }
}

fn aad(extension_id: Uuid, purpose: &str) -> Vec<u8> {
    let mut data = extension_id.as_bytes().to_vec();
    data.extend_from_slice(purpose.as_bytes());
    data
}

/// Stable JSON text with sorted object keys, so the same arguments hash the
/// same regardless of the order the model produced them in.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => other.to_string(),
    }
}
