CREATE TABLE user_identities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel TEXT NOT NULL,
    external_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_identities_channel_external_key UNIQUE (channel, external_id),
    CONSTRAINT user_identities_channel_not_empty CHECK (length(btrim(channel)) > 0),
    CONSTRAINT user_identities_external_not_empty CHECK (length(btrim(external_id)) > 0)
);
