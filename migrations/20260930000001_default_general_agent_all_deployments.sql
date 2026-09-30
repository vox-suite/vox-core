INSERT INTO agent_definitions (deployment_id, external_key, purpose)
SELECT id, 'general', 'General-purpose Vox assistant for voice and messaging conversations.'
FROM platform_deployments
ON CONFLICT (deployment_id, external_key) DO NOTHING;

INSERT INTO agent_model_configurations (agent_definition_id, version, model_adapter, model)
SELECT d.id, 1, 'gemini', 'gemini-3.5-flash-lite'
FROM agent_definitions d
WHERE d.external_key = 'general'
ON CONFLICT (agent_definition_id, version) DO NOTHING;

INSERT INTO deployment_agent_selections (deployment_id, agent_definition_id, model_configuration_id)
SELECT d.deployment_id, d.id, c.id
FROM agent_definitions d
JOIN agent_model_configurations c ON c.agent_definition_id = d.id AND c.version = 1
WHERE d.external_key = 'general'
ON CONFLICT (deployment_id, agent_definition_id) DO NOTHING;
