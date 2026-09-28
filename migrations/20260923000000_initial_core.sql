-- ============================================================================
-- Vox Core Target Schema (Clean 21-Table Consumer Baseline)
-- ============================================================================
-- Fresh install baseline for Vox Core consumer architecture.
-- Replaces multi-company legacy tables with 21 core relational tables,
-- JSONB dynamic data payloads, and explicit constraints.

-- Extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS vector;

-- 1. users: Canonical user identity and bounded profile facts
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('provisional', 'active', 'disabled')),
    display_name TEXT,
    preferences JSONB NOT NULL DEFAULT '{}'::jsonb,
    profile_facts JSONB NOT NULL DEFAULT '{}'::jsonb,
    persona JSONB NOT NULL DEFAULT '{"tone":"direct","verbosity":"concise","proactivity":"medium","technical_depth":"standard"}'::jsonb,
    profile_version BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT users_preferences_is_object CHECK (jsonb_typeof(preferences) = 'object'),
    CONSTRAINT users_profile_facts_is_object CHECK (jsonb_typeof(profile_facts) = 'object'),
    CONSTRAINT users_persona_is_object CHECK (jsonb_typeof(persona) = 'object')
);

-- 2. auth_identities: Consumer authentication accounts (e.g. Google OIDC)
CREATE TABLE auth_identities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    issuer TEXT NOT NULL,
    subject TEXT NOT NULL,
    profile_metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    verified_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT auth_identities_issuer_subject_key UNIQUE (issuer, subject),
    CONSTRAINT auth_identities_issuer_not_empty CHECK (length(btrim(issuer)) > 0),
    CONSTRAINT auth_identities_subject_not_empty CHECK (length(btrim(subject)) > 0)
);

-- 3. channel_identities: Contact channels (phone, WhatsApp, voice)
CREATE TABLE channel_identities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel TEXT NOT NULL,
    provider_scope TEXT NOT NULL DEFAULT 'global',
    normalized_external_id TEXT NOT NULL,
    verified_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT channel_identities_channel_not_empty CHECK (length(btrim(channel)) > 0),
    CONSTRAINT channel_identities_external_not_empty CHECK (length(btrim(normalized_external_id)) > 0)
);

CREATE UNIQUE INDEX channel_identities_active_idx 
    ON channel_identities (channel, provider_scope, normalized_external_id) 
    WHERE revoked_at IS NULL;

-- 4. auth_sessions: Issued bearer sessions with family rotation
CREATE TABLE auth_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    auth_identity_id UUID REFERENCES auth_identities(id) ON DELETE SET NULL,
    device_id UUID, -- FK to devices added after devices table creation
    token_hash TEXT NOT NULL UNIQUE,
    family_id UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT auth_sessions_token_hash_not_empty CHECK (length(btrim(token_hash)) > 0)
);

-- 5. conversations: Channel-scoped conversation sessions
CREATE TABLE conversations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_identity_id UUID REFERENCES channel_identities(id) ON DELETE SET NULL,
    external_conversation_id TEXT NOT NULL,
    channel TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'completed', 'archived')),
    latest_summary JSONB NOT NULL DEFAULT '{}'::jsonb,
    summary_version INTEGER NOT NULL DEFAULT 0,
    summary_through_sequence BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT conversations_channel_external_key UNIQUE (channel, external_conversation_id),
    CONSTRAINT conversations_external_not_empty CHECK (length(btrim(external_conversation_id)) > 0)
);

-- 6. messages: Ordered turns within a conversation
CREATE TABLE messages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    conversation_id UUID NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    sequence_number BIGINT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system')),
    text TEXT NOT NULL CHECK (length(btrim(text)) > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT messages_conversation_sequence_key UNIQUE (conversation_id, sequence_number)
);

-- 7. collections: Reusable groupings (projects, trips, courses, life areas)
CREATE TABLE collections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (length(btrim(name)) > 0),
    description TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT 'project' CHECK (kind IN ('project', 'trip', 'course', 'area')),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'paused', 'completed', 'archived')),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT collections_metadata_is_object CHECK (jsonb_typeof(metadata) = 'object')
);

-- 8. tasks: User tasks and execution intent
CREATE TABLE tasks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    collection_id UUID REFERENCES collections(id) ON DELETE SET NULL,
    title TEXT NOT NULL CHECK (length(btrim(title)) > 0),
    instruction TEXT NOT NULL CHECK (length(btrim(instruction)) > 0),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'evaluating', 'executing', 'waiting_user', 'completed', 'failed', 'cancelled')),
    priority INTEGER NOT NULL DEFAULT 0,
    execution_type TEXT NOT NULL DEFAULT 'manual_human' CHECK (execution_type IN ('autonomous', 'interactive', 'manual_human')),
    feasibility_reasoning TEXT,
    execution_result JSONB NOT NULL DEFAULT '{}'::jsonb,
    due_at TIMESTAMPTZ,
    version INTEGER NOT NULL DEFAULT 1,
    cancellation_requested_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);

-- 9. schedules: Temporal intent (one-off reminders or recurring ticker)
CREATE TABLE schedules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    task_id UUID REFERENCES tasks(id) ON DELETE SET NULL,
    instruction TEXT NOT NULL CHECK (length(btrim(instruction)) > 0),
    kind TEXT NOT NULL CHECK (kind IN ('once', 'recurring')),
    recurrence_expression TEXT,
    timezone TEXT NOT NULL DEFAULT 'UTC',
    next_run_at TIMESTAMPTZ,
    state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'paused', 'completed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT schedules_recurrence_shape CHECK (
        (kind = 'once' AND recurrence_expression IS NULL)
        OR (kind = 'recurring' AND recurrence_expression IS NOT NULL)
    )
);

-- 10. jobs: Unified background execution jobs (one scheduler)
CREATE TABLE jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('process_event', 'run_schedule', 'dispatch_action', 'summarize_conversation', 'evaluate_task', 'execute_task')),
    payload_reference_id UUID,
    task_id UUID REFERENCES tasks(id) ON DELETE SET NULL,
    schedule_id UUID REFERENCES schedules(id) ON DELETE SET NULL,
    source_event_id UUID,
    occurrence_at TIMESTAMPTZ,
    dedupe_key TEXT,
    input_reference TEXT,
    checkpoint JSONB NOT NULL DEFAULT '{}'::jsonb,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'running', 'completed', 'failed', 'cancelled')),
    wait_reason TEXT,
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deadline_at TIMESTAMPTZ,
    max_attempts INTEGER NOT NULL DEFAULT 5 CHECK (max_attempts > 0),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    lease_generation BIGINT NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_expires_at TIMESTAMPTZ,
    execution_policy JSONB NOT NULL DEFAULT '{}'::jsonb,
    assigned_device_id UUID,
    last_error_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);

-- 11. job_attempts: Execution attempt history & fenced leases
CREATE TABLE job_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
    lease_generation BIGINT NOT NULL,
    executor_kind TEXT NOT NULL DEFAULT 'server',
    device_id UUID,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    heartbeat_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    outcome TEXT CHECK (outcome IN ('succeeded', 'failed', 'timed_out', 'cancelled')),
    error_details TEXT,
    result_reference JSONB NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT job_attempts_job_attempt_key UNIQUE (job_id, attempt_number)
);

-- 12. data_schemas: Versioned dynamic JSON schemas
CREATE TABLE data_schemas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    owner_scope TEXT GENERATED ALWAYS AS (COALESCE(user_id::text, 'global')) STORED,
    namespace TEXT NOT NULL CHECK (length(btrim(namespace)) > 0),
    name TEXT NOT NULL CHECK (length(btrim(name)) > 0),
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    description TEXT NOT NULL DEFAULT '',
    json_schema JSONB NOT NULL CHECK (jsonb_typeof(json_schema) = 'object'),
    embedding vector(768),
    state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'deprecated')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT data_schemas_id_scope_key UNIQUE (id, owner_scope),
    CONSTRAINT data_schemas_user_namespace_name_version_key UNIQUE (user_id, namespace, name, version)
);

-- 13. records: Unified fact, goal, and insight storage with JSONB
CREATE TABLE records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    schema_id UUID NOT NULL,
    schema_scope TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'fact' CHECK (kind IN ('fact', 'goal', 'insight')),
    domain TEXT NOT NULL DEFAULT 'general' CHECK (length(btrim(domain)) > 0),
    entity_type TEXT NOT NULL DEFAULT 'record' CHECK (length(btrim(entity_type)) > 0),
    title TEXT NOT NULL CHECK (length(btrim(title)) > 0),
    data JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(data) = 'object'),
    embedding vector(768),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    source TEXT NOT NULL DEFAULT 'unknown',
    source_event_id UUID,
    source_record_ids UUID[] NOT NULL DEFAULT '{}'::uuid[],
    collection_id UUID REFERENCES collections(id) ON DELETE SET NULL,
    valid_until TIMESTAMPTZ,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT records_schema_scope_valid CHECK (schema_scope = 'global' OR schema_scope = user_id::text),
    CONSTRAINT records_schema_fk FOREIGN KEY (schema_id, schema_scope) REFERENCES data_schemas(id, owner_scope) ON DELETE RESTRICT
);

-- 14. devices: User-enrolled client devices (desktop/mobile)
CREATE TABLE devices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_identifier TEXT NOT NULL CHECK (length(btrim(device_identifier)) > 0),
    platform TEXT NOT NULL CHECK (length(btrim(platform)) > 0),
    label TEXT NOT NULL DEFAULT '',
    public_key TEXT,
    capabilities JSONB NOT NULL DEFAULT '{}'::jsonb,
    execution_consent BOOLEAN NOT NULL DEFAULT false,
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT devices_user_identifier_key UNIQUE (user_id, device_identifier)
);

-- Link device_id foreign keys now that devices is declared
ALTER TABLE auth_sessions ADD CONSTRAINT auth_sessions_device_fk FOREIGN KEY (device_id) REFERENCES devices(id) ON DELETE SET NULL;
ALTER TABLE jobs ADD CONSTRAINT jobs_assigned_device_fk FOREIGN KEY (assigned_device_id) REFERENCES devices(id) ON DELETE SET NULL;
ALTER TABLE job_attempts ADD CONSTRAINT job_attempts_device_fk FOREIGN KEY (device_id) REFERENCES devices(id) ON DELETE SET NULL;

-- 15. connections: External integrations (Google, WhatsApp, Slack, etc.)
CREATE TABLE connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider_key TEXT NOT NULL CHECK (length(btrim(provider_key)) > 0),
    catalog_version TEXT NOT NULL DEFAULT 'v1',
    external_account_hash TEXT NOT NULL,
    secret_reference TEXT NOT NULL,
    scopes TEXT[] NOT NULL DEFAULT '{}'::text[],
    allowed_capabilities TEXT[] NOT NULL DEFAULT '{}'::text[],
    authorization_state TEXT NOT NULL DEFAULT 'authorized' CHECK (authorization_state IN ('pending', 'authorized', 'expired', 'revoked')),
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    sync_cursor TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT connections_user_provider_account_key UNIQUE (user_id, provider_key, external_account_hash)
);

-- 16. action_proposals: Proposed external side effects
CREATE TABLE action_proposals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    task_id UUID REFERENCES tasks(id) ON DELETE SET NULL,
    job_id UUID REFERENCES jobs(id) ON DELETE SET NULL,
    actor_key TEXT NOT NULL,
    connection_id UUID REFERENCES connections(id) ON DELETE SET NULL,
    capability TEXT NOT NULL,
    details JSONB NOT NULL CHECK (jsonb_typeof(details) = 'object'),
    details_hash TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'proposed' CHECK (state IN ('proposed', 'approved', 'rejected', 'expired')),
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 17. action_approvals: User explicit approvals
CREATE TABLE action_approvals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    proposal_id UUID NOT NULL UNIQUE REFERENCES action_proposals(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    approved_details_hash TEXT NOT NULL,
    session_evidence JSONB NOT NULL DEFAULT '{}'::jsonb,
    approved_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    consumed_execution_id UUID -- populated atomically upon consumption
);

