CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_identities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel TEXT NOT NULL,
    external_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_identities_channel_external_key UNIQUE (channel, external_id),
    CONSTRAINT user_identities_channel_not_empty CHECK (length(btrim(channel)) > 0),
    CONSTRAINT user_identities_external_not_empty CHECK (length(btrim(external_id)) > 0)
);

CREATE TABLE user_profiles (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    facts JSONB NOT NULL DEFAULT '{}'::jsonb,
    persona JSONB NOT NULL DEFAULT '{"tone":"direct","verbosity":"concise","proactivity":"medium","technical_depth":"standard"}'::jsonb,
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_profiles_facts_object CHECK (jsonb_typeof(facts) = 'object'),
    CONSTRAINT user_profiles_persona_object CHECK (jsonb_typeof(persona) = 'object')
);

CREATE TABLE conversations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel TEXT NOT NULL,
    external_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active',
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT conversations_channel_external_key UNIQUE (channel, external_id),
    CONSTRAINT conversations_status_valid CHECK (status IN ('active', 'completed'))
);

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

CREATE TABLE conversation_summaries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    version INTEGER NOT NULL DEFAULT 1,
    recap TEXT NOT NULL,
    profile_updates JSONB NOT NULL DEFAULT '{}'::jsonb,
    commitments JSONB NOT NULL DEFAULT '[]'::jsonb,
    decisions JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT conversation_summaries_conversation_key UNIQUE (conversation_id),
    CONSTRAINT conversation_summaries_version_valid CHECK (version = 1)
);

CREATE TABLE projects (
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

CREATE TABLE tasks (
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

CREATE TABLE user_goals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id UUID REFERENCES projects(id) ON DELETE SET NULL,
    schema_id UUID REFERENCES data_schemas(id) ON DELETE SET NULL,
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

CREATE TABLE user_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    schema_id UUID REFERENCES data_schemas(id) ON DELETE SET NULL,
    domain TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    title TEXT NOT NULL,
    data JSONB NOT NULL DEFAULT '{}'::jsonb,
    embedding vector(768),
    occurred_at TIMESTAMPTZ NOT NULL,
    source TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_records_domain_not_empty CHECK (length(btrim(domain)) > 0),
    CONSTRAINT user_records_entity_type_not_empty CHECK (length(btrim(entity_type)) > 0),
    CONSTRAINT user_records_title_not_empty CHECK (length(btrim(title)) > 0),
    CONSTRAINT user_records_source_not_empty CHECK (length(btrim(source)) > 0)
);

CREATE TABLE user_insights (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    schema_id UUID REFERENCES data_schemas(id) ON DELETE SET NULL,
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

CREATE TABLE client_devices (
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

CREATE TABLE events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    idempotency_key TEXT NOT NULL,
    event_type TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at TIMESTAMPTZ,
    CONSTRAINT events_idempotency_key_key UNIQUE (idempotency_key),
    CONSTRAINT events_type_not_empty CHECK (length(btrim(event_type)) > 0),
    CONSTRAINT events_key_not_empty CHECK (length(btrim(idempotency_key)) > 0)
);

CREATE TABLE scheduled_tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    instruction TEXT NOT NULL,
    schedule_kind TEXT NOT NULL,
    recurrence_expression TEXT,
    timezone TEXT NOT NULL,
    next_run_at TIMESTAMPTZ,
    state TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT scheduled_tasks_kind_valid CHECK (schedule_kind IN ('once', 'recurring')),
    CONSTRAINT scheduled_tasks_state_valid CHECK (state IN ('active', 'paused', 'completed')),
    CONSTRAINT scheduled_tasks_instruction_not_empty CHECK (length(btrim(instruction)) > 0),
    CONSTRAINT scheduled_tasks_recurrence_shape CHECK (
        (schedule_kind = 'once' AND recurrence_expression IS NULL)
        OR (schedule_kind = 'recurring' AND recurrence_expression IS NOT NULL)
    )
);

CREATE TABLE jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL,
    payload_reference_id UUID NOT NULL,
    schedule_id UUID REFERENCES scheduled_tasks(id) ON DELETE CASCADE,
    occurrence_at TIMESTAMPTZ,
    state TEXT NOT NULL DEFAULT 'pending',
    attempt_count INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 5,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_owner TEXT,
    lease_expires_at TIMESTAMPTZ,
    last_error_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT jobs_kind_valid CHECK (kind IN ('process_event', 'run_schedule', 'dispatch_action', 'summarize_conversation', 'evaluate_task', 'execute_task')),
    CONSTRAINT jobs_state_valid CHECK (state IN ('pending', 'running', 'completed', 'failed')),
    CONSTRAINT jobs_attempts_valid CHECK (attempt_count >= 0 AND max_attempts > 0),
    CONSTRAINT schedule_occurrence_shape CHECK (
        (schedule_id IS NULL AND occurrence_at IS NULL)
        OR (schedule_id IS NOT NULL AND occurrence_at IS NOT NULL)
    ),
    CONSTRAINT schedule_occurrence_key UNIQUE (schedule_id, occurrence_at)
);

