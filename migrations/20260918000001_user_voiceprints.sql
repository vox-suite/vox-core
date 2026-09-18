CREATE TABLE IF NOT EXISTS user_voiceprints (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    signature JSONB NOT NULL,
    sample_duration_ms INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_voiceprints_user_id_key UNIQUE (user_id),
    CONSTRAINT user_voiceprints_signature_is_object CHECK (jsonb_typeof(signature) = 'object')
);

CREATE INDEX IF NOT EXISTS user_voiceprints_user_id_idx ON user_voiceprints (user_id);