-- 18. executions: Authorized action execution instances
CREATE TABLE executions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    proposal_id UUID NOT NULL UNIQUE REFERENCES action_proposals(id) ON DELETE RESTRICT,
    approval_id UUID NOT NULL UNIQUE REFERENCES action_approvals(id) ON DELETE RESTRICT,
    connection_id UUID REFERENCES connections(id) ON DELETE SET NULL,
    idempotency_key TEXT NOT NULL,
    provider_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'in_progress', 'succeeded', 'failed', 'reconciling')),
    provider_reference TEXT,
    confirmation_evidence JSONB NOT NULL DEFAULT '{}'::jsonb,
    policy_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT executions_user_idempotency_key UNIQUE (user_id, idempotency_key)
);

ALTER TABLE action_approvals ADD CONSTRAINT action_approvals_consumed_fk FOREIGN KEY (consumed_execution_id) REFERENCES executions(id) ON DELETE SET NULL;

-- 19. execution_attempts: Individual attempts to execute an action
CREATE TABLE execution_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    execution_id UUID NOT NULL REFERENCES executions(id) ON DELETE CASCADE,
    attempt_number INTEGER NOT NULL CHECK (attempt_number > 0),
    request_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('started', 'succeeded', 'failed', 'timeout')),
    provider_reference TEXT,
    error_details TEXT,
    policy_decision_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT execution_attempts_execution_attempt_key UNIQUE (execution_id, attempt_number)
);

-- 20. inbound_events: Immutable raw ingestion observations
CREATE TABLE inbound_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL,
    source_id TEXT NOT NULL,
    external_event_id TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    event_type TEXT NOT NULL CHECK (length(btrim(event_type)) > 0),
    payload_version INTEGER NOT NULL DEFAULT 1,
    occurred_at TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    payload JSONB NOT NULL,
    execution_id UUID REFERENCES executions(id) ON DELETE SET NULL,
    processed_at TIMESTAMPTZ,
    processing_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT inbound_events_source_external_key UNIQUE (source_kind, source_id, external_event_id)
);

ALTER TABLE jobs ADD CONSTRAINT jobs_source_event_fk FOREIGN KEY (source_event_id) REFERENCES inbound_events(id) ON DELETE SET NULL;
ALTER TABLE records ADD CONSTRAINT records_source_event_fk FOREIGN KEY (source_event_id) REFERENCES inbound_events(id) ON DELETE SET NULL;

-- 21. audit_events: Append-only sensitive system and security audit log
CREATE TABLE audit_events (
    cursor_id BIGSERIAL PRIMARY KEY,
    id UUID NOT NULL DEFAULT gen_random_uuid() UNIQUE,
    user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    actor TEXT NOT NULL,
    event_type TEXT NOT NULL,
    affected_ids JSONB NOT NULL DEFAULT '[]'::jsonb,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    schema_version INTEGER NOT NULL DEFAULT 1
);

-- ============================================================================
-- Indexes for High Performance
-- ============================================================================
CREATE INDEX jobs_queued_idx ON jobs (available_at, id) WHERE state = 'pending';
CREATE INDEX jobs_lease_recovery_idx ON jobs (lease_expires_at) WHERE state = 'running';
CREATE INDEX jobs_user_kind_idx ON jobs (user_id, kind, state);

CREATE INDEX schedules_active_idx ON schedules (next_run_at) WHERE state = 'active';

CREATE INDEX messages_conv_seq_idx ON messages (conversation_id, sequence_number);

CREATE INDEX tasks_user_status_idx ON tasks (user_id, status, due_at, id);
CREATE INDEX tasks_collection_idx ON tasks (collection_id);

CREATE INDEX collections_user_kind_idx ON collections (user_id, kind, status);

CREATE INDEX records_user_schema_idx ON records (user_id, schema_id, occurred_at DESC, id);
CREATE INDEX records_user_domain_idx ON records (user_id, domain, entity_type, occurred_at DESC);
CREATE INDEX records_gin_data ON records USING gin (data);

CREATE INDEX data_schemas_lookup_idx ON data_schemas (user_id, namespace, name);

CREATE INDEX devices_user_active_idx ON devices (user_id, is_active);

CREATE INDEX inbound_events_user_processed_idx ON inbound_events (user_id, processed_at) WHERE processed_at IS NULL;
CREATE INDEX audit_events_user_time_idx ON audit_events (user_id, occurred_at DESC);

-- ============================================================================
-- Compatibility Views for Seamless Transition
-- ============================================================================
CREATE OR REPLACE VIEW user_records AS
SELECT 
    id, user_id, schema_id, domain, entity_type, title, data, embedding, occurred_at, source, created_at, updated_at
FROM records
WHERE kind = 'fact';

CREATE OR REPLACE VIEW user_goals AS
SELECT 
    id, user_id, collection_id AS project_id, schema_id, domain, title, 
    COALESCE(data->>'description', '') AS description,
    COALESCE(data->'target_metric', '{}'::jsonb) AS target_metric,
    CASE 
        WHEN valid_until IS NOT NULL AND valid_until < now() THEN 'completed'
        ELSE 'active'
    END AS status,
    valid_until AS target_date,
    created_at, updated_at
FROM records
WHERE kind = 'goal';

CREATE OR REPLACE VIEW user_insights AS
SELECT 
    id, user_id, schema_id, domain, title AS summary,
    COALESCE(data->>'reasoning', '') AS reasoning,
    source_record_ids,
    COALESCE(data->>'outcome_status', 'pending') AS outcome_status,
    valid_until, created_at
FROM records
WHERE kind = 'insight';

CREATE OR REPLACE VIEW events AS
SELECT 
    id, user_id, external_event_id AS idempotency_key, event_type, occurred_at, payload, created_at, processed_at
FROM inbound_events;

CREATE OR REPLACE VIEW scheduled_tasks AS
SELECT 
    id, user_id, instruction, kind AS schedule_kind, recurrence_expression, timezone, next_run_at, state, created_at, updated_at
FROM schedules;

CREATE OR REPLACE VIEW projects AS
SELECT 
    id, user_id, name, description, status, NULL::vector(768) AS embedding, created_at, updated_at
FROM collections
WHERE kind = 'project';

CREATE OR REPLACE VIEW client_devices AS
SELECT 
    id, user_id, device_identifier, platform, label AS device_name, is_active, last_seen_at, capabilities AS telemetry, created_at, updated_at
FROM devices;

-- ============================================================================
-- Row Level Security (RLS) Configuration
-- ============================================================================

-- Ensure Supabase auth helper exists for standalone/test environments
DO $do$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_proc p 
        JOIN pg_namespace n ON p.pronamespace = n.oid 
        WHERE n.nspname = 'auth' AND p.proname = 'uid'
    ) THEN
        BEGIN
            CREATE SCHEMA IF NOT EXISTS auth;
            EXECUTE $create$
CREATE FUNCTION auth.uid() RETURNS uuid LANGUAGE sql STABLE AS $fn$
SELECT nullif(current_setting('request.jwt.claim.sub', true), '')::uuid;
$fn$;
$create$;
        EXCEPTION WHEN OTHERS THEN
            NULL;
        END;
    END IF;
END
$do$;

-- Ensure default Supabase roles exist (idempotent on Supabase)
DO $do$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'anon') THEN
        CREATE ROLE anon NOLOGIN;
    END IF;
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'authenticated') THEN
        CREATE ROLE authenticated NOLOGIN;
    END IF;
    IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'service_role') THEN
        CREATE ROLE service_role NOLOGIN;
    END IF;
END
$do$;

-- Grants for standard roles
GRANT USAGE ON SCHEMA public TO anon, authenticated, service_role;
GRANT ALL ON ALL TABLES IN SCHEMA public TO service_role;
GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO service_role;
GRANT ALL ON ALL ROUTINES IN SCHEMA public TO service_role;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON TABLES TO service_role;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON SEQUENCES TO service_role;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON ROUTINES TO service_role;

-- Grant selective access to authenticated users (never full DML on security-critical tables)
GRANT SELECT ON ALL TABLES IN SCHEMA public TO authenticated;
GRANT INSERT, UPDATE, DELETE ON 
    conversations, messages, collections, tasks, schedules, 
    records, devices, connections, action_proposals, inbound_events
TO authenticated;
GRANT INSERT, UPDATE ON data_schemas TO authenticated;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO authenticated;

-- Enable RLS on all 21 core tables
ALTER TABLE users ENABLE ROW LEVEL SECURITY;
ALTER TABLE auth_identities ENABLE ROW LEVEL SECURITY;
ALTER TABLE channel_identities ENABLE ROW LEVEL SECURITY;
ALTER TABLE auth_sessions ENABLE ROW LEVEL SECURITY;
ALTER TABLE conversations ENABLE ROW LEVEL SECURITY;
ALTER TABLE messages ENABLE ROW LEVEL SECURITY;
ALTER TABLE collections ENABLE ROW LEVEL SECURITY;
ALTER TABLE tasks ENABLE ROW LEVEL SECURITY;
ALTER TABLE schedules ENABLE ROW LEVEL SECURITY;
ALTER TABLE jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE job_attempts ENABLE ROW LEVEL SECURITY;
ALTER TABLE data_schemas ENABLE ROW LEVEL SECURITY;
ALTER TABLE records ENABLE ROW LEVEL SECURITY;
ALTER TABLE devices ENABLE ROW LEVEL SECURITY;
ALTER TABLE connections ENABLE ROW LEVEL SECURITY;
ALTER TABLE action_proposals ENABLE ROW LEVEL SECURITY;
ALTER TABLE action_approvals ENABLE ROW LEVEL SECURITY;
ALTER TABLE executions ENABLE ROW LEVEL SECURITY;
ALTER TABLE execution_attempts ENABLE ROW LEVEL SECURITY;
ALTER TABLE inbound_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE audit_events ENABLE ROW LEVEL SECURITY;

-- Service role bypass policies (ensures backend/service_role is never blocked)
CREATE POLICY "service_role_users" ON users FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_auth_identities" ON auth_identities FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_channel_identities" ON channel_identities FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_auth_sessions" ON auth_sessions FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_conversations" ON conversations FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_messages" ON messages FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_collections" ON collections FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_tasks" ON tasks FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_schedules" ON schedules FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_jobs" ON jobs FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_job_attempts" ON job_attempts FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_data_schemas" ON data_schemas FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_records" ON records FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_devices" ON devices FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_connections" ON connections FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_action_proposals" ON action_proposals FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_action_approvals" ON action_approvals FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_executions" ON executions FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_execution_attempts" ON execution_attempts FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_inbound_events" ON inbound_events FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "service_role_audit_events" ON audit_events FOR ALL TO service_role USING (true) WITH CHECK (true);

-- Authenticated user policies: users table
CREATE POLICY "users_select_own" ON users FOR SELECT TO authenticated USING (id = auth.uid());
CREATE POLICY "users_update_own" ON users FOR UPDATE TO authenticated USING (id = auth.uid()) WITH CHECK (id = auth.uid());

-- Security-hardened RLS policies for auth & audit tables:
-- auth_identities: Read-only for authenticated user; writes only by service_role
CREATE POLICY "auth_identities_user_select" ON auth_identities FOR SELECT TO authenticated USING (user_id = auth.uid());

-- channel_identities: Read-only for authenticated user; verification/writes only by service_role
CREATE POLICY "channel_identities_user_select" ON channel_identities FOR SELECT TO authenticated USING (user_id = auth.uid());

-- auth_sessions: Read-only for active sessions; session issuance/revocation strictly by service_role
CREATE POLICY "auth_sessions_user_select" ON auth_sessions FOR SELECT TO authenticated USING (user_id = auth.uid() AND revoked_at IS NULL AND expires_at > now());