CREATE TABLE actions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    event_id UUID REFERENCES events(id) ON DELETE SET NULL,
    schedule_id UUID REFERENCES scheduled_tasks(id) ON DELETE SET NULL,
    task_id UUID REFERENCES tasks(id) ON DELETE SET NULL,
    target_device_id UUID REFERENCES client_devices(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    idempotency_key TEXT NOT NULL,
    provider_call_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT actions_kind_valid CHECK (kind IN ('outbound_call', 'client_command', 'user_notification')),
    CONSTRAINT actions_state_valid CHECK (state IN ('pending', 'in_progress', 'succeeded', 'failed')),
    CONSTRAINT actions_idempotency_key_key UNIQUE (idempotency_key)
);

CREATE TABLE action_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    action_id UUID NOT NULL REFERENCES actions(id) ON DELETE CASCADE,
    attempt_number INTEGER NOT NULL,
    state TEXT NOT NULL,
    error_code TEXT,
    provider_metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT action_attempts_action_number_key UNIQUE (action_id, attempt_number),
    CONSTRAINT action_attempts_number_valid CHECK (attempt_number > 0),
    CONSTRAINT action_attempts_state_valid CHECK (state IN ('started', 'accepted', 'failed'))
);

CREATE INDEX jobs_claimable_idx ON jobs (next_attempt_at, created_at)
    WHERE state = 'pending';
CREATE INDEX jobs_expired_lease_idx ON jobs (lease_expires_at)
    WHERE state = 'running';
CREATE UNIQUE INDEX jobs_summary_unique_idx ON jobs (payload_reference_id)
    WHERE kind = 'summarize_conversation';
CREATE INDEX scheduled_tasks_due_idx ON scheduled_tasks (next_run_at)
    WHERE state = 'active';
CREATE INDEX messages_conversation_idx ON messages (conversation_id, sequence_number);
CREATE INDEX summaries_user_created_idx ON conversation_summaries (user_id, created_at DESC);
CREATE INDEX projects_user_status_idx ON projects (user_id, status);
CREATE INDEX tasks_user_status_idx ON tasks (user_id, status);
CREATE INDEX tasks_project_idx ON tasks (project_id);
CREATE INDEX user_goals_user_domain_idx ON user_goals (user_id, domain, status);
CREATE INDEX user_records_domain_idx ON user_records (user_id, domain, occurred_at DESC);
CREATE INDEX user_records_gin_data ON user_records USING gin (data);
CREATE INDEX user_records_user_schema_idx ON user_records (user_id, schema_id, occurred_at DESC);
CREATE INDEX data_schemas_lookup_idx ON data_schemas (user_id, namespace, name);
CREATE INDEX data_schemas_gin_schema ON data_schemas USING gin (json_schema);
CREATE INDEX user_insights_user_idx ON user_insights (user_id, domain, outcome_status);
CREATE INDEX client_devices_user_active_idx ON client_devices (user_id, is_active);
CREATE INDEX actions_user_state_idx ON actions (user_id, state);

CREATE TABLE outbound_calls (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    task_id UUID REFERENCES tasks(id) ON DELETE SET NULL,
    schedule_id UUID REFERENCES scheduled_tasks(id) ON DELETE SET NULL,
    phone_number TEXT NOT NULL,
    reason TEXT NOT NULL,
    opening_instruction TEXT NOT NULL,
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    provider_call_id TEXT,
    state TEXT NOT NULL DEFAULT 'initiated',
    idempotency_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT outbound_calls_phone_not_empty CHECK (length(btrim(phone_number)) > 0),
    CONSTRAINT outbound_calls_state_valid CHECK (state IN ('initiated', 'in_progress', 'completed', 'failed', 'busy', 'no_answer')),
    CONSTRAINT outbound_calls_idempotency_key UNIQUE (idempotency_key)
);

CREATE INDEX outbound_calls_user_created_idx ON outbound_calls (user_id, created_at DESC);
CREATE INDEX outbound_calls_state_idx ON outbound_calls (state, created_at);
