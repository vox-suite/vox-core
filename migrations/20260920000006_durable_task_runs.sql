ALTER TABLE tasks ADD COLUMN cancellation_requested_at TIMESTAMPTZ;

CREATE TABLE task_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
    state TEXT NOT NULL DEFAULT 'queued',
    wait_reason TEXT,
    checkpoint JSONB NOT NULL DEFAULT '{}'::jsonb,
    lease_owner TEXT,
    lease_expires_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT task_runs_state_valid CHECK (state IN ('queued', 'running', 'waiting', 'completed', 'cancelled', 'failed')),
    CONSTRAINT task_runs_wait_reason_valid CHECK (wait_reason IS NULL OR wait_reason IN ('clarification', 'connection', 'approval', 'authentication', 'reconciliation')),
    CONSTRAINT task_runs_wait_shape CHECK ((state = 'waiting') = (wait_reason IS NOT NULL)),
    CONSTRAINT task_runs_checkpoint_object CHECK (jsonb_typeof(checkpoint) = 'object'),
    CONSTRAINT task_runs_lease_shape CHECK ((state = 'running') = (lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL))
);

CREATE UNIQUE INDEX task_runs_one_live_run_per_task
    ON task_runs (task_id) WHERE state IN ('queued', 'running', 'waiting');
CREATE INDEX task_runs_claimable_idx ON task_runs (state, lease_expires_at, created_at)
    WHERE state IN ('queued', 'running');
