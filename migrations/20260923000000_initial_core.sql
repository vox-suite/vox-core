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
