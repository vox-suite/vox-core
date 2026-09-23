CREATE TABLE user_preferences (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    category TEXT NOT NULL,
    preference_key TEXT NOT NULL,
    value JSONB NOT NULL,
    is_sensitive BOOLEAN NOT NULL DEFAULT false,
    confirmed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_preferences_context_key_uniq UNIQUE (user_context_id, preference_key),
    CONSTRAINT user_preferences_sensitive_confirmed CHECK (NOT is_sensitive OR confirmed_at IS NOT NULL)
);

CREATE INDEX user_preferences_context_category_idx ON user_preferences (user_context_id, category);
