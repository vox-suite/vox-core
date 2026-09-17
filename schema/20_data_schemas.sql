CREATE TABLE data_schemas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    description TEXT NOT NULL,
    json_schema JSONB NOT NULL,
    embedding vector(768),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT data_schemas_user_namespace_name_version_key UNIQUE (user_id, namespace, name, version),
    CONSTRAINT data_schemas_namespace_not_empty CHECK (length(btrim(namespace)) > 0),
    CONSTRAINT data_schemas_name_not_empty CHECK (length(btrim(name)) > 0),
    CONSTRAINT data_schemas_description_not_empty CHECK (length(btrim(description)) > 0),
    CONSTRAINT data_schemas_json_schema_is_object CHECK (jsonb_typeof(json_schema) = 'object')
);
