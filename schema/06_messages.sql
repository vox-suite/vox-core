CREATE TABLE messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    sequence_number BIGINT NOT NULL,
    role TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT messages_conversation_sequence_key UNIQUE (conversation_id, sequence_number),
    CONSTRAINT messages_role_valid CHECK (role IN ('user', 'assistant', 'system')),
    CONSTRAINT messages_text_not_empty CHECK (length(btrim(text)) > 0)
);