-- audit_events: Read-only for authenticated user; strictly append-only by service_role
CREATE POLICY "audit_events_user_select" ON audit_events FOR SELECT TO authenticated USING (user_id = auth.uid());

-- Authenticated user policies: direct user-scoped tables
CREATE POLICY "conversations_user_all" ON conversations FOR ALL TO authenticated USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());
CREATE POLICY "collections_user_all" ON collections FOR ALL TO authenticated USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());
CREATE POLICY "tasks_user_all" ON tasks FOR ALL TO authenticated USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());
CREATE POLICY "schedules_user_all" ON schedules FOR ALL TO authenticated USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());
CREATE POLICY "records_user_all" ON records FOR ALL TO authenticated USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());
CREATE POLICY "devices_user_all" ON devices FOR ALL TO authenticated USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());
CREATE POLICY "connections_user_all" ON connections FOR ALL TO authenticated USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());
CREATE POLICY "action_proposals_user_all" ON action_proposals FOR ALL TO authenticated USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());
CREATE POLICY "inbound_events_user_all" ON inbound_events FOR ALL TO authenticated USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());

-- Authenticated user policies: data_schemas (can view system schemas where user_id IS NULL, and manage their own)
CREATE POLICY "data_schemas_user_select" ON data_schemas FOR SELECT TO authenticated USING (user_id IS NULL OR user_id = auth.uid());
CREATE POLICY "data_schemas_user_insert" ON data_schemas FOR INSERT TO authenticated WITH CHECK (user_id = auth.uid());
CREATE POLICY "data_schemas_user_update" ON data_schemas FOR UPDATE TO authenticated USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());
CREATE POLICY "data_schemas_user_delete" ON data_schemas FOR DELETE TO authenticated USING (user_id = auth.uid());

-- Authenticated user policies: messages (scoped by conversation ownership)
CREATE POLICY "messages_user_select" ON messages FOR SELECT TO authenticated USING (
    EXISTS (SELECT 1 FROM conversations c WHERE c.id = messages.conversation_id AND c.user_id = auth.uid())
);
CREATE POLICY "messages_user_insert" ON messages FOR INSERT TO authenticated WITH CHECK (
    EXISTS (SELECT 1 FROM conversations c WHERE c.id = messages.conversation_id AND c.user_id = auth.uid())
);
CREATE POLICY "messages_user_update" ON messages FOR UPDATE TO authenticated USING (
    EXISTS (SELECT 1 FROM conversations c WHERE c.id = messages.conversation_id AND c.user_id = auth.uid())
) WITH CHECK (
    EXISTS (SELECT 1 FROM conversations c WHERE c.id = messages.conversation_id AND c.user_id = auth.uid())
);
CREATE POLICY "messages_user_delete" ON messages FOR DELETE TO authenticated USING (
    EXISTS (SELECT 1 FROM conversations c WHERE c.id = messages.conversation_id AND c.user_id = auth.uid())
);

-- Authenticated user policies: action_approvals & executions (scoped via action_proposals)
CREATE POLICY "action_approvals_user_all" ON action_approvals FOR ALL TO authenticated USING (
    EXISTS (SELECT 1 FROM action_proposals p WHERE p.id = action_approvals.proposal_id AND p.user_id = auth.uid())
) WITH CHECK (
    EXISTS (SELECT 1 FROM action_proposals p WHERE p.id = action_approvals.proposal_id AND p.user_id = auth.uid())
);

CREATE POLICY "executions_user_select" ON executions FOR SELECT TO authenticated USING (
    EXISTS (SELECT 1 FROM action_proposals p WHERE p.id = executions.proposal_id AND p.user_id = auth.uid())
);

CREATE POLICY "execution_attempts_user_select" ON execution_attempts FOR SELECT TO authenticated USING (
    EXISTS (
        SELECT 1 FROM executions e 
        JOIN action_proposals p ON p.id = e.proposal_id 
        WHERE e.id = execution_attempts.execution_id AND p.user_id = auth.uid()
    )
);

-- Security invoker for backward-compatible views (applies RLS to view queries)
ALTER VIEW user_records SET (security_invoker = true);
ALTER VIEW user_goals SET (security_invoker = true);
ALTER VIEW user_insights SET (security_invoker = true);
ALTER VIEW events SET (security_invoker = true);
ALTER VIEW scheduled_tasks SET (security_invoker = true);
ALTER VIEW projects SET (security_invoker = true);
ALTER VIEW client_devices SET (security_invoker = true);

-- Restore the accepted Platform V1 context and authority tables removed by
-- the consolidated consumer baseline. Existing consumer tables remain intact.
-- Sources: the immutable accepted migrations listed below at backup/pr52-before-main-rebase-20260923.

-- Source: 20260919000000_canonical_user_contexts.sql
CREATE TABLE platform_deployments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    external_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT platform_deployments_external_key_key UNIQUE (external_key),
    CONSTRAINT platform_deployments_external_key_not_empty
        CHECK (length(btrim(external_key)) BETWEEN 1 AND 255)
);

CREATE TABLE host_apps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT host_apps_deployment_id_id_key UNIQUE (deployment_id, id),
    CONSTRAINT host_apps_deployment_external_key_key UNIQUE (deployment_id, external_key),
    CONSTRAINT host_apps_external_key_not_empty
        CHECK (length(btrim(external_key)) BETWEEN 1 AND 255)
);

CREATE TABLE host_organizations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL,
    host_app_id UUID NOT NULL,
    external_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT host_organizations_host_app_fkey
        FOREIGN KEY (deployment_id, host_app_id)
        REFERENCES host_apps(deployment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT host_organizations_scope_id_key
        UNIQUE (deployment_id, host_app_id, id),
    CONSTRAINT host_organizations_scope_external_key_key
        UNIQUE (deployment_id, host_app_id, external_key),
    CONSTRAINT host_organizations_external_key_not_empty
        CHECK (length(btrim(external_key)) BETWEEN 1 AND 255)
);

CREATE TABLE user_contexts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL,
    host_app_id UUID NOT NULL,
    organization_id UUID,
    host_user_id TEXT NOT NULL,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_contexts_host_app_fkey
        FOREIGN KEY (deployment_id, host_app_id)
        REFERENCES host_apps(deployment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT user_contexts_organization_fkey
        FOREIGN KEY (deployment_id, host_app_id, organization_id)
        REFERENCES host_organizations(deployment_id, host_app_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT user_contexts_user_id_key UNIQUE (user_id),
    CONSTRAINT user_contexts_host_user_id_not_empty
        CHECK (
            length(btrim(host_user_id)) > 0
            AND octet_length(host_user_id) <= 512
        )
);

CREATE UNIQUE INDEX user_contexts_unorganized_subject_key
    ON user_contexts (deployment_id, host_app_id, host_user_id)
    WHERE organization_id IS NULL;

CREATE UNIQUE INDEX user_contexts_organized_subject_key
    ON user_contexts (deployment_id, host_app_id, organization_id, host_user_id)
    WHERE organization_id IS NOT NULL;

CREATE INDEX user_contexts_scope_idx
    ON user_contexts (deployment_id, host_app_id, organization_id);

-- Source: 20260920000000_host_app_trust.sql
-- Host-app credentials authenticate the host assertion, not an end user. The
-- secret is returned only at creation time; the platform retains its SHA-256
-- verifier and never needs to persist the raw secret.
ALTER TABLE host_apps
    ADD COLUMN allowed_origins TEXT[] NOT NULL DEFAULT '{}'::text[];

CREATE TABLE host_app_credentials (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL,
    host_app_id UUID NOT NULL,
    secret_hash BYTEA NOT NULL,
    state TEXT NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    CONSTRAINT host_app_credentials_host_app_fkey
        FOREIGN KEY (deployment_id, host_app_id)
        REFERENCES host_apps(deployment_id, id)
        ON DELETE RESTRICT,
    CONSTRAINT host_app_credentials_secret_hash_length
        CHECK (octet_length(secret_hash) = 32),
    CONSTRAINT host_app_credentials_state_valid
        CHECK (state IN ('active', 'revoked')),
    CONSTRAINT host_app_credentials_revocation_state
        CHECK (
            (state = 'active' AND revoked_at IS NULL)
            OR (state = 'revoked' AND revoked_at IS NOT NULL)
        )
);

CREATE INDEX host_app_credentials_active_idx
    ON host_app_credentials (id)
    WHERE state = 'active';

-- A signed assertion is single-use inside its short validity window. Keeping
-- only the nonce, credential ID, and expiry makes replay prevention durable
-- without retaining host-user assertions or signing material.
CREATE TABLE host_app_assertion_nonces (
    credential_id UUID NOT NULL REFERENCES host_app_credentials(id) ON DELETE CASCADE,
    nonce UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (credential_id, nonce)
);

CREATE INDEX host_app_assertion_nonces_expiry_idx
    ON host_app_assertion_nonces (expires_at);

-- Source: 20260920000001_identity_adapters.sql
CREATE TABLE identity_adapters (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    configuration JSONB NOT NULL,
    state TEXT NOT NULL DEFAULT 'enabled',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    disabled_at TIMESTAMPTZ,
    CONSTRAINT identity_adapters_external_key_unique UNIQUE (deployment_id, external_key),
    CONSTRAINT identity_adapters_external_key_not_empty CHECK (length(btrim(external_key)) BETWEEN 1 AND 255),
    CONSTRAINT identity_adapters_kind_valid CHECK (kind IN ('federated_ed25519', 'passwordless_recovery')),
    CONSTRAINT identity_adapters_configuration_object CHECK (jsonb_typeof(configuration) = 'object'),
    CONSTRAINT identity_adapters_state_valid CHECK (state IN ('enabled', 'disabled')),
    CONSTRAINT identity_adapters_disabled_state CHECK (
        (state = 'enabled' AND disabled_at IS NULL)
        OR (state = 'disabled' AND disabled_at IS NOT NULL)
    )
);

CREATE TABLE login_identities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    adapter_id UUID NOT NULL REFERENCES identity_adapters(id) ON DELETE RESTRICT,
    subject_hash BYTEA NOT NULL,
    verified_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_authenticated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT login_identities_subject_hash_length CHECK (octet_length(subject_hash) = 32),
    CONSTRAINT login_identities_unique_subject_in_context UNIQUE (user_context_id, adapter_id, subject_hash)
);

CREATE TABLE federated_identity_nonces (
    adapter_id UUID NOT NULL REFERENCES identity_adapters(id) ON DELETE CASCADE,
    nonce UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (adapter_id, nonce)
);

CREATE INDEX federated_identity_nonces_expiry_idx ON federated_identity_nonces (expires_at);

CREATE TABLE passwordless_recovery_challenges (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    adapter_id UUID NOT NULL REFERENCES identity_adapters(id) ON DELETE RESTRICT,
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    recovery_handle_hash BYTEA NOT NULL,
    code_hash BYTEA NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT passwordless_recovery_handle_hash_length CHECK (octet_length(recovery_handle_hash) = 32),
    CONSTRAINT passwordless_recovery_code_hash_length CHECK (octet_length(code_hash) = 32)
);

CREATE INDEX passwordless_recovery_challenges_active_idx
    ON passwordless_recovery_challenges (adapter_id, user_context_id, expires_at)
    WHERE consumed_at IS NULL;

CREATE TABLE identity_authentication_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    login_identity_id UUID NOT NULL REFERENCES login_identities(id) ON DELETE RESTRICT,
    token_hash BYTEA NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT identity_authentication_sessions_token_hash_length CHECK (octet_length(token_hash) = 32)
);

CREATE INDEX identity_authentication_sessions_active_idx
    ON identity_authentication_sessions (token_hash, expires_at)
    WHERE consumed_at IS NULL;

