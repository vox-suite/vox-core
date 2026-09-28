-- An account owner can have several host contexts. External action authority
-- belongs to one context, including the proposal, approval and execution.
ALTER TABLE action_proposals ALTER COLUMN user_context_id SET NOT NULL;
ALTER TABLE action_approvals ALTER COLUMN user_context_id SET NOT NULL;
ALTER TABLE executions ALTER COLUMN user_context_id SET NOT NULL;

ALTER TABLE action_proposals
    ADD CONSTRAINT action_proposals_id_context_key UNIQUE (id, user_context_id);
ALTER TABLE action_approvals
    ADD CONSTRAINT action_approvals_id_context_key UNIQUE (id, user_context_id);

ALTER TABLE action_approvals DROP CONSTRAINT IF EXISTS action_approvals_proposal_id_fkey;
ALTER TABLE action_approvals
    ADD CONSTRAINT action_approvals_proposal_context_fk
    FOREIGN KEY (proposal_id, user_context_id)
    REFERENCES action_proposals(id, user_context_id) ON DELETE CASCADE;

ALTER TABLE executions DROP CONSTRAINT IF EXISTS executions_proposal_id_fkey;
ALTER TABLE executions DROP CONSTRAINT IF EXISTS executions_approval_id_fkey;
ALTER TABLE executions
    ADD CONSTRAINT executions_proposal_context_fk
    FOREIGN KEY (proposal_id, user_context_id)
    REFERENCES action_proposals(id, user_context_id) ON DELETE RESTRICT;
ALTER TABLE executions
    ADD CONSTRAINT executions_approval_context_fk
    FOREIGN KEY (approval_id, user_context_id)
    REFERENCES action_approvals(id, user_context_id) ON DELETE RESTRICT;

ALTER TABLE executions DROP CONSTRAINT IF EXISTS executions_user_idempotency_key;
ALTER TABLE executions
    ADD CONSTRAINT executions_context_idempotency_key UNIQUE (user_context_id, idempotency_key);
