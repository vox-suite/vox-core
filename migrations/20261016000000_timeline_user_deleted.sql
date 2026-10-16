ALTER TABLE timeline_events DROP CONSTRAINT IF EXISTS timeline_events_record_state_check;
ALTER TABLE timeline_events ADD CONSTRAINT timeline_events_record_state_check
    CHECK (record_state IN ('active', 'superseded', 'retracted', 'deleted'));
ALTER TABLE timeline_events ADD COLUMN IF NOT EXISTS deleted_at TIMESTAMPTZ;

CREATE OR REPLACE FUNCTION keep_timeline_event_deleted() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.record_state = 'deleted' AND NEW.user_id = OLD.user_id THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER timeline_events_keep_deleted BEFORE UPDATE ON timeline_events
    FOR EACH ROW EXECUTE FUNCTION keep_timeline_event_deleted();
