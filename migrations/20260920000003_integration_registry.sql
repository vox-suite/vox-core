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
