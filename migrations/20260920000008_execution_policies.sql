CREATE TABLE spending_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    capability_external_key TEXT NOT NULL,
    provider_external_key TEXT,
    currency TEXT NOT NULL,
    max_amount_minor BIGINT NOT NULL CHECK (max_amount_minor >= 0),
    policy_version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT spending_policies_currency_format CHECK (currency ~ '^[A-Z]{3}$'),
    CONSTRAINT spending_policies_scope_unique UNIQUE NULLS NOT DISTINCT
        (user_context_id, capability_external_key, provider_external_key, currency)
);
CREATE INDEX spending_policies_context_capability_idx
    ON spending_policies (user_context_id, capability_external_key);

CREATE TABLE operational_quotas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    provider_external_key TEXT NOT NULL,
    model_identifier TEXT NOT NULL,
    external_account_hash BYTEA NOT NULL,
    connection_id UUID NOT NULL REFERENCES external_connections(id) ON DELETE RESTRICT,
    max_attempts INTEGER NOT NULL CHECK (max_attempts > 0),
    reserved_attempts INTEGER NOT NULL DEFAULT 0 CHECK (reserved_attempts >= 0),
    policy_version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT operational_quotas_account_hash_length CHECK (octet_length(external_account_hash) = 32),
    CONSTRAINT operational_quotas_reservation_bound CHECK (reserved_attempts <= max_attempts),
    CONSTRAINT operational_quotas_identity_unique UNIQUE
        (user_context_id, provider_external_key, model_identifier, external_account_hash, connection_id)
);

CREATE TABLE operational_quota_reservations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    quota_id UUID NOT NULL REFERENCES operational_quotas(id) ON DELETE RESTRICT,
    attempt_id UUID NOT NULL UNIQUE,
    approval_id UUID NOT NULL REFERENCES action_approvals(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE execution_policy_decisions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    approval_id UUID NOT NULL REFERENCES action_approvals(id) ON DELETE RESTRICT,
    attempt_id UUID NOT NULL UNIQUE,
    decision TEXT NOT NULL CHECK (decision IN ('constraints_satisfied', 'fresh_proposal_required', 'spending_policy_exceeded', 'quota_exhausted', 'approval_required')),
    policy_snapshot JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX execution_policy_decisions_context_created_idx
    ON execution_policy_decisions (user_context_id, created_at DESC);
