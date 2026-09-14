CREATE TABLE conversation_summaries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    version INTEGER NOT NULL DEFAULT 1,
    recap TEXT NOT NULL,
    profile_updates JSONB NOT NULL DEFAULT '{}'::jsonb,
    commitments JSONB NOT NULL DEFAULT '[]'::jsonb,
    decisions JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT conversation_summaries_conversation_key UNIQUE (conversation_id),
    CONSTRAINT conversation_summaries_version_valid CHECK (version = 1)
);
