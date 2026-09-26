-- Installing a skill never exposes its instructions to every agent.
CREATE TABLE skill_agent_enablements (
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    skill_id UUID NOT NULL REFERENCES skill_packages(id) ON DELETE RESTRICT,
    agent_definition_id UUID NOT NULL REFERENCES agent_definitions(id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT true,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_context_id, skill_id, agent_definition_id),
    FOREIGN KEY (user_context_id, skill_id)
        REFERENCES skill_installations(user_context_id, skill_id) ON DELETE CASCADE
);
