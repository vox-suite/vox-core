CREATE EXTENSION IF NOT EXISTS vector;

ALTER TABLE user_profiles ADD COLUMN IF NOT EXISTS persona JSONB NOT NULL DEFAULT '{"tone":"direct","verbosity":"concise","proactivity":"medium","technical_depth":"standard"}'::jsonb;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'user_profiles_persona_object'
    ) THEN
        ALTER TABLE user_profiles ADD CONSTRAINT user_profiles_persona_object CHECK (jsonb_typeof(persona) = 'object');
    END IF;
END $$;

CREATE TABLE IF NOT EXISTS projects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'active',
    embedding vector(768),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT projects_name_not_empty CHECK (length(btrim(name)) > 0),
    CONSTRAINT projects_status_valid CHECK (status IN ('active', 'paused', 'completed', 'archived'))
);

CREATE TABLE IF NOT EXISTS tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id UUID REFERENCES projects(id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    raw_instruction TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    execution_type TEXT NOT NULL DEFAULT 'manual_human',
    feasibility_reasoning TEXT,
    execution_result JSONB NOT NULL DEFAULT '{}'::jsonb,
    due_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT tasks_title_not_empty CHECK (length(btrim(title)) > 0),
    CONSTRAINT tasks_instruction_not_empty CHECK (length(btrim(raw_instruction)) > 0),
    CONSTRAINT tasks_status_valid CHECK (status IN ('pending', 'evaluating', 'executing', 'waiting_user', 'completed', 'failed', 'cancelled')),
    CONSTRAINT tasks_execution_type_valid CHECK (execution_type IN ('autonomous', 'interactive', 'manual_human'))
);

CREATE TABLE IF NOT EXISTS user_goals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id UUID REFERENCES projects(id) ON DELETE SET NULL,
    domain TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    target_metric JSONB NOT NULL DEFAULT '{}'::jsonb,
    status TEXT NOT NULL DEFAULT 'active',
    target_date TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_goals_domain_not_empty CHECK (length(btrim(domain)) > 0),
    CONSTRAINT user_goals_title_not_empty CHECK (length(btrim(title)) > 0),
    CONSTRAINT user_goals_status_valid CHECK (status IN ('active', 'paused', 'completed', 'abandoned'))
);

CREATE TABLE IF NOT EXISTS user_records (
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

CREATE TABLE IF NOT EXISTS user_insights (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    domain TEXT NOT NULL,
    summary TEXT NOT NULL,
    reasoning TEXT NOT NULL,
    source_record_ids UUID[] NOT NULL DEFAULT '{}'::uuid[],
    outcome_status TEXT NOT NULL DEFAULT 'pending',
    valid_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_insights_domain_not_empty CHECK (length(btrim(domain)) > 0),
    CONSTRAINT user_insights_summary_not_empty CHECK (length(btrim(summary)) > 0),
    CONSTRAINT user_insights_reasoning_not_empty CHECK (length(btrim(reasoning)) > 0),
    CONSTRAINT user_insights_outcome_status_valid CHECK (outcome_status IN ('pending', 'notified', 'acknowledged', 'resolved', 'dismissed'))
);

CREATE TABLE IF NOT EXISTS client_devices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_identifier TEXT NOT NULL,
    platform TEXT NOT NULL,
    device_name TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    telemetry JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT client_devices_user_identifier_key UNIQUE (user_id, device_identifier),
    CONSTRAINT client_devices_identifier_not_empty CHECK (length(btrim(device_identifier)) > 0),
    CONSTRAINT client_devices_platform_not_empty CHECK (length(btrim(platform)) > 0)
);

ALTER TABLE actions ADD COLUMN IF NOT EXISTS task_id UUID REFERENCES tasks(id) ON DELETE SET NULL;
ALTER TABLE actions ADD COLUMN IF NOT EXISTS target_device_id UUID REFERENCES client_devices(id) ON DELETE SET NULL;

ALTER TABLE actions DROP CONSTRAINT IF EXISTS actions_kind_valid;
ALTER TABLE actions ADD CONSTRAINT actions_kind_valid CHECK (kind IN ('outbound_call', 'client_command', 'user_notification'));

ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_kind_valid;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind_valid CHECK (kind IN ('process_event', 'run_schedule', 'dispatch_action', 'summarize_conversation', 'evaluate_task', 'execute_task'));

CREATE INDEX IF NOT EXISTS projects_user_status_idx ON projects (user_id, status);
CREATE INDEX IF NOT EXISTS tasks_user_status_idx ON tasks (user_id, status);
CREATE INDEX IF NOT EXISTS tasks_project_idx ON tasks (project_id);
CREATE INDEX IF NOT EXISTS user_goals_user_domain_idx ON user_goals (user_id, domain, status);
CREATE INDEX IF NOT EXISTS user_records_domain_idx ON user_records (user_id, domain, occurred_at DESC);
CREATE INDEX IF NOT EXISTS user_records_gin_data ON user_records USING gin (data);
CREATE INDEX IF NOT EXISTS user_insights_user_idx ON user_insights (user_id, domain, outcome_status);
CREATE INDEX IF NOT EXISTS client_devices_user_active_idx ON client_devices (user_id, is_active);
