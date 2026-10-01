ALTER TABLE inbound_events
    ADD COLUMN batch_id UUID,
    ADD COLUMN failed_at TIMESTAMPTZ,
    ADD COLUMN retryable BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN requeue_count INTEGER NOT NULL DEFAULT 0;

CREATE INDEX inbound_events_batch_idx ON inbound_events (batch_id) WHERE batch_id IS NOT NULL;
CREATE INDEX inbound_events_requeue_idx ON inbound_events (failed_at) WHERE processing_error IS NOT NULL AND retryable;

ALTER TABLE jobs DROP CONSTRAINT jobs_kind_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind_check CHECK (kind IN (
    'process_event', 'process_event_batch', 'run_schedule', 'dispatch_action', 'summarize_conversation',
    'evaluate_span', 'execute_span', 'run_space'));

UPDATE inbound_events
SET retryable = true, failed_at = now()
WHERE processing_error ~ '^(triage_failed|classify_failed|system_two_failed|system_two_validation_failed|schema_upsert_failed)';

UPDATE spans SET source = 'sms' WHERE source = 'sms_message';
