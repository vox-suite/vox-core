CREATE TABLE events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL,
    event_type TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at TIMESTAMPTZ,
    CONSTRAINT events_idempotency_key_key UNIQUE (idempotency_key),
    CONSTRAINT events_type_not_empty CHECK (length(btrim(event_type)) > 0),
    CONSTRAINT events_key_not_empty CHECK (length(btrim(idempotency_key)) > 0)
);
