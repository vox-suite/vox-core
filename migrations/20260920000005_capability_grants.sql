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
