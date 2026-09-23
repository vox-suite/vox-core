-- Migration: 20260923000004_provider_verified_connections.sql
-- Provider-verified connection authorization sessions and account display identity.

ALTER TABLE external_connections
    ADD COLUMN IF NOT EXISTS account_display_id TEXT;

CREATE TABLE IF NOT EXISTS connection_authorization_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    integration_id UUID NOT NULL REFERENCES integration_definitions(id) ON DELETE CASCADE,
    state_token TEXT NOT NULL UNIQUE,
    credential_custody TEXT NOT NULL DEFAULT 'external_operator',
    requested_capabilities TEXT[] NOT NULL DEFAULT '{}'::text[],
    redirect_uri TEXT,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT conn_auth_sess_custody_valid CHECK (credential_custody IN ('platform_held', 'external_operator'))
);

CREATE INDEX IF NOT EXISTS conn_auth_sess_state_idx ON connection_authorization_sessions (state_token) WHERE consumed_at IS NULL;
