-- Deployment definitions are templates; only context-owned instances hold access.
ALTER TABLE user_contexts ADD CONSTRAINT user_contexts_deployment_id_unique UNIQUE (deployment_id,id);
ALTER TABLE agent_definitions
    ADD COLUMN owner_user_context_id uuid,
    ADD COLUMN template_id uuid REFERENCES agent_definitions(id) ON DELETE RESTRICT,
    ADD COLUMN display_name text NOT NULL DEFAULT 'Assistant',
    ADD COLUMN is_default boolean NOT NULL DEFAULT false,
    ADD COLUMN instruction_version integer NOT NULL DEFAULT 1,
    ADD CONSTRAINT agent_template_scope FOREIGN KEY(deployment_id,template_id) REFERENCES agent_definitions(deployment_id,id),
    ADD CONSTRAINT agent_owner_scope FOREIGN KEY (deployment_id,owner_user_context_id)
        REFERENCES user_contexts(deployment_id,id),
    ADD CONSTRAINT agent_owner_id UNIQUE(owner_user_context_id,id),
    ADD CONSTRAINT agent_default_owned CHECK (NOT is_default OR (owner_user_context_id IS NOT NULL AND state='enabled')),
    ADD CONSTRAINT agent_name_valid CHECK (length(btrim(display_name)) BETWEEN 1 AND 100);
ALTER TABLE agent_definitions DROP CONSTRAINT agent_definitions_deployment_key_unique;
CREATE UNIQUE INDEX agent_template_key ON agent_definitions(deployment_id,external_key) WHERE owner_user_context_id IS NULL;
CREATE UNIQUE INDEX agent_owned_key ON agent_definitions(owner_user_context_id,external_key) WHERE owner_user_context_id IS NOT NULL;
CREATE UNIQUE INDEX agent_owned_default ON agent_definitions(owner_user_context_id) WHERE is_default;

-- Pre-launch template-based permissions cannot be transferred as implied consent.
DELETE FROM agent_capability_grants;
DELETE FROM skill_agent_enablements;
ALTER TABLE agent_capability_grants ADD CONSTRAINT grant_owned_agent
    FOREIGN KEY(user_context_id,agent_definition_id) REFERENCES agent_definitions(owner_user_context_id,id);
ALTER TABLE skill_agent_enablements ADD CONSTRAINT skill_owned_agent
    FOREIGN KEY(user_context_id,agent_definition_id) REFERENCES agent_definitions(owner_user_context_id,id);

CREATE TABLE agent_instruction_versions (
    agent_id uuid NOT NULL REFERENCES agent_definitions(id) ON DELETE RESTRICT,
    version integer NOT NULL CHECK(version>0),
    instructions text NOT NULL CHECK(length(btrim(instructions)) BETWEEN 1 AND 2048),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(agent_id,version)
);

UPDATE agent_definitions SET requested_capability_categories=ARRAY['*'] WHERE owner_user_context_id IS NULL AND external_key='general';
