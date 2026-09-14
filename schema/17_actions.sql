CREATE TABLE actions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    event_id UUID REFERENCES events(id) ON DELETE SET NULL,
    schedule_id UUID REFERENCES scheduled_tasks(id) ON DELETE SET NULL,
    task_id UUID REFERENCES tasks(id) ON DELETE SET NULL,
    target_device_id UUID REFERENCES client_devices(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    idempotency_key TEXT NOT NULL,
    provider_call_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT actions_kind_valid CHECK (kind IN ('outbound_call', 'client_command', 'user_notification')),
    CONSTRAINT actions_state_valid CHECK (state IN ('pending', 'in_progress', 'succeeded', 'failed')),
    CONSTRAINT actions_idempotency_key_key UNIQUE (idempotency_key)
);
