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
