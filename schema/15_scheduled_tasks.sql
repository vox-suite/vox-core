CREATE TABLE scheduled_tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    instruction TEXT NOT NULL,
    schedule_kind TEXT NOT NULL,
    recurrence_expression TEXT,
    timezone TEXT NOT NULL,
    next_run_at TIMESTAMPTZ,
    state TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT scheduled_tasks_kind_valid CHECK (schedule_kind IN ('once', 'recurring')),
    CONSTRAINT scheduled_tasks_state_valid CHECK (state IN ('active', 'paused', 'completed')),
    CONSTRAINT scheduled_tasks_instruction_not_empty CHECK (length(btrim(instruction)) > 0),
    CONSTRAINT scheduled_tasks_recurrence_shape CHECK (
        (schedule_kind = 'once' AND recurrence_expression IS NULL)
        OR (schedule_kind = 'recurring' AND recurrence_expression IS NOT NULL)
    )
);