CREATE TABLE identity_links (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    left_login_identity_id UUID NOT NULL REFERENCES login_identities(id) ON DELETE RESTRICT,
    right_login_identity_id UUID NOT NULL REFERENCES login_identities(id) ON DELETE RESTRICT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    removed_at TIMESTAMPTZ,
    CONSTRAINT identity_links_distinct_identities CHECK (left_login_identity_id <> right_login_identity_id),
    CONSTRAINT identity_links_ordered_identities CHECK (left_login_identity_id < right_login_identity_id),
    CONSTRAINT identity_links_pair_unique UNIQUE (left_login_identity_id, right_login_identity_id)
);

CREATE TABLE identity_link_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    link_id UUID NOT NULL REFERENCES identity_links(id) ON DELETE RESTRICT,
    event_kind TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT identity_link_events_kind_valid CHECK (event_kind IN ('linked', 'unlinked'))
);

-- Source: 20260920000002_agent_registry.sql
CREATE TABLE agent_definitions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    purpose TEXT NOT NULL,
    requested_capability_categories TEXT[] NOT NULL DEFAULT '{}'::text[],
    state TEXT NOT NULL DEFAULT 'enabled',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT agent_definitions_deployment_key_unique UNIQUE (deployment_id, external_key),
    CONSTRAINT agent_definitions_deployment_id_unique UNIQUE (deployment_id, id),
    CONSTRAINT agent_definitions_key_not_empty CHECK (length(btrim(external_key)) BETWEEN 1 AND 255),
    CONSTRAINT agent_definitions_purpose_not_empty CHECK (length(btrim(purpose)) BETWEEN 1 AND 2048),
    CONSTRAINT agent_definitions_state_valid CHECK (state IN ('enabled', 'disabled')),
    CONSTRAINT agent_definitions_capability_categories_size CHECK (cardinality(requested_capability_categories) <= 64)
);

CREATE TABLE agent_model_configurations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent_definition_id UUID NOT NULL REFERENCES agent_definitions(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL,
    model_adapter TEXT NOT NULL,
    model TEXT NOT NULL,
    configuration JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT agent_model_configurations_version_unique UNIQUE (agent_definition_id, version),
    CONSTRAINT agent_model_configurations_adapter_not_empty CHECK (length(btrim(model_adapter)) BETWEEN 1 AND 255),
    CONSTRAINT agent_model_configurations_model_not_empty CHECK (length(btrim(model)) BETWEEN 1 AND 255),
    CONSTRAINT agent_model_configurations_configuration_object CHECK (jsonb_typeof(configuration) = 'object')
);

CREATE TABLE deployment_agent_selections (
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    agent_definition_id UUID NOT NULL REFERENCES agent_definitions(id) ON DELETE RESTRICT,
    model_configuration_id UUID NOT NULL REFERENCES agent_model_configurations(id) ON DELETE RESTRICT,
    selected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (deployment_id, agent_definition_id),
    CONSTRAINT deployment_agent_selections_definition_scope_fkey
        FOREIGN KEY (deployment_id, agent_definition_id)
        REFERENCES agent_definitions(deployment_id, id)
        ON DELETE RESTRICT
);

CREATE INDEX agent_definitions_deployment_enabled_idx
    ON agent_definitions (deployment_id, external_key)
    WHERE state = 'enabled';

-- Source: 20260920000003_integration_registry.sql
CREATE TABLE integration_definitions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    protocol TEXT NOT NULL,
    display_name TEXT NOT NULL,
    declaration_version INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'disabled',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT integration_definitions_key_unique UNIQUE (deployment_id, external_key),
    CONSTRAINT integration_definitions_deployment_id_unique UNIQUE (deployment_id, id),
    CONSTRAINT integration_definitions_protocol_valid CHECK (protocol IN ('mcp', 'direct')),
    CONSTRAINT integration_definitions_state_valid CHECK (state IN ('enabled', 'disabled')),
    CONSTRAINT integration_definitions_version_valid CHECK (declaration_version > 0),
    CONSTRAINT integration_definitions_key_not_empty CHECK (length(btrim(external_key)) BETWEEN 1 AND 255),
    CONSTRAINT integration_definitions_name_not_empty CHECK (length(btrim(display_name)) BETWEEN 1 AND 255)
);

CREATE TABLE integration_capability_declarations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    integration_id UUID NOT NULL REFERENCES integration_definitions(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    effect TEXT NOT NULL,
    access_needs TEXT[] NOT NULL DEFAULT '{}'::text[],
    data_recipients TEXT[] NOT NULL DEFAULT '{}'::text[],
    regions TEXT[] NOT NULL DEFAULT '{}'::text[],
    failure_modes TEXT[] NOT NULL DEFAULT '{}'::text[],
    optional_guarantees JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT integration_capabilities_key_unique UNIQUE (integration_id, external_key),
    CONSTRAINT integration_capabilities_effect_valid CHECK (effect IN ('read', 'write', 'mixed')),
    CONSTRAINT integration_capabilities_key_not_empty CHECK (length(btrim(external_key)) BETWEEN 1 AND 255),
    CONSTRAINT integration_capabilities_guarantees_object CHECK (jsonb_typeof(optional_guarantees) = 'object')
);

CREATE INDEX integration_definitions_enabled_idx ON integration_definitions (deployment_id, external_key) WHERE state = 'enabled';

-- Source: 20260920000004_connections.sql
CREATE TABLE external_connections (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    integration_id UUID NOT NULL REFERENCES integration_definitions(id) ON DELETE RESTRICT,
    external_account_hash BYTEA NOT NULL,
    credential_custody TEXT NOT NULL,
    authorization_state TEXT NOT NULL,
    authorized_capabilities TEXT[] NOT NULL DEFAULT '{}'::text[],
    expires_at TIMESTAMPTZ,
    failure_code TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    CONSTRAINT external_connections_unique_account UNIQUE (user_context_id, integration_id, external_account_hash),
    CONSTRAINT external_connections_account_hash_length CHECK (octet_length(external_account_hash) = 32),
    CONSTRAINT external_connections_custody_valid CHECK (credential_custody IN ('platform_held', 'external_operator')),
    CONSTRAINT external_connections_state_valid CHECK (authorization_state IN ('pending', 'authorized', 'expired', 'revoked', 'cancelled', 'failed')),
    CONSTRAINT external_connections_failure_shape CHECK ((authorization_state = 'failed' AND failure_code IS NOT NULL) OR (authorization_state <> 'failed')),
    CONSTRAINT external_connections_expiry_shape CHECK ((authorization_state = 'authorized') OR expires_at IS NULL)
);
CREATE INDEX external_connections_context_state_idx ON external_connections (user_context_id, authorization_state);

-- Source: 20260920000005_capability_grants.sql
CREATE TABLE agent_capability_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    agent_definition_id UUID NOT NULL REFERENCES agent_definitions(id) ON DELETE RESTRICT,
    connection_id UUID NOT NULL REFERENCES external_connections(id) ON DELETE RESTRICT,
    capability_external_key TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'enabled',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    CONSTRAINT agent_capability_grants_unique_scope
        UNIQUE (user_context_id, agent_definition_id, connection_id, capability_external_key),
    CONSTRAINT agent_capability_grants_capability_not_empty
        CHECK (length(btrim(capability_external_key)) BETWEEN 1 AND 511),
    CONSTRAINT agent_capability_grants_state_valid CHECK (state IN ('enabled', 'revoked')),
    CONSTRAINT agent_capability_grants_revocation_shape
        CHECK ((state = 'revoked' AND revoked_at IS NOT NULL) OR (state = 'enabled' AND revoked_at IS NULL))
);

CREATE INDEX agent_capability_grants_context_agent_idx
    ON agent_capability_grants (user_context_id, agent_definition_id)
    WHERE state = 'enabled';


-- E05 expansion over the consolidated consumer schema. The reserved legacy
-- scope records provenance for existing channel-owned users; it is never
-- accepted as proof that a host may act for that user.
INSERT INTO platform_deployments (external_key)
VALUES ('vox.legacy.deployment')
ON CONFLICT (external_key) DO NOTHING;

INSERT INTO host_apps (deployment_id, external_key)
SELECT id, 'vox.legacy.channel-host'
FROM platform_deployments
WHERE external_key = 'vox.legacy.deployment'
ON CONFLICT (deployment_id, external_key) DO NOTHING;

-- Verified first-party sign-in gets its own internal scope. It is not a
-- public host credential or a way to import a legacy channel identity.
INSERT INTO platform_deployments (external_key)
VALUES ('vox.standalone.deployment')
ON CONFLICT (external_key) DO NOTHING;

INSERT INTO host_apps (deployment_id, external_key)
SELECT id, 'vox.standalone.web'
FROM platform_deployments
WHERE external_key = 'vox.standalone.deployment'
ON CONFLICT (deployment_id, external_key) DO NOTHING;

INSERT INTO user_contexts (deployment_id, host_app_id, host_user_id, user_id)
SELECT d.id, h.id, u.id::text, u.id
FROM users u
JOIN platform_deployments d ON d.external_key = 'vox.legacy.deployment'
JOIN host_apps h ON h.deployment_id = d.id
    AND h.external_key = 'vox.legacy.channel-host'
LEFT JOIN user_contexts existing ON existing.user_id = u.id
WHERE existing.id IS NULL
ON CONFLICT (user_id) DO NOTHING;

ALTER TABLE user_contexts
    ADD CONSTRAINT user_contexts_id_user_id_key UNIQUE (id, user_id);

-- Every row carrying a user owner gets a stable context. NULL remains valid
-- only for genuinely global rows (jobs, schemas, events, and audit entries).
ALTER TABLE auth_identities ADD COLUMN user_context_id UUID;
ALTER TABLE channel_identities ADD COLUMN user_context_id UUID;
ALTER TABLE auth_sessions ADD COLUMN user_context_id UUID;
ALTER TABLE conversations ADD COLUMN user_context_id UUID;
ALTER TABLE collections ADD COLUMN user_context_id UUID;
ALTER TABLE tasks ADD COLUMN user_context_id UUID;
ALTER TABLE schedules ADD COLUMN user_context_id UUID;
ALTER TABLE jobs ADD COLUMN user_context_id UUID;
ALTER TABLE data_schemas ADD COLUMN user_context_id UUID;
ALTER TABLE records ADD COLUMN user_context_id UUID;
ALTER TABLE devices ADD COLUMN user_context_id UUID;
ALTER TABLE connections ADD COLUMN user_context_id UUID;
ALTER TABLE action_proposals ADD COLUMN user_context_id UUID;
ALTER TABLE action_approvals ADD COLUMN user_context_id UUID;
ALTER TABLE executions ADD COLUMN user_context_id UUID;
ALTER TABLE inbound_events ADD COLUMN user_context_id UUID;
ALTER TABLE audit_events ADD COLUMN user_context_id UUID;

DO $backfill$
DECLARE
    resource_name TEXT;
BEGIN
    FOREACH resource_name IN ARRAY ARRAY[
        'auth_identities', 'channel_identities', 'auth_sessions',
        'conversations', 'collections', 'tasks', 'schedules', 'jobs',
        'data_schemas', 'records', 'devices', 'connections',
        'action_proposals', 'action_approvals', 'executions',
        'inbound_events', 'audit_events'
    ] LOOP
        EXECUTE format(
            'UPDATE %I r SET user_context_id = c.id FROM user_contexts c
             WHERE r.user_id = c.user_id AND r.user_context_id IS NULL',
            resource_name
        );
    END LOOP;
END $backfill$;

