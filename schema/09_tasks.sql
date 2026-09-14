CREATE TABLE tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id UUID REFERENCES projects(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    raw_instruction TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    execution_type TEXT NOT NULL DEFAULT 'manual_human',
    feasibility_reasoning TEXT,
    execution_result JSONB NOT NULL DEFAULT '{}'::jsonb,
    due_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT tasks_title_not_empty CHECK (length(btrim(title)) > 0),
    CONSTRAINT tasks_instruction_not_empty CHECK (length(btrim(raw_instruction)) > 0),
    CONSTRAINT tasks_status_valid CHECK (status IN ('pending', 'evaluating', 'executing', 'waiting_user', 'completed', 'failed', 'cancelled')),
    CONSTRAINT tasks_execution_type_valid CHECK (execution_type IN ('autonomous', 'interactive', 'manual_human'))
);
