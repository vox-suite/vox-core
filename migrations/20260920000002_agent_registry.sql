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
