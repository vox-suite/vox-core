CREATE TABLE IF NOT EXISTS data_schemas (
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

CREATE INDEX IF NOT EXISTS data_schemas_lookup_idx ON data_schemas (user_id, namespace, name);
CREATE INDEX IF NOT EXISTS data_schemas_gin_schema ON data_schemas USING gin (json_schema);

ALTER TABLE user_records ADD COLUMN IF NOT EXISTS schema_id UUID REFERENCES data_schemas(id) ON DELETE SET NULL;
ALTER TABLE user_records ADD COLUMN IF NOT EXISTS embedding vector(768);
CREATE INDEX IF NOT EXISTS user_records_user_schema_idx ON user_records (user_id, schema_id, occurred_at DESC);

ALTER TABLE user_goals ADD COLUMN IF NOT EXISTS schema_id UUID REFERENCES data_schemas(id) ON DELETE SET NULL;
ALTER TABLE user_insights ADD COLUMN IF NOT EXISTS schema_id UUID REFERENCES data_schemas(id) ON DELETE SET NULL;

INSERT INTO data_schemas (user_id, namespace, name, version, description, json_schema)
VALUES
    (
        NULL,
        'finance',
        'transaction',
        1,
        'Personal financial transactions, expenses, incomes, and transfers',
        '{
            "type": "object",
            "properties": {
                "amount": { "type": "number", "description": "Transaction value" },
                "currency": { "type": "string", "description": "ISO currency code (e.g. USD, EUR, INR)" },
                "category": { "type": "string", "description": "Expense or income category" },
                "merchant": { "type": "string", "description": "Vendor or recipient" },
                "payment_method": { "type": "string", "description": "Payment instrument" },
                "notes": { "type": "string", "description": "Optional notes or tags" }
            },
            "required": ["amount", "currency"]
        }'::jsonb
    ),
    (
        NULL,
        'location',
        'breadcrumb',
        1,
        'Historical geographic coordinates and venue pings from client devices',
        '{
            "type": "object",
            "properties": {
                "latitude": { "type": "number", "description": "Latitude in decimal degrees" },
                "longitude": { "type": "number", "description": "Longitude in decimal degrees" },
                "place_name": { "type": "string", "description": "Resolved venue or place name" },
                "accuracy_meters": { "type": "number", "description": "Accuracy radius in meters" },
                "activity": { "type": "string", "description": "Observed activity: stationary, walking, running, driving, cycling" }
            },
            "required": ["latitude", "longitude"]
        }'::jsonb
    ),
    (
        NULL,
        'health',
        'vitals',
        1,
        'Health metrics, heart rate, sleep, workouts, and vitals from mobile and wearable sensors',
        '{
            "type": "object",
            "properties": {
                "metric_type": { "type": "string", "description": "heart_rate, steps, sleep, blood_pressure, workout, weight" },
                "value": { "type": "number", "description": "Primary measurement value" },
                "unit": { "type": "string", "description": "Unit of measurement (e.g. bpm, count, minutes, mmHg, kg)" },
                "details": { "type": "object", "description": "Sensor-specific payload or breakdown" }
            },
            "required": ["metric_type", "value", "unit"]
        }'::jsonb
    ),
    (
        NULL,
        'activity',
        'log',
        1,
        'Client device application usage, focus sessions, window titles, and desktop/mobile events',
        '{
            "type": "object",
            "properties": {
                "activity_type": { "type": "string", "description": "app_usage, web_browsing, meeting, focus_session, reading" },
                "application_name": { "type": "string", "description": "Active application or process" },
                "duration_seconds": { "type": "integer", "description": "Duration in seconds" },
                "context": { "type": "string", "description": "Active window title, URL, or document title" }
            },
            "required": ["activity_type"]
        }'::jsonb
    )
ON CONFLICT DO NOTHING;