-- Existing writers use user_id during rollout. A compatibility trigger fills
-- only the context already bound to that user; it cannot choose or change
-- the owner. Canonical callers may supply the context explicitly.
CREATE FUNCTION resource_context_compatibility() RETURNS trigger
LANGUAGE plpgsql AS $function$
BEGIN
    IF NEW.user_id IS NULL THEN
        IF TG_OP = 'UPDATE' AND TG_TABLE_NAME = 'audit_events'
           AND OLD.user_id IS NOT NULL THEN
            -- audit_events retains its row when users.user_id is SET NULL.
            NEW.user_context_id := NULL;
        ELSIF NEW.user_context_id IS NOT NULL THEN
            RAISE EXCEPTION 'global row cannot have user context'
                USING ERRCODE = '23514';
        END IF;
    ELSIF NEW.user_context_id IS NULL THEN
        SELECT id INTO NEW.user_context_id
        FROM user_contexts WHERE user_id = NEW.user_id;
        IF NEW.user_context_id IS NULL THEN
            RAISE EXCEPTION 'user % has no canonical context', NEW.user_id
                USING ERRCODE = '23503';
        END IF;
    END IF;
    RETURN NEW;
END $function$;

DO $constraints$
DECLARE
    resource_name TEXT;
BEGIN
    FOREACH resource_name IN ARRAY ARRAY[
        'auth_identities', 'channel_identities', 'auth_sessions',
        'conversations', 'collections', 'tasks', 'schedules', 'jobs',
        'data_schemas', 'records', 'devices', 'connections',
        'action_proposals', 'action_approvals', 'executions',
        'inbound_events', 'audit_events'
    ] LOOP
        EXECUTE format(
            'ALTER TABLE %I ADD CONSTRAINT %I
             FOREIGN KEY (user_context_id, user_id)
             REFERENCES user_contexts(id, user_id) ON DELETE RESTRICT',
            resource_name, resource_name || '_context_owner_fk'
        );
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE INSERT OR UPDATE OF user_id, user_context_id
             ON %I FOR EACH ROW EXECUTE FUNCTION resource_context_compatibility()',
            resource_name || '_context_compatibility', resource_name
        );
        EXECUTE format(
            'ALTER TABLE %I ADD CONSTRAINT %I
             CHECK ((user_id IS NULL) = (user_context_id IS NULL))',
            resource_name, resource_name || '_context_presence'
        );
        EXECUTE format(
            'CREATE INDEX %I ON %I (user_context_id)',
            resource_name || '_context_idx', resource_name
        );
    END LOOP;
END $constraints$;

-- The old global conversation key made two users sharing a host-supplied
-- conversation identifier collide. Ownership is part of the key now.
ALTER TABLE conversations DROP CONSTRAINT conversations_channel_external_key;
ALTER TABLE conversations ADD CONSTRAINT conversations_context_channel_external_key
    UNIQUE (user_context_id, channel, external_conversation_id);

