CREATE TABLE external_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    integration_id UUID NOT NULL REFERENCES integration_definitions(id) ON DELETE RESTRICT,
    external_account_hash BYTEA NOT NULL,
    credential_custody TEXT NOT NULL,
    authorization_state TEXT NOT NULL,
    authorized_capabilities TEXT[] NOT NULL DEFAULT '{}'::text[],
    expires_at TIMESTAMPTZ,
    failure_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    CONSTRAINT external_connections_unique_account UNIQUE (user_context_id, integration_id, external_account_hash),
    CONSTRAINT external_connections_account_hash_length CHECK (octet_length(external_account_hash) = 32),
    CONSTRAINT external_connections_custody_valid CHECK (credential_custody IN ('platform_held', 'external_operator')),
    CONSTRAINT external_connections_state_valid CHECK (authorization_state IN ('pending', 'authorized', 'expired', 'revoked', 'cancelled', 'failed')),
    CONSTRAINT external_connections_failure_shape CHECK ((authorization_state = 'failed' AND failure_code IS NOT NULL) OR (authorization_state <> 'failed')),
    CONSTRAINT external_connections_expiry_shape CHECK ((authorization_state = 'authorized') OR expires_at IS NULL)
);
CREATE INDEX external_connections_context_state_idx ON external_connections (user_context_id, authorization_state);
