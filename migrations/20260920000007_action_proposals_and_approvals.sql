CREATE TABLE action_proposals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    task_id UUID NOT NULL REFERENCES tasks(id) ON DELETE RESTRICT,
    task_run_id UUID NOT NULL REFERENCES task_runs(id) ON DELETE RESTRICT,
    agent_definition_id UUID NOT NULL REFERENCES agent_definitions(id) ON DELETE RESTRICT,
    capability_external_key TEXT NOT NULL,
    details JSONB NOT NULL,
    details_hash BYTEA NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT action_proposals_state_valid CHECK (state IN ('pending','approved','superseded','expired','consumed')),
    CONSTRAINT action_proposals_details_object CHECK (jsonb_typeof(details) = 'object'),
    CONSTRAINT action_proposals_hash_length CHECK (octet_length(details_hash) = 32)
);
CREATE INDEX action_proposals_context_state_idx ON action_proposals (user_context_id, state, expires_at);
CREATE UNIQUE INDEX action_proposals_task_run_live_idx ON action_proposals (task_run_id) WHERE state IN ('pending','approved');

CREATE TABLE action_approvals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    proposal_id UUID NOT NULL UNIQUE REFERENCES action_proposals(id) ON DELETE RESTRICT,
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    host_app_id UUID NOT NULL REFERENCES host_apps(id) ON DELETE RESTRICT,
    proposal_hash BYTEA NOT NULL,
    approved_at TIMESTAMPTZ NOT NULL,
    consumed_attempt_id UUID UNIQUE,
    consumed_at TIMESTAMPTZ,
    CONSTRAINT action_approvals_hash_length CHECK (octet_length(proposal_hash) = 32),
    CONSTRAINT action_approvals_consumption_shape CHECK ((consumed_attempt_id IS NULL) = (consumed_at IS NULL))
);
