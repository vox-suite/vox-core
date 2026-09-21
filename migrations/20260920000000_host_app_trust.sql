ALTER TABLE host_apps
    ADD COLUMN allowed_origins TEXT[] NOT NULL DEFAULT '{}'::text[];

CREATE TABLE host_app_credentials (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL,
    host_app_id UUID NOT NULL,
    secret_hash BYTEA NOT NULL,
    state TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    CONSTRAINT host_app_credentials_host_app_fkey
        FOREIGN KEY (deployment_id, host_app_id)
        REFERENCES host_apps(deployment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT host_app_credentials_secret_hash_length
        CHECK (octet_length(secret_hash) = 32),
    CONSTRAINT host_app_credentials_state_valid
        CHECK (state IN ('active', 'revoked')),
    CONSTRAINT host_app_credentials_revocation_state
        CHECK (
            (state = 'active' AND revoked_at IS NULL)
            OR (state = 'revoked' AND revoked_at IS NOT NULL)
        )
);

CREATE INDEX host_app_credentials_active_idx
    ON host_app_credentials (id)
    WHERE state = 'active';

CREATE TABLE host_app_assertion_nonces (
    credential_id UUID NOT NULL REFERENCES host_app_credentials(id) ON DELETE CASCADE,
    nonce UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (credential_id, nonce)
);

CREATE INDEX host_app_assertion_nonces_expiry_idx
    ON host_app_assertion_nonces (expires_at);
