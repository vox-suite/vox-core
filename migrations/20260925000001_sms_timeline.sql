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
