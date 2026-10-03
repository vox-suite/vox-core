CREATE TABLE playstation_accounts (
    connection_id UUID PRIMARY KEY REFERENCES external_connections(id) ON DELETE CASCADE,
    generation UUID NOT NULL DEFAULT gen_random_uuid(),
    account_id TEXT NOT NULL CHECK (length(account_id) BETWEEN 1 AND 128),
    access_ciphertext BYTEA NOT NULL,
    refresh_ciphertext BYTEA NOT NULL,
    access_expires_at TIMESTAMPTZ NOT NULL,
    refresh_expires_at TIMESTAMPTZ NOT NULL,
    capture_enabled BOOLEAN NOT NULL DEFAULT false,
    snapshots JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(snapshots) = 'object'),
    last_synced_at TIMESTAMPTZ,
    next_sync_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    failure_code TEXT,
    failure_count INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX playstation_accounts_due ON playstation_accounts(next_sync_at) WHERE capture_enabled;
ALTER TABLE playstation_accounts ENABLE ROW LEVEL SECURITY;
