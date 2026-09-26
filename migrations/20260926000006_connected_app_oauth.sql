-- OAuth clients Vox holds with each MCP authorization server. Dynamic clients
-- are registered once per (issuer, redirect URI) and reused for every user.
CREATE TABLE mcp_oauth_clients (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issuer TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    client_id TEXT NOT NULL,
    client_secret_ciphertext BYTEA,
    token_endpoint_auth_method TEXT NOT NULL DEFAULT 'none',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT mcp_oauth_clients_issuer_redirect_unique UNIQUE (issuer, redirect_uri)
);

-- One pending browser authorization. Only a hash of the state is stored, the
-- PKCE verifier is encrypted, and a session is single-use and short-lived.
CREATE TABLE mcp_authorization_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    extension_id UUID NOT NULL REFERENCES remote_extensions(id) ON DELETE CASCADE,
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    state_hash TEXT NOT NULL UNIQUE,
    code_verifier_ciphertext BYTEA NOT NULL,
    issuer TEXT NOT NULL,
    token_endpoint TEXT NOT NULL,
    client_id TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    resource TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Provider-verified credentials for a connected app plus the tools its MCP
-- server reported. Tokens are AES-256-GCM encrypted with VOX_CREDENTIAL_KEY.
CREATE TABLE remote_extension_credentials (
    extension_id UUID PRIMARY KEY REFERENCES remote_extensions(id) ON DELETE CASCADE,
    issuer TEXT NOT NULL,
    token_endpoint TEXT NOT NULL,
    client_id TEXT NOT NULL,
    resource TEXT NOT NULL,
    access_token_ciphertext BYTEA NOT NULL,
    refresh_token_ciphertext BYTEA,
    scope TEXT,
    expires_at TIMESTAMPTZ,
    server_info JSONB NOT NULL DEFAULT '{}'::jsonb,
    tools JSONB NOT NULL DEFAULT '[]'::jsonb,
    connected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A connected-app action that changes something in the user's account waits
-- here until the user confirms it in a later conversation turn.
CREATE TABLE connected_app_pending_actions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    extension_id UUID NOT NULL REFERENCES remote_extensions(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    arguments_hash TEXT NOT NULL,
    proposed_turn UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX connected_app_pending_actions_lookup
    ON connected_app_pending_actions (user_id, extension_id, tool_name, arguments_hash);
