-- Export data is sensitive and scoped to its originating user context.
CREATE TABLE IF NOT EXISTS portable_exports (
    id UUID PRIMARY KEY,
    user_context_id UUID NOT NULL,
    categories TEXT[] NOT NULL,
    bundle_data JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);
DELETE FROM portable_exports e WHERE NOT EXISTS (
    SELECT 1 FROM user_contexts c WHERE c.id=e.user_context_id
);
ALTER TABLE portable_exports ADD CONSTRAINT portable_exports_context_fk
    FOREIGN KEY (user_context_id) REFERENCES user_contexts(id) ON DELETE CASCADE;
CREATE INDEX portable_exports_expiry_idx ON portable_exports (expires_at);
