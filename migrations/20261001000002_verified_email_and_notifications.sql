ALTER TABLE users
    ADD COLUMN verified_email TEXT,
    ADD COLUMN verified_email_at TIMESTAMPTZ;

CREATE TABLE user_notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel TEXT NOT NULL CHECK (channel IN ('email')),
    idempotency_key TEXT NOT NULL,
    subject TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    error_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    sent_at TIMESTAMPTZ,
    CONSTRAINT user_notifications_idempotency_key UNIQUE (user_id, idempotency_key)
);

CREATE INDEX user_notifications_user_recent_idx ON user_notifications (user_id, created_at DESC);
