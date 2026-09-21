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

CREATE TABLE user_profiles (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    facts JSONB NOT NULL DEFAULT '{}'::jsonb,
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_profiles_facts_object CHECK (jsonb_typeof(facts) = 'object')
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
    CONSTRAINT jobs_kind_valid CHECK (kind IN ('process_event', 'run_schedule', 'dispatch_action', 'summarize_conversation')),
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
    kind TEXT NOT NULL,
    payload JSONB NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    idempotency_key TEXT NOT NULL,
    provider_call_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT actions_kind_valid CHECK (kind IN ('outbound_call')),
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
CREATE INDEX scheduled_tasks_due_idx ON scheduled_tasks (next_run_at)
    WHERE state = 'active';
CREATE INDEX messages_conversation_idx ON messages (conversation_id, sequence_number);
CREATE INDEX summaries_user_created_idx ON conversation_summaries (user_id, created_at DESC);
