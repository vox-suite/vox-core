CREATE TABLE spending_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    capability_external_key TEXT NOT NULL,
    provider_external_key TEXT NOT NULL,
    currency TEXT NOT NULL,
    max_amount_minor BIGINT NOT NULL CHECK (max_amount_minor >= 0),
    version INTEGER NOT NULL DEFAULT 1,
    UNIQUE (user_context_id, capability_external_key, provider_external_key, currency)
);

CREATE TABLE operational_quotas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    provider_external_key TEXT NOT NULL,
    model_identifier TEXT NOT NULL,
    account_hash BYTEA NOT NULL,
    connection_id UUID NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    max_attempts INTEGER NOT NULL CHECK (max_attempts > 0),
    reserved_attempts INTEGER NOT NULL DEFAULT 0 CHECK (reserved_attempts >= 0),
    version INTEGER NOT NULL DEFAULT 1,
    UNIQUE (user_context_id, provider_external_key, model_identifier, account_hash, connection_id)
);

CREATE TABLE operational_quota_reservations (
    attempt_id UUID PRIMARY KEY,
    quota_id UUID NOT NULL REFERENCES operational_quotas(id) ON DELETE CASCADE,
    approval_id UUID NOT NULL REFERENCES action_approvals(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
