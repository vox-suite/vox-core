-- Conversations retain the selected agent; switching agents starts a new conversation.
ALTER TABLE conversations ADD COLUMN agent_external_key TEXT NOT NULL DEFAULT 'general';
ALTER TABLE conversations ALTER COLUMN agent_external_key DROP DEFAULT;
