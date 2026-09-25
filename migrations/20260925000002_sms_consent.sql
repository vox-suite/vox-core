CREATE TABLE data_source_consents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    data_source TEXT NOT NULL CHECK (data_source IN ('sms', 'location')),
    granted_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    retention_days INTEGER NOT NULL DEFAULT 90 CHECK (retention_days > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT data_source_consents_user_source_unique UNIQUE (user_id, data_source)
);
