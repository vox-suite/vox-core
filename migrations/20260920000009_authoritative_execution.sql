CREATE TABLE executions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    approval_id UUID NOT NULL UNIQUE REFERENCES action_approvals(id) ON DELETE RESTRICT,
    proposal_id UUID NOT NULL UNIQUE REFERENCES action_proposals(id) ON DELETE RESTRICT,
    idempotency_key TEXT NOT NULL,
    integration_external_key TEXT NOT NULL,
    capability_external_key TEXT NOT NULL,
    connection_id UUID NOT NULL REFERENCES external_connections(id) ON DELETE RESTRICT,
    execution_identity JSONB NOT NULL,
    policy_snapshot JSONB NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    provider_reference TEXT,
    confirmation_evidence JSONB,
    error_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT executions_idempotency UNIQUE (user_context_id, idempotency_key),
    CONSTRAINT executions_state_valid CHECK (state IN ('pending','dispatching','awaiting_provider_authentication','reconciling','succeeded','failed','cancelled','expired','unknown')),
    CONSTRAINT executions_identity_object CHECK (jsonb_typeof(execution_identity) = 'object'),
    CONSTRAINT executions_policy_object CHECK (jsonb_typeof(policy_snapshot) = 'object'),
    CONSTRAINT executions_success_evidence CHECK ((state <> 'succeeded') OR confirmation_evidence IS NOT NULL),
    CONSTRAINT executions_completed_shape CHECK ((state IN ('succeeded','failed','cancelled','expired','unknown')) = (completed_at IS NOT NULL))
);
CREATE INDEX executions_context_state_idx ON executions (user_context_id, state, created_at DESC);

CREATE TABLE execution_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    execution_id UUID NOT NULL REFERENCES executions(id) ON DELETE RESTRICT,
    attempt_number INTEGER NOT NULL DEFAULT 1,
    state TEXT NOT NULL DEFAULT 'pending',
    provider_reference TEXT,
    outcome_evidence JSONB,
    error_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT execution_attempts_unique_number UNIQUE (execution_id, attempt_number),
    CONSTRAINT execution_attempts_single_attempt CHECK (attempt_number = 1),
    CONSTRAINT execution_attempts_state_valid CHECK (state IN ('pending','dispatching','awaiting_provider_authentication','reconciling','succeeded','failed','cancelled','expired','unknown'))
);

DROP TABLE IF EXISTS action_attempts;
DROP TABLE IF EXISTS actions;
ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_kind_valid;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind_valid CHECK (kind IN ('process_event','run_schedule','summarize_conversation','evaluate_task','execute_task'));
