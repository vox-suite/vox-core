-- Restore the accepted Platform V1 context and authority tables removed by
-- the consolidated consumer baseline. Existing consumer tables remain intact.
-- Sources: the immutable accepted migrations listed below at backup/pr52-before-main-rebase-20260923.

-- Source: 20260919000000_canonical_user_contexts.sql
CREATE TABLE platform_deployments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    external_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT platform_deployments_external_key_key UNIQUE (external_key),
    CONSTRAINT platform_deployments_external_key_not_empty
        CHECK (length(btrim(external_key)) BETWEEN 1 AND 255)
);

CREATE TABLE host_apps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT host_apps_deployment_id_id_key UNIQUE (deployment_id, id),
    CONSTRAINT host_apps_deployment_external_key_key UNIQUE (deployment_id, external_key),
    CONSTRAINT host_apps_external_key_not_empty
        CHECK (length(btrim(external_key)) BETWEEN 1 AND 255)
);

CREATE TABLE host_organizations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL,
    host_app_id UUID NOT NULL,
    external_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT host_organizations_host_app_fkey
        FOREIGN KEY (deployment_id, host_app_id)
        REFERENCES host_apps(deployment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT host_organizations_scope_id_key
        UNIQUE (deployment_id, host_app_id, id),
    CONSTRAINT host_organizations_scope_external_key_key
        UNIQUE (deployment_id, host_app_id, external_key),
    CONSTRAINT host_organizations_external_key_not_empty
        CHECK (length(btrim(external_key)) BETWEEN 1 AND 255)
);

CREATE TABLE user_contexts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL,
    host_app_id UUID NOT NULL,
    organization_id UUID,
    host_user_id TEXT NOT NULL,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_contexts_host_app_fkey
        FOREIGN KEY (deployment_id, host_app_id)
        REFERENCES host_apps(deployment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT user_contexts_organization_fkey
        FOREIGN KEY (deployment_id, host_app_id, organization_id)
        REFERENCES host_organizations(deployment_id, host_app_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT user_contexts_user_id_key UNIQUE (user_id),
    CONSTRAINT user_contexts_host_user_id_not_empty
        CHECK (
            length(btrim(host_user_id)) > 0
            AND octet_length(host_user_id) <= 512
        )
);

CREATE UNIQUE INDEX user_contexts_unorganized_subject_key
    ON user_contexts (deployment_id, host_app_id, host_user_id)
    WHERE organization_id IS NULL;

CREATE UNIQUE INDEX user_contexts_organized_subject_key
    ON user_contexts (deployment_id, host_app_id, organization_id, host_user_id)
    WHERE organization_id IS NOT NULL;

CREATE INDEX user_contexts_scope_idx
    ON user_contexts (deployment_id, host_app_id, organization_id);

-- Source: 20260920000000_host_app_trust.sql
-- Host-app credentials authenticate the host assertion, not an end user. The
-- secret is returned only at creation time; the platform retains its SHA-256
-- verifier and never needs to persist the raw secret.
ALTER TABLE host_apps
    ADD COLUMN allowed_origins TEXT[] NOT NULL DEFAULT '{}'::text[];

CREATE TABLE host_app_credentials (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL,
    host_app_id UUID NOT NULL,
    secret_hash BYTEA NOT NULL,
    state TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    CONSTRAINT host_app_credentials_host_app_fkey
        FOREIGN KEY (deployment_id, host_app_id)
        REFERENCES host_apps(deployment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT host_app_credentials_secret_hash_length
        CHECK (octet_length(secret_hash) = 32),
    CONSTRAINT host_app_credentials_state_valid
        CHECK (state IN ('active', 'revoked')),
    CONSTRAINT host_app_credentials_revocation_state
        CHECK (
            (state = 'active' AND revoked_at IS NULL)
            OR (state = 'revoked' AND revoked_at IS NOT NULL)
        )
);

CREATE INDEX host_app_credentials_active_idx
    ON host_app_credentials (id)
    WHERE state = 'active';

-- A signed assertion is single-use inside its short validity window. Keeping
-- only the nonce, credential ID, and expiry makes replay prevention durable
-- without retaining host-user assertions or signing material.
CREATE TABLE host_app_assertion_nonces (
    credential_id UUID NOT NULL REFERENCES host_app_credentials(id) ON DELETE CASCADE,
    nonce UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (credential_id, nonce)
);

CREATE INDEX host_app_assertion_nonces_expiry_idx
    ON host_app_assertion_nonces (expires_at);

-- Source: 20260920000001_identity_adapters.sql
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

-- Source: 20260920000002_agent_registry.sql
CREATE TABLE agent_definitions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    purpose TEXT NOT NULL,
    requested_capability_categories TEXT[] NOT NULL DEFAULT '{}'::text[],
    state TEXT NOT NULL DEFAULT 'enabled',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT agent_definitions_deployment_key_unique UNIQUE (deployment_id, external_key),
    CONSTRAINT agent_definitions_deployment_id_unique UNIQUE (deployment_id, id),
    CONSTRAINT agent_definitions_key_not_empty CHECK (length(btrim(external_key)) BETWEEN 1 AND 255),
    CONSTRAINT agent_definitions_purpose_not_empty CHECK (length(btrim(purpose)) BETWEEN 1 AND 2048),
    CONSTRAINT agent_definitions_state_valid CHECK (state IN ('enabled', 'disabled')),
    CONSTRAINT agent_definitions_capability_categories_size CHECK (cardinality(requested_capability_categories) <= 64)
);

CREATE TABLE agent_model_configurations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_definition_id UUID NOT NULL REFERENCES agent_definitions(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL,
    model_adapter TEXT NOT NULL,
    model TEXT NOT NULL,
    configuration JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT agent_model_configurations_version_unique UNIQUE (agent_definition_id, version),
    CONSTRAINT agent_model_configurations_adapter_not_empty CHECK (length(btrim(model_adapter)) BETWEEN 1 AND 255),
    CONSTRAINT agent_model_configurations_model_not_empty CHECK (length(btrim(model)) BETWEEN 1 AND 255),
    CONSTRAINT agent_model_configurations_configuration_object CHECK (jsonb_typeof(configuration) = 'object')
);

CREATE TABLE deployment_agent_selections (
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    agent_definition_id UUID NOT NULL REFERENCES agent_definitions(id) ON DELETE RESTRICT,
    model_configuration_id UUID NOT NULL REFERENCES agent_model_configurations(id) ON DELETE RESTRICT,
    selected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (deployment_id, agent_definition_id),
    CONSTRAINT deployment_agent_selections_definition_scope_fkey
        FOREIGN KEY (deployment_id, agent_definition_id)
        REFERENCES agent_definitions(deployment_id, id)
        ON DELETE RESTRICT
);

CREATE INDEX agent_definitions_deployment_enabled_idx
    ON agent_definitions (deployment_id, external_key)
    WHERE state = 'enabled';

-- Source: 20260920000003_integration_registry.sql
CREATE TABLE integration_definitions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    protocol TEXT NOT NULL,
    display_name TEXT NOT NULL,
    declaration_version INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'disabled',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT integration_definitions_key_unique UNIQUE (deployment_id, external_key),
    CONSTRAINT integration_definitions_deployment_id_unique UNIQUE (deployment_id, id),
    CONSTRAINT integration_definitions_protocol_valid CHECK (protocol IN ('mcp', 'direct')),
    CONSTRAINT integration_definitions_state_valid CHECK (state IN ('enabled', 'disabled')),
    CONSTRAINT integration_definitions_version_valid CHECK (declaration_version > 0),
    CONSTRAINT integration_definitions_key_not_empty CHECK (length(btrim(external_key)) BETWEEN 1 AND 255),
    CONSTRAINT integration_definitions_name_not_empty CHECK (length(btrim(display_name)) BETWEEN 1 AND 255)
);

