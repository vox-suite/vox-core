CREATE TABLE identity_adapters (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    configuration JSONB NOT NULL,
    state TEXT NOT NULL DEFAULT 'enabled',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    disabled_at TIMESTAMPTZ,
    CONSTRAINT identity_adapters_external_key_unique UNIQUE (deployment_id, external_key),
    CONSTRAINT identity_adapters_external_key_not_empty CHECK (length(btrim(external_key)) BETWEEN 1 AND 255),
    CONSTRAINT identity_adapters_kind_valid CHECK (kind IN ('federated_ed25519', 'passwordless_recovery')),
    CONSTRAINT identity_adapters_configuration_object CHECK (jsonb_typeof(configuration) = 'object'),
    CONSTRAINT identity_adapters_state_valid CHECK (state IN ('enabled', 'disabled')),
    CONSTRAINT identity_adapters_disabled_state CHECK (
        (state = 'enabled' AND disabled_at IS NULL)
        OR (state = 'disabled' AND disabled_at IS NOT NULL)
    )
);

CREATE TABLE login_identities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    adapter_id UUID NOT NULL REFERENCES identity_adapters(id) ON DELETE RESTRICT,
    subject_hash BYTEA NOT NULL,
    verified_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_authenticated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT login_identities_subject_hash_length CHECK (octet_length(subject_hash) = 32),
    CONSTRAINT login_identities_unique_subject_in_context UNIQUE (user_context_id, adapter_id, subject_hash)
);

CREATE TABLE federated_identity_nonces (
    adapter_id UUID NOT NULL REFERENCES identity_adapters(id) ON DELETE CASCADE,
    nonce UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (adapter_id, nonce)
);

CREATE INDEX federated_identity_nonces_expiry_idx ON federated_identity_nonces (expires_at);

CREATE TABLE passwordless_recovery_challenges (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    adapter_id UUID NOT NULL REFERENCES identity_adapters(id) ON DELETE RESTRICT,
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    recovery_handle_hash BYTEA NOT NULL,
    code_hash BYTEA NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT passwordless_recovery_handle_hash_length CHECK (octet_length(recovery_handle_hash) = 32),
    CONSTRAINT passwordless_recovery_code_hash_length CHECK (octet_length(code_hash) = 32)
);

CREATE INDEX passwordless_recovery_challenges_active_idx
    ON passwordless_recovery_challenges (adapter_id, user_context_id, expires_at)
    WHERE consumed_at IS NULL;

CREATE TABLE identity_authentication_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    login_identity_id UUID NOT NULL REFERENCES login_identities(id) ON DELETE RESTRICT,
    token_hash BYTEA NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT identity_authentication_sessions_token_hash_length CHECK (octet_length(token_hash) = 32)
);

CREATE INDEX identity_authentication_sessions_active_idx
    ON identity_authentication_sessions (token_hash, expires_at)
    WHERE consumed_at IS NULL;

CREATE TABLE identity_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    left_login_identity_id UUID NOT NULL REFERENCES login_identities(id) ON DELETE RESTRICT,
    right_login_identity_id UUID NOT NULL REFERENCES login_identities(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    removed_at TIMESTAMPTZ,
    CONSTRAINT identity_links_distinct_identities CHECK (left_login_identity_id <> right_login_identity_id),
    CONSTRAINT identity_links_ordered_identities CHECK (left_login_identity_id < right_login_identity_id),
    CONSTRAINT identity_links_pair_unique UNIQUE (left_login_identity_id, right_login_identity_id)
);

CREATE TABLE identity_link_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    link_id UUID NOT NULL REFERENCES identity_links(id) ON DELETE RESTRICT,
    event_kind TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT identity_link_events_kind_valid CHECK (event_kind IN ('linked', 'unlinked'))
);
