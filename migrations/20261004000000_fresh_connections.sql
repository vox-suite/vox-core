CREATE TABLE IF NOT EXISTS vox_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    connector_id TEXT NOT NULL,
    account_id TEXT,
    account_display_id TEXT,
    access_ciphertext BYTEA,
    refresh_ciphertext BYTEA,
    access_expires_at TIMESTAMPTZ,
    authorization_state TEXT NOT NULL DEFAULT 'authorized',
    sync_timeline BOOLEAN NOT NULL DEFAULT true,
    assistant_read BOOLEAN NOT NULL DEFAULT true,
    last_synced_at TIMESTAMPTZ,
    next_sync_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    failure_code TEXT,
    failure_count INTEGER NOT NULL DEFAULT 0,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT vox_connections_user_connector_unique UNIQUE (user_id, connector_id)
);

CREATE INDEX IF NOT EXISTS vox_connections_sync_due_idx
    ON vox_connections(next_sync_at)
    WHERE authorization_state = 'authorized' AND sync_timeline;

CREATE TABLE IF NOT EXISTS vox_connection_setups (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    connector_id TEXT NOT NULL,
    state_token TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL DEFAULT 'pending',
    connection_id UUID REFERENCES vox_connections(id) ON DELETE SET NULL,
    error TEXT,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '15 minutes',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS vox_connection_setups_token_idx
    ON vox_connection_setups(state_token);
