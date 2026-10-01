CREATE TABLE space_messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    space_id UUID NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system')),
    text TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX space_messages_space_created_idx ON space_messages (space_id, created_at);

ALTER TABLE spaces
    ADD COLUMN run_state TEXT NOT NULL DEFAULT 'idle' CHECK (run_state IN ('idle', 'running', 'failed')),
    ADD COLUMN run_error TEXT;
