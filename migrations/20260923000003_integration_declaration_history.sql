CREATE TABLE integration_declaration_versions (
    integration_id UUID NOT NULL REFERENCES integration_definitions(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL CHECK (version > 0),
    declaration JSONB NOT NULL CHECK (jsonb_typeof(declaration) = 'object'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (integration_id, version)
);

-- Earlier overwritten versions cannot be reconstructed. Preserve the current
-- declaration at migration time so subsequent revisions have a durable base.
INSERT INTO integration_declaration_versions (integration_id, version, declaration, created_at)
SELECT i.id, i.declaration_version,
       jsonb_build_object(
           'deployment_external_key', d.external_key,
           'external_key', i.external_key,
           'protocol', i.protocol,
           'display_name', i.display_name,
           'declaration_version', i.declaration_version,
           'capabilities', COALESCE(
               jsonb_agg(jsonb_build_object(
                   'external_key', c.external_key,
                   'effect', c.effect,
                   'access_needs', c.access_needs,
                   'data_recipients', c.data_recipients,
                   'regions', c.regions,
                   'failure_modes', c.failure_modes,
                   'optional_guarantees', c.optional_guarantees
               ) ORDER BY c.external_key) FILTER (WHERE c.id IS NOT NULL),
               '[]'::jsonb
           )
       ), now()
FROM integration_definitions i
JOIN platform_deployments d ON d.id = i.deployment_id
LEFT JOIN integration_capability_declarations c ON c.integration_id = i.id
GROUP BY i.id, d.external_key;
