DROP VIEW IF EXISTS user_insights;
DROP VIEW IF EXISTS user_goals;
DROP VIEW IF EXISTS user_records;
DROP TABLE IF EXISTS records;

DROP TABLE IF EXISTS sms_processed;
DROP TABLE IF EXISTS sms_batches;

ALTER TABLE spans
    ADD COLUMN schema_id UUID REFERENCES data_schemas(id),
    ADD COLUMN source_event_id UUID REFERENCES inbound_events(id) ON DELETE SET NULL;

CREATE INDEX spans_user_schema_idx ON spans (user_id, schema_id, start_at DESC);
CREATE INDEX spans_data_gin ON spans USING gin (data jsonb_path_ops);

ALTER TABLE jobs DROP CONSTRAINT jobs_kind_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind_check CHECK (kind IN (
    'process_event', 'run_schedule', 'dispatch_action', 'summarize_conversation',
    'evaluate_span', 'execute_span'));
DELETE FROM jobs WHERE kind = 'process_sms_batch';
