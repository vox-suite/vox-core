CREATE TABLE action_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    action_id UUID NOT NULL REFERENCES actions(id) ON DELETE CASCADE,
    attempt_number INTEGER NOT NULL,
    state TEXT NOT NULL,
    error_code TEXT,
    provider_metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT action_attempts_action_number_key UNIQUE (action_id, attempt_number),
    CONSTRAINT action_attempts_number_valid CHECK (attempt_number > 0),
    CONSTRAINT action_attempts_state_valid CHECK (state IN ('started', 'accepted', 'failed'))
);
