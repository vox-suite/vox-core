CREATE TABLE IF NOT EXISTS outbound_calls (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID REFERENCES user_contexts(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    task_id UUID REFERENCES tasks(id) ON DELETE SET NULL,
    schedule_id UUID REFERENCES scheduled_tasks(id) ON DELETE SET NULL,
    phone_number TEXT NOT NULL,
    reason TEXT NOT NULL,
    opening_instruction TEXT NOT NULL,
    conversation_id UUID NOT NULL,
    provider_call_id TEXT,
    state TEXT NOT NULL DEFAULT 'initiated',
    idempotency_key TEXT UNIQUE NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT outbound_calls_state_valid CHECK (state IN ('pending', 'initiated', 'in_progress', 'completed', 'failed'))
);

CREATE INDEX IF NOT EXISTS outbound_calls_user_idx ON outbound_calls (user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS outbound_calls_state_idx ON outbound_calls (state, created_at);