CREATE TABLE integration_capability_declarations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    integration_id UUID NOT NULL REFERENCES integration_definitions(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    effect TEXT NOT NULL,
    access_needs TEXT[] NOT NULL DEFAULT '{}'::text[],
    data_recipients TEXT[] NOT NULL DEFAULT '{}'::text[],
    regions TEXT[] NOT NULL DEFAULT '{}'::text[],
    failure_modes TEXT[] NOT NULL DEFAULT '{}'::text[],
    optional_guarantees JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT integration_capabilities_key_unique UNIQUE (integration_id, external_key),
    CONSTRAINT integration_capabilities_effect_valid CHECK (effect IN ('read', 'write', 'mixed')),
    CONSTRAINT integration_capabilities_key_not_empty CHECK (length(btrim(external_key)) BETWEEN 1 AND 255),
    CONSTRAINT integration_capabilities_guarantees_object CHECK (jsonb_typeof(optional_guarantees) = 'object')
);

CREATE INDEX integration_definitions_enabled_idx ON integration_definitions (deployment_id, external_key) WHERE state = 'enabled';

-- Source: 20260920000004_connections.sql
CREATE TABLE external_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    integration_id UUID NOT NULL REFERENCES integration_definitions(id) ON DELETE RESTRICT,
    external_account_hash BYTEA NOT NULL,
    credential_custody TEXT NOT NULL,
    authorization_state TEXT NOT NULL,
    authorized_capabilities TEXT[] NOT NULL DEFAULT '{}'::text[],
    expires_at TIMESTAMPTZ,
    failure_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    CONSTRAINT external_connections_unique_account UNIQUE (user_context_id, integration_id, external_account_hash),
    CONSTRAINT external_connections_account_hash_length CHECK (octet_length(external_account_hash) = 32),
    CONSTRAINT external_connections_custody_valid CHECK (credential_custody IN ('platform_held', 'external_operator')),
    CONSTRAINT external_connections_state_valid CHECK (authorization_state IN ('pending', 'authorized', 'expired', 'revoked', 'cancelled', 'failed')),
    CONSTRAINT external_connections_failure_shape CHECK ((authorization_state = 'failed' AND failure_code IS NOT NULL) OR (authorization_state <> 'failed')),
    CONSTRAINT external_connections_expiry_shape CHECK ((authorization_state = 'authorized') OR expires_at IS NULL)
);
CREATE INDEX external_connections_context_state_idx ON external_connections (user_context_id, authorization_state);

-- Source: 20260920000005_capability_grants.sql
CREATE TABLE agent_capability_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    agent_definition_id UUID NOT NULL REFERENCES agent_definitions(id) ON DELETE RESTRICT,
    connection_id UUID NOT NULL REFERENCES external_connections(id) ON DELETE RESTRICT,
    capability_external_key TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'enabled',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    CONSTRAINT agent_capability_grants_unique_scope
        UNIQUE (user_context_id, agent_definition_id, connection_id, capability_external_key),
    CONSTRAINT agent_capability_grants_capability_not_empty
        CHECK (length(btrim(capability_external_key)) BETWEEN 1 AND 511),
    CONSTRAINT agent_capability_grants_state_valid CHECK (state IN ('enabled', 'revoked')),
    CONSTRAINT agent_capability_grants_revocation_shape
        CHECK ((state = 'revoked' AND revoked_at IS NOT NULL) OR (state = 'enabled' AND revoked_at IS NULL))
);

CREATE INDEX agent_capability_grants_context_agent_idx
    ON agent_capability_grants (user_context_id, agent_definition_id)
    WHERE state = 'enabled';

