CREATE TABLE user_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    domain TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    title TEXT NOT NULL,
    data JSONB NOT NULL DEFAULT '{}'::jsonb,
    occurred_at TIMESTAMPTZ NOT NULL,
    source TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_records_domain_not_empty CHECK (length(btrim(domain)) > 0),
    CONSTRAINT user_records_entity_type_not_empty CHECK (length(btrim(entity_type)) > 0),
    CONSTRAINT user_records_title_not_empty CHECK (length(btrim(title)) > 0),
    CONSTRAINT user_records_source_not_empty CHECK (length(btrim(source)) > 0)
);
