-- Agent facts do not inherit the former user-wide profile.
CREATE TABLE agent_memories (
    user_context_id uuid NOT NULL,
    agent_id uuid NOT NULL,
    facts jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(facts)='object' AND octet_length(facts::text)<=8192),
    version bigint NOT NULL DEFAULT 1 CHECK(version>0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(user_context_id,agent_id),
    FOREIGN KEY(user_context_id,agent_id) REFERENCES agent_definitions(owner_user_context_id,id) ON DELETE CASCADE
);
CREATE INDEX conversations_agent_memory ON conversations(user_context_id,agent_external_key,updated_at DESC) WHERE summary_version>0;
