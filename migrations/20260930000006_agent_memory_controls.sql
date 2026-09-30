ALTER TABLE agent_memories
    ADD COLUMN retention_enabled boolean NOT NULL DEFAULT true,
    ADD COLUMN cleared_at timestamptz NOT NULL DEFAULT '1970-01-01 00:00:00+00';
