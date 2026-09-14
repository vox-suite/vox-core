CREATE TABLE user_profiles (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    facts JSONB NOT NULL DEFAULT '{}'::jsonb,
    persona JSONB NOT NULL DEFAULT '{"tone":"direct","verbosity":"concise","proactivity":"medium","technical_depth":"standard"}'::jsonb,
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_profiles_facts_object CHECK (jsonb_typeof(facts) = 'object'),
    CONSTRAINT user_profiles_persona_object CHECK (jsonb_typeof(persona) = 'object')
);
