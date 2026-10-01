ALTER TABLE channel_identities ADD COLUMN otp_verified_at TIMESTAMPTZ;

CREATE TABLE phone_verifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    normalized_phone TEXT NOT NULL,
    code_hash TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX phone_verifications_user_idx ON phone_verifications (user_id, created_at DESC);
CREATE INDEX phone_verifications_phone_idx ON phone_verifications (normalized_phone, created_at DESC);
