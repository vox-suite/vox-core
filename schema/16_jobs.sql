CREATE TABLE jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL,
    payload_reference_id UUID NOT NULL,
    schedule_id UUID REFERENCES scheduled_tasks(id) ON DELETE CASCADE,
    occurrence_at TIMESTAMPTZ,
    state TEXT NOT NULL DEFAULT 'pending',
    attempt_count INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 5,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_owner TEXT,
    lease_expires_at TIMESTAMPTZ,
    last_error_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT jobs_kind_valid CHECK (kind IN ('process_event', 'run_schedule', 'dispatch_action', 'summarize_conversation', 'evaluate_task', 'execute_task')),
    CONSTRAINT jobs_state_valid CHECK (state IN ('pending', 'running', 'completed', 'failed')),
    CONSTRAINT jobs_attempts_valid CHECK (attempt_count >= 0 AND max_attempts > 0),
    CONSTRAINT schedule_occurrence_shape CHECK (
        (schedule_id IS NULL AND occurrence_at IS NULL)
        OR (schedule_id IS NOT NULL AND occurrence_at IS NOT NULL)
    ),
    CONSTRAINT schedule_occurrence_key UNIQUE (schedule_id, occurrence_at)
);