-- A matching owner on the child row is also required for references to
-- another user-owned resource. The older single-column foreign keys remain
-- for their existing delete behavior; these keys add owner integrity.
ALTER TABLE auth_identities ADD CONSTRAINT auth_identities_id_user_key UNIQUE (id, user_id);
ALTER TABLE channel_identities ADD CONSTRAINT channel_identities_id_user_key UNIQUE (id, user_id);
ALTER TABLE collections ADD CONSTRAINT collections_id_user_key UNIQUE (id, user_id);
ALTER TABLE tasks ADD CONSTRAINT tasks_id_user_key UNIQUE (id, user_id);
ALTER TABLE schedules ADD CONSTRAINT schedules_id_user_key UNIQUE (id, user_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_id_user_key UNIQUE (id, user_id);
ALTER TABLE devices ADD CONSTRAINT devices_id_user_key UNIQUE (id, user_id);
ALTER TABLE connections ADD CONSTRAINT connections_id_user_key UNIQUE (id, user_id);
ALTER TABLE action_proposals ADD CONSTRAINT action_proposals_id_user_key UNIQUE (id, user_id);
ALTER TABLE action_approvals ADD CONSTRAINT action_approvals_id_user_key UNIQUE (id, user_id);
ALTER TABLE executions ADD CONSTRAINT executions_id_user_key UNIQUE (id, user_id);
ALTER TABLE inbound_events ADD CONSTRAINT inbound_events_id_user_key UNIQUE (id, user_id);

ALTER TABLE auth_sessions ADD CONSTRAINT auth_sessions_identity_owner_fk
    FOREIGN KEY (auth_identity_id, user_id) REFERENCES auth_identities(id, user_id)
    ON DELETE SET NULL (auth_identity_id);
ALTER TABLE auth_sessions ADD CONSTRAINT auth_sessions_device_owner_fk
    FOREIGN KEY (device_id, user_id) REFERENCES devices(id, user_id)
    ON DELETE SET NULL (device_id);
ALTER TABLE conversations ADD CONSTRAINT conversations_channel_identity_owner_fk
    FOREIGN KEY (channel_identity_id, user_id) REFERENCES channel_identities(id, user_id)
    ON DELETE SET NULL (channel_identity_id);
ALTER TABLE tasks ADD CONSTRAINT tasks_collection_owner_fk
    FOREIGN KEY (collection_id, user_id) REFERENCES collections(id, user_id)
    ON DELETE SET NULL (collection_id);
ALTER TABLE schedules ADD CONSTRAINT schedules_task_owner_fk
    FOREIGN KEY (task_id, user_id) REFERENCES tasks(id, user_id)
    ON DELETE SET NULL (task_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_task_owner_fk
    FOREIGN KEY (task_id, user_id) REFERENCES tasks(id, user_id)
    ON DELETE SET NULL (task_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_schedule_owner_fk
    FOREIGN KEY (schedule_id, user_id) REFERENCES schedules(id, user_id)
    ON DELETE SET NULL (schedule_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_device_owner_fk
    FOREIGN KEY (assigned_device_id, user_id) REFERENCES devices(id, user_id)
    ON DELETE SET NULL (assigned_device_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_source_event_owner_fk
    FOREIGN KEY (source_event_id, user_id) REFERENCES inbound_events(id, user_id)
    ON DELETE SET NULL (source_event_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_owned_refs_have_user
    CHECK ((task_id IS NULL AND schedule_id IS NULL AND assigned_device_id IS NULL
        AND source_event_id IS NULL) OR user_id IS NOT NULL);
ALTER TABLE records ADD CONSTRAINT records_collection_owner_fk
    FOREIGN KEY (collection_id, user_id) REFERENCES collections(id, user_id)
    ON DELETE SET NULL (collection_id);
ALTER TABLE records ADD CONSTRAINT records_source_event_owner_fk
    FOREIGN KEY (source_event_id, user_id) REFERENCES inbound_events(id, user_id)
    ON DELETE SET NULL (source_event_id);
ALTER TABLE action_proposals ADD CONSTRAINT action_proposals_task_owner_fk
    FOREIGN KEY (task_id, user_id) REFERENCES tasks(id, user_id)
    ON DELETE SET NULL (task_id);
ALTER TABLE action_proposals ADD CONSTRAINT action_proposals_job_owner_fk
    FOREIGN KEY (job_id, user_id) REFERENCES jobs(id, user_id)
    ON DELETE SET NULL (job_id);
ALTER TABLE action_proposals ADD CONSTRAINT action_proposals_connection_owner_fk
    FOREIGN KEY (connection_id, user_id) REFERENCES connections(id, user_id)
    ON DELETE SET NULL (connection_id);
ALTER TABLE action_approvals ADD CONSTRAINT action_approvals_proposal_owner_fk
    FOREIGN KEY (proposal_id, user_id) REFERENCES action_proposals(id, user_id)
    ON DELETE CASCADE;
ALTER TABLE executions ADD CONSTRAINT executions_proposal_owner_fk
    FOREIGN KEY (proposal_id, user_id) REFERENCES action_proposals(id, user_id)
    ON DELETE RESTRICT;
ALTER TABLE executions ADD CONSTRAINT executions_approval_owner_fk
    FOREIGN KEY (approval_id, user_id) REFERENCES action_approvals(id, user_id)
    ON DELETE RESTRICT;
ALTER TABLE executions ADD CONSTRAINT executions_connection_owner_fk
    FOREIGN KEY (connection_id, user_id) REFERENCES connections(id, user_id)
    ON DELETE SET NULL (connection_id);
ALTER TABLE inbound_events ADD CONSTRAINT inbound_events_execution_owner_fk
    FOREIGN KEY (execution_id, user_id) REFERENCES executions(id, user_id)
    ON DELETE SET NULL (execution_id);
ALTER TABLE inbound_events ADD CONSTRAINT inbound_events_execution_has_user
    CHECK (execution_id IS NULL OR user_id IS NOT NULL);

-- Stop the migration if a malformed preexisting row escaped the backfill.
DO $reconcile$
DECLARE
    resource_name TEXT;
    orphan_count BIGINT;
BEGIN
    FOREACH resource_name IN ARRAY ARRAY[
        'auth_identities', 'channel_identities', 'auth_sessions',
        'conversations', 'collections', 'tasks', 'schedules', 'jobs',
        'data_schemas', 'records', 'devices', 'connections',
        'action_proposals', 'action_approvals', 'executions',
        'inbound_events', 'audit_events'
    ] LOOP
        EXECUTE format(
            'SELECT count(*) FROM %I r LEFT JOIN user_contexts c
             ON c.id = r.user_context_id AND c.user_id = r.user_id
             WHERE r.user_id IS NOT NULL AND c.id IS NULL',
            resource_name
        ) INTO orphan_count;
        IF orphan_count > 0 THEN
            RAISE EXCEPTION '% has % owner orphans', resource_name, orphan_count;
        END IF;
    END LOOP;
END $reconcile$;

CREATE TABLE integration_declaration_versions (
    integration_id UUID NOT NULL REFERENCES integration_definitions(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL CHECK (version > 0),
    declaration JSONB NOT NULL CHECK (jsonb_typeof(declaration) = 'object'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (integration_id, version)
);

-- Earlier overwritten versions cannot be reconstructed. Preserve the current
-- declaration at migration time so subsequent revisions have a durable base.
INSERT INTO integration_declaration_versions (integration_id, version, declaration, created_at)
SELECT i.id, i.declaration_version,
       jsonb_build_object(
           'deployment_external_key', d.external_key,
           'external_key', i.external_key,
           'protocol', i.protocol,
           'display_name', i.display_name,
           'declaration_version', i.declaration_version,
           'capabilities', COALESCE(
               jsonb_agg(jsonb_build_object(
                   'external_key', c.external_key,
                   'effect', c.effect,
                   'access_needs', c.access_needs,
                   'data_recipients', c.data_recipients,
                   'regions', c.regions,
                   'failure_modes', c.failure_modes,
                   'optional_guarantees', c.optional_guarantees
               ) ORDER BY c.external_key) FILTER (WHERE c.id IS NOT NULL),
               '[]'::jsonb
           )
       ), now()
FROM integration_definitions i
JOIN platform_deployments d ON d.id = i.deployment_id
LEFT JOIN integration_capability_declarations c ON c.integration_id = i.id
GROUP BY i.id, d.external_key;

-- Migration: 20260923000004_provider_verified_connections.sql
-- Provider-verified connection authorization sessions and account display identity.

ALTER TABLE external_connections
    ADD COLUMN IF NOT EXISTS account_display_id TEXT;

CREATE TABLE IF NOT EXISTS connection_authorization_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    integration_id UUID NOT NULL REFERENCES integration_definitions(id) ON DELETE CASCADE,
    state_token TEXT NOT NULL UNIQUE,
    credential_custody TEXT NOT NULL DEFAULT 'external_operator',
    requested_capabilities TEXT[] NOT NULL DEFAULT '{}'::text[],
    redirect_uri TEXT,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT conn_auth_sess_custody_valid CHECK (credential_custody IN ('platform_held', 'external_operator'))
);

CREATE INDEX IF NOT EXISTS conn_auth_sess_state_idx ON connection_authorization_sessions (state_token) WHERE consumed_at IS NULL;

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

CREATE TABLE remote_extensions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    display_name TEXT NOT NULL,
    protocol TEXT NOT NULL CHECK (protocol IN ('mcp', 'direct')),
    endpoint_url TEXT NOT NULL,
    operator_id TEXT NOT NULL,
    operator_name TEXT NOT NULL,
    support_email TEXT,
    terms_url TEXT,
    current_version INTEGER NOT NULL DEFAULT 1 CHECK (current_version > 0),
    conformance_status TEXT NOT NULL DEFAULT 'pending' CHECK (conformance_status IN ('pending', 'passed', 'failed')),
    operator_enabled BOOLEAN NOT NULL DEFAULT false,
    consent_status TEXT NOT NULL DEFAULT 'consented' CHECK (consent_status IN ('consented', 'consent_required')),
    lifecycle_state TEXT NOT NULL DEFAULT 'installed' CHECK (lifecycle_state IN ('installed', 'active', 'quarantined', 'disabled', 'removed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT remote_extensions_user_key_unique UNIQUE (user_context_id, external_key)
);

CREATE TABLE remote_extension_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    extension_id UUID NOT NULL REFERENCES remote_extensions(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL CHECK (version > 0),
    endpoint_url TEXT NOT NULL,
    operator_id TEXT NOT NULL,
    operator_name TEXT NOT NULL,
    capabilities JSONB NOT NULL DEFAULT '[]'::jsonb,
    conformance_status TEXT NOT NULL DEFAULT 'pending' CHECK (conformance_status IN ('pending', 'passed', 'failed')),
    conformance_report JSONB NOT NULL DEFAULT '{}'::jsonb,
    consent_granted_at TIMESTAMPTZ,
    quarantined_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT remote_extension_versions_unique UNIQUE (extension_id, version)
);

CREATE TABLE remote_extension_conformance_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    extension_id UUID NOT NULL REFERENCES remote_extensions(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('passed', 'failed')),
    report JSONB NOT NULL DEFAULT '{}'::jsonb,
    run_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX remote_extensions_user_state_idx ON remote_extensions (user_context_id, lifecycle_state);

-- 20260923000007_reminders.sql
-- Table definitions for explicit-timezone reminders and delivery records (E40).

CREATE TABLE reminders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    title TEXT NOT NULL CHECK (length(btrim(title)) > 0),
    message TEXT NOT NULL CHECK (length(btrim(message)) > 0),
    channel TEXT NOT NULL CHECK (length(btrim(channel)) > 0),
    destination TEXT NOT NULL CHECK (length(btrim(destination)) > 0),
    timezone TEXT NOT NULL DEFAULT 'UTC',
    schedule_kind TEXT NOT NULL CHECK (schedule_kind IN ('one_time', 'interval', 'calendar_recurrence')),
    run_at TIMESTAMPTZ,
    interval_seconds BIGINT,
    recurrence_expression TEXT,
    next_trigger_at TIMESTAMPTZ,
    status TEXT NOT NULL DEFAULT 'scheduled' CHECK (status IN ('scheduled', 'delivered_to_channel', 'failed', 'unknown', 'missed', 'cancelled')),
    retry_count INT NOT NULL DEFAULT 0,
    max_retries INT NOT NULL DEFAULT 3,
    last_attempt_at TIMESTAMPTZ,
    delivered_at TIMESTAMPTZ,
    failure_reason TEXT,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT reminders_schedule_shape CHECK (
        (schedule_kind = 'one_time' AND run_at IS NOT NULL AND interval_seconds IS NULL AND recurrence_expression IS NULL)
        OR (schedule_kind = 'interval' AND interval_seconds IS NOT NULL AND interval_seconds > 0 AND recurrence_expression IS NULL)
        OR (schedule_kind = 'calendar_recurrence' AND recurrence_expression IS NOT NULL AND interval_seconds IS NULL)
    )
);

CREATE TABLE reminder_deliveries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    reminder_id UUID NOT NULL REFERENCES reminders(id) ON DELETE CASCADE,
    scheduled_for TIMESTAMPTZ NOT NULL,
    attempted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    status TEXT NOT NULL CHECK (status IN ('delivered_to_channel', 'failed', 'unknown', 'missed')),
    channel TEXT NOT NULL,
    destination TEXT NOT NULL,
    provider_receipt_id TEXT,
    failure_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX reminders_active_idx ON reminders (next_trigger_at) WHERE status = 'scheduled';
CREATE INDEX reminders_user_context_idx ON reminders (user_context_id);
CREATE INDEX reminder_deliveries_reminder_idx ON reminder_deliveries (reminder_id, scheduled_for);

ALTER TABLE reminders ENABLE ROW LEVEL SECURITY;
CREATE POLICY "service_role_reminders" ON reminders FOR ALL TO service_role USING (true) WITH CHECK (true);

ALTER TABLE reminder_deliveries ENABLE ROW LEVEL SECURITY;
CREATE POLICY "service_role_reminder_deliveries" ON reminder_deliveries FOR ALL TO service_role USING (true) WITH CHECK (true);

-- Durable change hints. These are never authoritative task or execution state.
CREATE TABLE status_events (
    cursor BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    aggregate_type TEXT NOT NULL,
    aggregate_id UUID NOT NULL,
    event_type TEXT NOT NULL,
    state TEXT NOT NULL,
    deduplication_key TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    committed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT status_events_dedup UNIQUE (user_context_id, deduplication_key),
    CONSTRAINT status_events_payload_object CHECK (jsonb_typeof(payload) = 'object')
);
CREATE INDEX status_events_context_cursor_idx ON status_events (user_context_id, cursor);

CREATE TABLE status_webhook_subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    endpoint TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'enabled' CHECK (state IN ('enabled', 'disabled')),
    secret_version INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX status_webhook_subscriptions_context_idx
    ON status_webhook_subscriptions (user_context_id, id);

-- Record committed state changes in the same transaction as the authoritative row.
CREATE FUNCTION record_task_status_event() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.user_context_id IS NOT NULL AND
       (TG_OP = 'INSERT' OR NEW.status IS DISTINCT FROM OLD.status) THEN
        -- Hold the context lock until commit, so cursor order matches commit order.
        PERFORM pg_advisory_xact_lock(73125, hashtext(NEW.user_context_id::text));
        INSERT INTO status_events (user_context_id, aggregate_type, aggregate_id,
            event_type, state, deduplication_key, occurred_at)
        VALUES (NEW.user_context_id, 'task', NEW.id, 'task.state_changed',
            NEW.status, gen_random_uuid()::text, now());
        INSERT INTO audit_events (user_id, user_context_id, actor, event_type, affected_ids, details)
        VALUES (NEW.user_id, NEW.user_context_id, 'core', 'task.state_changed',
            jsonb_build_array(jsonb_build_object('type','task','id',NEW.id)),
            jsonb_build_object('state',NEW.status));
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER tasks_status_event AFTER INSERT OR UPDATE OF status ON tasks
    FOR EACH ROW EXECUTE FUNCTION record_task_status_event();

CREATE FUNCTION record_run_status_event() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.user_context_id IS NOT NULL AND
       (TG_OP = 'INSERT' OR NEW.state IS DISTINCT FROM OLD.state OR
        NEW.wait_reason IS DISTINCT FROM OLD.wait_reason) THEN
        PERFORM pg_advisory_xact_lock(73125, hashtext(NEW.user_context_id::text));
        INSERT INTO status_events (user_context_id, aggregate_type, aggregate_id,
            event_type, state, deduplication_key, occurred_at)
        VALUES (NEW.user_context_id, 'run', NEW.id, 'run.state_changed',
            NEW.state, gen_random_uuid()::text, now());
        INSERT INTO audit_events (user_id, user_context_id, actor, event_type, affected_ids, details)
        VALUES (NEW.user_id, NEW.user_context_id, 'core', 'run.state_changed',
            jsonb_build_array(jsonb_build_object('type','task_run','id',NEW.id)),
            jsonb_build_object('state',NEW.state));
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER jobs_status_event AFTER INSERT OR UPDATE OF state, wait_reason ON jobs
    FOR EACH ROW EXECUTE FUNCTION record_run_status_event();

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

CREATE TABLE spending_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    capability_external_key TEXT NOT NULL,
    provider_external_key TEXT NOT NULL,
    currency TEXT NOT NULL,
    max_amount_minor BIGINT NOT NULL CHECK (max_amount_minor >= 0),
    version INTEGER NOT NULL DEFAULT 1,
    UNIQUE (user_context_id, capability_external_key, provider_external_key, currency)
);

CREATE TABLE operational_quotas (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    provider_external_key TEXT NOT NULL,
    model_identifier TEXT NOT NULL,
    account_hash BYTEA NOT NULL,
    connection_id UUID NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    max_attempts INTEGER NOT NULL CHECK (max_attempts > 0),
    reserved_attempts INTEGER NOT NULL DEFAULT 0 CHECK (reserved_attempts >= 0),
    version INTEGER NOT NULL DEFAULT 1,
    UNIQUE (user_context_id, provider_external_key, model_identifier, account_hash, connection_id)
);

CREATE TABLE operational_quota_reservations (
    attempt_id UUID PRIMARY KEY,
    quota_id UUID NOT NULL REFERENCES operational_quotas(id) ON DELETE CASCADE,
    approval_id UUID NOT NULL REFERENCES action_approvals(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- An occurrence may request an external effect only once. Unknown outcomes need
-- reconciliation; blindly retrying could place a second phone call.
CREATE TABLE schedule_occurrence_dispatches (
    schedule_id UUID NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
    occurrence_at TIMESTAMPTZ NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('claimed','dispatched','unknown')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (schedule_id, occurrence_at)
);

-- Status hints use an outbox so a committed state transition cannot be lost
-- when an API or worker process stops before delivery.
ALTER TABLE status_webhook_subscriptions
    DROP CONSTRAINT status_webhook_subscriptions_state_check;
ALTER TABLE status_webhook_subscriptions
    ADD CONSTRAINT status_webhook_subscriptions_state_check
    CHECK (state IN ('enabled', 'unhealthy', 'disabled'));

CREATE TABLE status_webhook_secrets (
    subscription_id UUID PRIMARY KEY REFERENCES status_webhook_subscriptions(id) ON DELETE CASCADE,
    ciphertext BYTEA NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE status_webhook_deliveries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id UUID NOT NULL REFERENCES status_webhook_subscriptions(id) ON DELETE CASCADE,
    event_cursor BIGINT NOT NULL REFERENCES status_events(cursor) ON DELETE CASCADE,
    state TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'sending', 'sent', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_until TIMESTAMPTZ,
    lease_token UUID,
    lease_owner TEXT,
    delivered_at TIMESTAMPTZ,
    last_http_status INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (subscription_id, event_cursor)
);
CREATE INDEX status_webhook_deliveries_ready_idx
    ON status_webhook_deliveries (available_at, id)
    WHERE state IN ('queued', 'sending');

CREATE FUNCTION enqueue_status_webhooks() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO status_webhook_deliveries (subscription_id, event_cursor)
    SELECT id, NEW.cursor FROM status_webhook_subscriptions
    WHERE user_context_id = NEW.user_context_id AND state = 'enabled';
    RETURN NEW;
END;
$$;
CREATE TRIGGER status_event_webhook_outbox AFTER INSERT ON status_events
    FOR EACH ROW EXECUTE FUNCTION enqueue_status_webhooks();

-- A provider event's replay identity is committed atomically with its
-- normalized execution transition; no raw provider payload is retained.
CREATE TABLE verified_integration_events (
    integration_external_key TEXT NOT NULL,
    external_account_hash TEXT NOT NULL,
    provider_event_id TEXT NOT NULL,
    execution_id UUID NOT NULL REFERENCES executions(id),
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (integration_external_key, external_account_hash, provider_event_id)
);

-- These are operational or credential records, never directly readable by a
-- host user. Only the trusted service role may access them.
ALTER TABLE status_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE status_webhook_subscriptions ENABLE ROW LEVEL SECURITY;
ALTER TABLE status_webhook_secrets ENABLE ROW LEVEL SECURITY;
ALTER TABLE status_webhook_deliveries ENABLE ROW LEVEL SECURITY;
ALTER TABLE verified_integration_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY service_role_status_events ON status_events
    FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY service_role_status_webhook_subscriptions ON status_webhook_subscriptions
    FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY service_role_status_webhook_secrets ON status_webhook_secrets
    FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY service_role_status_webhook_deliveries ON status_webhook_deliveries
    FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY service_role_verified_integration_events ON verified_integration_events
    FOR ALL TO service_role USING (true) WITH CHECK (true);

ALTER TABLE jobs DROP CONSTRAINT jobs_kind_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind_check CHECK (kind IN ('process_event', 'run_schedule', 'dispatch_action', 'summarize_conversation', 'evaluate_task', 'execute_task', 'process_sms_batch'));

CREATE TABLE sms_batches (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    messages JSONB NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'processed', 'failed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at TIMESTAMPTZ
);

CREATE INDEX sms_batches_user_status_idx ON sms_batches (user_id, status);

CREATE TABLE device_timeline_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    source TEXT NOT NULL,
    category TEXT NOT NULL,
    title TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'completed' CHECK (kind IN ('completed', 'scheduled', 'overdue')),
    start_at TIMESTAMPTZ NOT NULL,
    end_at TIMESTAMPTZ,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    sms_batch_id UUID REFERENCES sms_batches(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX device_timeline_entries_user_start_idx ON device_timeline_entries (user_id, start_at);

CREATE TABLE data_source_consents (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    data_source TEXT NOT NULL CHECK (data_source IN ('sms', 'location')),
    granted_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    retention_days INTEGER NOT NULL DEFAULT 90 CHECK (retention_days > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT data_source_consents_user_source_unique UNIQUE (user_id, data_source)
);

-- 20260926000001_spans.sql
-- Replaces tasks and device_timeline_entries with spans: anything that
-- occupies time (past, present, or planned). Projects become collections
-- that hold spans through collection_spans.

CREATE TABLE spans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    user_context_id UUID,
    parent_id UUID,
    title TEXT NOT NULL CHECK (length(btrim(title)) > 0),
    notes TEXT NOT NULL DEFAULT '',
    category TEXT NOT NULL DEFAULT 'general' CHECK (length(btrim(category)) > 0),
    source TEXT NOT NULL DEFAULT 'user' CHECK (length(btrim(source)) > 0),
    source_ref TEXT,
    status TEXT NOT NULL DEFAULT 'planned'
        CHECK (status IN ('planned', 'active', 'waiting_user', 'done', 'failed', 'cancelled')),
    start_at TIMESTAMPTZ,
    end_at TIMESTAMPTZ,
    due_at TIMESTAMPTZ,
    priority INTEGER NOT NULL DEFAULT 0,
    execution_type TEXT CHECK (execution_type IN ('autonomous', 'interactive', 'manual_human')),
    execution_result JSONB NOT NULL DEFAULT '{}'::jsonb,
    data JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(data) = 'object'),
    version INTEGER NOT NULL DEFAULT 1,
    cancellation_requested_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT spans_id_user_key UNIQUE (id, user_id),
    CONSTRAINT spans_context_owner_fk FOREIGN KEY (user_context_id, user_id)
        REFERENCES user_contexts(id, user_id) ON DELETE RESTRICT,
    CONSTRAINT spans_context_presence CHECK ((user_id IS NULL) = (user_context_id IS NULL)),
    CONSTRAINT spans_parent_owner_fk FOREIGN KEY (parent_id, user_id)
        REFERENCES spans(id, user_id) ON DELETE SET NULL (parent_id),
    CONSTRAINT spans_not_own_parent CHECK (parent_id IS NULL OR parent_id <> id),
    CONSTRAINT spans_time_order CHECK (start_at IS NULL OR end_at IS NULL OR end_at >= start_at),
    CONSTRAINT spans_source_ref_key UNIQUE (user_id, source, source_ref)
);

CREATE TRIGGER spans_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id
    ON spans FOR EACH ROW EXECUTE FUNCTION resource_context_compatibility();

CREATE INDEX spans_user_start_idx ON spans (user_id, start_at);
CREATE INDEX spans_user_status_idx ON spans (user_id, status, due_at);
CREATE INDEX spans_parent_idx ON spans (parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX spans_context_idx ON spans (user_context_id);

ALTER TABLE collections DROP CONSTRAINT collections_kind_check;
UPDATE collections SET kind = 'custom' WHERE kind = 'project';
ALTER TABLE collections
    ALTER COLUMN kind SET DEFAULT 'custom',
    ADD CONSTRAINT collections_kind_check CHECK (kind IN ('trip', 'event', 'course', 'area', 'custom')),
    ADD COLUMN starts_at TIMESTAMPTZ,
    ADD COLUMN ends_at TIMESTAMPTZ,
    ADD CONSTRAINT collections_window_order CHECK (starts_at IS NULL OR ends_at IS NULL OR ends_at >= starts_at);

CREATE TABLE collection_spans (
    collection_id UUID NOT NULL,
    span_id UUID NOT NULL,
    user_id UUID NOT NULL,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (collection_id, span_id),
    CONSTRAINT collection_spans_collection_owner_fk FOREIGN KEY (collection_id, user_id)
        REFERENCES collections(id, user_id) ON DELETE CASCADE,
    CONSTRAINT collection_spans_span_owner_fk FOREIGN KEY (span_id, user_id)
        REFERENCES spans(id, user_id) ON DELETE CASCADE
);
CREATE INDEX collection_spans_span_idx ON collection_spans (span_id);

ALTER TABLE schedules DROP CONSTRAINT schedules_task_owner_fk;
ALTER TABLE schedules DROP CONSTRAINT IF EXISTS schedules_task_id_fkey;
UPDATE schedules SET task_id = NULL;
ALTER TABLE schedules RENAME COLUMN task_id TO span_id;
ALTER TABLE schedules ADD CONSTRAINT schedules_span_owner_fk
    FOREIGN KEY (span_id, user_id) REFERENCES spans(id, user_id) ON DELETE SET NULL (span_id);

ALTER TABLE jobs DROP CONSTRAINT jobs_task_owner_fk;
ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_task_id_fkey;
ALTER TABLE jobs DROP CONSTRAINT jobs_owned_refs_have_user;
ALTER TABLE jobs DROP CONSTRAINT jobs_kind_check;
DELETE FROM jobs WHERE kind IN ('evaluate_task', 'execute_task');
ALTER TABLE jobs RENAME COLUMN task_id TO span_id;
ALTER TABLE jobs ADD CONSTRAINT jobs_span_owner_fk
    FOREIGN KEY (span_id, user_id) REFERENCES spans(id, user_id) ON DELETE SET NULL (span_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_owned_refs_have_user
    CHECK ((span_id IS NULL AND schedule_id IS NULL AND assigned_device_id IS NULL
        AND source_event_id IS NULL) OR user_id IS NOT NULL);
ALTER TABLE jobs ADD CONSTRAINT jobs_kind_check CHECK (kind IN (
    'process_event', 'run_schedule', 'dispatch_action', 'summarize_conversation',
    'evaluate_span', 'execute_span', 'process_sms_batch'));

ALTER TABLE action_proposals DROP CONSTRAINT action_proposals_task_owner_fk;
ALTER TABLE action_proposals DROP CONSTRAINT IF EXISTS action_proposals_task_id_fkey;
UPDATE action_proposals SET task_id = NULL;
ALTER TABLE action_proposals RENAME COLUMN task_id TO span_id;
ALTER TABLE action_proposals ADD CONSTRAINT action_proposals_span_owner_fk
    FOREIGN KEY (span_id, user_id) REFERENCES spans(id, user_id) ON DELETE SET NULL (span_id);

CREATE FUNCTION record_span_status_event() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.user_context_id IS NOT NULL AND NEW.execution_type IS NOT NULL AND
       (TG_OP = 'INSERT' OR NEW.status IS DISTINCT FROM OLD.status) THEN
        PERFORM pg_advisory_xact_lock(73125, hashtext(NEW.user_context_id::text));
        INSERT INTO status_events (user_context_id, aggregate_type, aggregate_id,
            event_type, state, deduplication_key, occurred_at)
        VALUES (NEW.user_context_id, 'span', NEW.id, 'span.state_changed',
            NEW.status, gen_random_uuid()::text, now());
        INSERT INTO audit_events (user_id, user_context_id, actor, event_type, affected_ids, details)
        VALUES (NEW.user_id, NEW.user_context_id, 'core', 'span.state_changed',
            jsonb_build_array(jsonb_build_object('type','span','id',NEW.id)),
            jsonb_build_object('state',NEW.status));
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER spans_status_event AFTER INSERT OR UPDATE OF status ON spans
    FOR EACH ROW EXECUTE FUNCTION record_span_status_event();

ALTER TABLE reminders ADD COLUMN span_id UUID REFERENCES spans(id) ON DELETE CASCADE;
CREATE INDEX reminders_span_idx ON reminders (span_id) WHERE span_id IS NOT NULL;

CREATE FUNCTION sync_reminder_span() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO spans (user_id, user_context_id, title, notes, category, source, source_ref, status, start_at)
        SELECT uc.user_id, uc.id, NEW.title, NEW.message, 'reminder', 'reminder', NEW.id::text,
            'planned', NEW.next_trigger_at
        FROM user_contexts uc WHERE uc.id = NEW.user_context_id
        RETURNING id INTO NEW.span_id;
    ELSIF NEW.span_id IS NOT NULL THEN
        UPDATE spans SET
            title = NEW.title,
            notes = NEW.message,
            status = CASE NEW.status
                WHEN 'scheduled' THEN 'planned'
                WHEN 'delivered_to_channel' THEN 'done'
                WHEN 'cancelled' THEN 'cancelled'
                ELSE 'failed' END,
            start_at = CASE WHEN NEW.status = 'scheduled' THEN NEW.next_trigger_at
                ELSE COALESCE(NEW.delivered_at, NEW.last_attempt_at, start_at) END,
            updated_at = now()
        WHERE id = NEW.span_id;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER reminders_sync_span BEFORE INSERT OR UPDATE ON reminders
    FOR EACH ROW EXECUTE FUNCTION sync_reminder_span();

DROP VIEW IF EXISTS projects;
DROP TABLE device_timeline_entries;
DROP TABLE tasks;

ALTER TABLE spans ENABLE ROW LEVEL SECURITY;
CREATE POLICY "service_role_spans" ON spans FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "spans_user_all" ON spans FOR ALL TO authenticated
    USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());

ALTER TABLE collection_spans ENABLE ROW LEVEL SECURITY;
CREATE POLICY "service_role_collection_spans" ON collection_spans FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY "collection_spans_user_all" ON collection_spans FOR ALL TO authenticated
    USING (user_id = auth.uid()) WITH CHECK (user_id = auth.uid());

ALTER TABLE data_source_consents ADD COLUMN IF NOT EXISTS synced_until TIMESTAMPTZ;

CREATE TABLE skill_packages (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id) ON DELETE CASCADE,
    owner_user_context_id UUID REFERENCES user_contexts(id) ON DELETE CASCADE,
    external_key TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    latest_version INTEGER NOT NULL DEFAULT 1 CHECK (latest_version > 0),
    state TEXT NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'removed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX skill_packages_curated_key
    ON skill_packages (deployment_id, external_key)
    WHERE owner_user_context_id IS NULL;
CREATE UNIQUE INDEX skill_packages_private_key
    ON skill_packages (owner_user_context_id, external_key)
    WHERE owner_user_context_id IS NOT NULL;

CREATE TABLE skill_package_versions (
    skill_id UUID NOT NULL REFERENCES skill_packages(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL CHECK (version > 0),
    instructions TEXT NOT NULL,
    requested_capabilities TEXT[] NOT NULL DEFAULT '{}'::text[],
    resources JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (skill_id, version)
);

CREATE TABLE skill_installations (
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    skill_id UUID NOT NULL REFERENCES skill_packages(id) ON DELETE RESTRICT,
    installed_version INTEGER NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_context_id, skill_id),
    FOREIGN KEY (skill_id, installed_version)
        REFERENCES skill_package_versions(skill_id, version)
);

-- A connection record is not proof that Core holds a provider credential.
ALTER TABLE connections ALTER COLUMN secret_reference DROP NOT NULL;
UPDATE connections SET secret_reference = NULL WHERE secret_reference = 'vault-' || id::text;

-- Installing a skill never exposes its instructions to every agent.
CREATE TABLE skill_agent_enablements (
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    skill_id UUID NOT NULL REFERENCES skill_packages(id) ON DELETE RESTRICT,
    agent_definition_id UUID NOT NULL REFERENCES agent_definitions(id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT true,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_context_id, skill_id, agent_definition_id),
    FOREIGN KEY (user_context_id, skill_id)
        REFERENCES skill_installations(user_context_id, skill_id) ON DELETE CASCADE
);

-- OAuth clients Vox holds with each MCP authorization server. Dynamic clients
-- are registered once per (issuer, redirect URI) and reused for every user.
CREATE TABLE mcp_oauth_clients (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issuer TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    client_id TEXT NOT NULL,
    client_secret_ciphertext BYTEA,
    token_endpoint_auth_method TEXT NOT NULL DEFAULT 'none',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT mcp_oauth_clients_issuer_redirect_unique UNIQUE (issuer, redirect_uri)
);

-- One pending browser authorization. Only a hash of the state is stored, the
-- PKCE verifier is encrypted, and a session is single-use and short-lived.
CREATE TABLE mcp_authorization_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    extension_id UUID NOT NULL REFERENCES remote_extensions(id) ON DELETE CASCADE,
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE CASCADE,
    state_hash TEXT NOT NULL UNIQUE,
    code_verifier_ciphertext BYTEA NOT NULL,
    issuer TEXT NOT NULL,
    token_endpoint TEXT NOT NULL,
    client_id TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    resource TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Provider-verified credentials for a connected app plus the tools its MCP
-- server reported. Tokens are AES-256-GCM encrypted with VOX_CREDENTIAL_KEY.
CREATE TABLE remote_extension_credentials (
    extension_id UUID PRIMARY KEY REFERENCES remote_extensions(id) ON DELETE CASCADE,
    issuer TEXT NOT NULL,
    token_endpoint TEXT NOT NULL,
    client_id TEXT NOT NULL,
    resource TEXT NOT NULL,
    access_token_ciphertext BYTEA NOT NULL,
    refresh_token_ciphertext BYTEA,
    scope TEXT,
    expires_at TIMESTAMPTZ,
    server_info JSONB NOT NULL DEFAULT '{}'::jsonb,
    tools JSONB NOT NULL DEFAULT '[]'::jsonb,
    connected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A connected-app action that changes something in the user's account waits
-- here until the user confirms it in a later conversation turn.
CREATE TABLE connected_app_pending_actions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    extension_id UUID NOT NULL REFERENCES remote_extensions(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    arguments_hash TEXT NOT NULL,
    proposed_turn UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX connected_app_pending_actions_lookup
    ON connected_app_pending_actions (user_id, extension_id, tool_name, arguments_hash);

-- When each app's tool list was last read from its server, and when the
-- agent last used the app (used to keep an in-progress flow's tools loaded).
ALTER TABLE remote_extension_credentials
    ADD COLUMN tools_refreshed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN last_used_at TIMESTAMPTZ;

-- Pending actions keep their exact arguments, so a confirmation runs what
-- the user was shown instead of whatever the model regenerates.
ALTER TABLE connected_app_pending_actions
    ADD COLUMN arguments JSONB NOT NULL DEFAULT '{}'::jsonb;

-- An OAuth callback must complete against the exact remote extension version
-- and endpoint that began the browser flow. Existing short-lived sessions
-- cannot be safely bound retroactively, so require a fresh authorization.
ALTER TABLE mcp_authorization_sessions
    ADD COLUMN endpoint_url TEXT,
    ADD COLUMN extension_version INTEGER;

DELETE FROM mcp_authorization_sessions;

ALTER TABLE mcp_authorization_sessions
    ALTER COLUMN endpoint_url SET NOT NULL,
    ALTER COLUMN extension_version SET NOT NULL,
    ADD CONSTRAINT mcp_authorization_sessions_version_positive
        CHECK (extension_version > 0);

-- Model-inferred conversation confirmations are retired. Consequential
-- connector actions must use the authenticated proposal/approval path.
DROP TABLE IF EXISTS connected_app_pending_actions;
-- The generic connection initiate/callback endpoints never had a provider
-- exchange; remove their unused state store with those endpoints.
DROP TABLE IF EXISTS connection_authorization_sessions;
ALTER TABLE remote_extension_credentials DROP COLUMN IF EXISTS last_used_at;

-- A connection has one authoritative record. Existing dual-written ids in
-- action proposals and executions already match external_connections ids.
ALTER TABLE action_proposals DROP CONSTRAINT IF EXISTS action_proposals_connection_id_fkey;
ALTER TABLE action_proposals DROP CONSTRAINT IF EXISTS action_proposals_connection_owner_fk;
ALTER TABLE executions DROP CONSTRAINT IF EXISTS executions_connection_id_fkey;
ALTER TABLE executions DROP CONSTRAINT IF EXISTS executions_connection_owner_fk;
ALTER TABLE operational_quotas DROP CONSTRAINT IF EXISTS operational_quotas_connection_id_fkey;
ALTER TABLE agent_capability_grants DROP CONSTRAINT IF EXISTS agent_capability_grants_connection_id_fkey;

ALTER TABLE external_connections
    ADD CONSTRAINT external_connections_id_context_key UNIQUE (id, user_context_id);

ALTER TABLE action_proposals
    ADD CONSTRAINT action_proposals_connection_context_fk
    FOREIGN KEY (connection_id, user_context_id)
    REFERENCES external_connections(id, user_context_id)
    ON DELETE SET NULL (connection_id);
ALTER TABLE executions
    ADD CONSTRAINT executions_connection_context_fk
    FOREIGN KEY (connection_id, user_context_id)
    REFERENCES external_connections(id, user_context_id)
    ON DELETE SET NULL (connection_id);
ALTER TABLE operational_quotas
    ADD CONSTRAINT operational_quotas_connection_context_fk
    FOREIGN KEY (connection_id, user_context_id)
    REFERENCES external_connections(id, user_context_id) ON DELETE CASCADE;
ALTER TABLE agent_capability_grants
    ADD CONSTRAINT agent_capability_grants_connection_context_fk
    FOREIGN KEY (connection_id, user_context_id)
    REFERENCES external_connections(id, user_context_id) ON DELETE RESTRICT;

DROP TABLE connections;

-- An account owner can have several host contexts. External action authority
-- belongs to one context, including the proposal, approval and execution.
ALTER TABLE action_proposals ALTER COLUMN user_context_id SET NOT NULL;
ALTER TABLE action_approvals ALTER COLUMN user_context_id SET NOT NULL;
ALTER TABLE executions ALTER COLUMN user_context_id SET NOT NULL;

ALTER TABLE action_proposals
    ADD CONSTRAINT action_proposals_id_context_key UNIQUE (id, user_context_id);
ALTER TABLE action_approvals
    ADD CONSTRAINT action_approvals_id_context_key UNIQUE (id, user_context_id);

ALTER TABLE action_approvals DROP CONSTRAINT IF EXISTS action_approvals_proposal_id_fkey;
ALTER TABLE action_approvals
    ADD CONSTRAINT action_approvals_proposal_context_fk
    FOREIGN KEY (proposal_id, user_context_id)
    REFERENCES action_proposals(id, user_context_id) ON DELETE CASCADE;

ALTER TABLE executions DROP CONSTRAINT IF EXISTS executions_proposal_id_fkey;
ALTER TABLE executions DROP CONSTRAINT IF EXISTS executions_approval_id_fkey;
ALTER TABLE executions
    ADD CONSTRAINT executions_proposal_context_fk
    FOREIGN KEY (proposal_id, user_context_id)
    REFERENCES action_proposals(id, user_context_id) ON DELETE RESTRICT;
ALTER TABLE executions
    ADD CONSTRAINT executions_approval_context_fk
    FOREIGN KEY (approval_id, user_context_id)
    REFERENCES action_approvals(id, user_context_id) ON DELETE RESTRICT;

ALTER TABLE executions DROP CONSTRAINT IF EXISTS executions_user_idempotency_key;
ALTER TABLE executions
    ADD CONSTRAINT executions_context_idempotency_key UNIQUE (user_context_id, idempotency_key);

-- A connection belongs to either a deployment integration or one user-owned
-- remote extension. OAuth credentials remain separate, encrypted custody data.
ALTER TABLE remote_extensions
    ADD CONSTRAINT remote_extensions_id_context_key UNIQUE (id, user_context_id);

ALTER TABLE external_connections ALTER COLUMN integration_id DROP NOT NULL;
ALTER TABLE external_connections
    ADD COLUMN remote_extension_id UUID;
ALTER TABLE external_connections
    ADD CONSTRAINT external_connections_one_source
        CHECK ((integration_id IS NOT NULL) <> (remote_extension_id IS NOT NULL));
ALTER TABLE external_connections
    ADD CONSTRAINT external_connections_remote_extension_context_fk
        FOREIGN KEY (remote_extension_id, user_context_id)
        REFERENCES remote_extensions(id, user_context_id) ON DELETE RESTRICT;
CREATE UNIQUE INDEX external_connections_one_account_per_remote_extension
    ON external_connections(user_context_id, remote_extension_id)
    WHERE remote_extension_id IS NOT NULL;

DROP TABLE IF EXISTS connection_authorization_sessions;
DROP TABLE IF EXISTS job_attempts;

ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_owned_refs_have_user;
ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_assigned_device_fk;
ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_device_owner_fk;
ALTER TABLE jobs DROP COLUMN IF EXISTS assigned_device_id;
ALTER TABLE jobs ADD CONSTRAINT jobs_owned_refs_have_user
    CHECK ((span_id IS NULL AND schedule_id IS NULL AND source_event_id IS NULL) OR user_id IS NOT NULL);

ALTER TABLE jobs ADD COLUMN priority SMALLINT NOT NULL DEFAULT 0;

DROP INDEX jobs_queued_idx;
CREATE INDEX jobs_queued_idx ON jobs (priority DESC, available_at, id) WHERE state = 'pending';

ALTER TABLE jobs ALTER COLUMN max_attempts SET DEFAULT 3;

-- Immutable deployment-reviewed connector packages. Installations carry no authority.
CREATE TABLE connector_packages (
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id),
    external_key TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    digest TEXT NOT NULL,
    manifest JSONB NOT NULL,
    review JSONB NOT NULL CHECK (jsonb_typeof(review)='object' AND review <> '{}'::jsonb),
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (deployment_id, external_key, version)
);
CREATE INDEX connector_packages_catalog ON connector_packages(deployment_id, external_key, version DESC) WHERE enabled;
CREATE TABLE connector_package_installations (
    extension_id UUID PRIMARY KEY REFERENCES remote_extensions(id),
    deployment_id UUID NOT NULL,
    external_key TEXT NOT NULL,
    version INTEGER NOT NULL,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (deployment_id,external_key,version) REFERENCES connector_packages(deployment_id,external_key,version)
);
CREATE INDEX connector_package_installations_version ON connector_package_installations(deployment_id,external_key,version);

