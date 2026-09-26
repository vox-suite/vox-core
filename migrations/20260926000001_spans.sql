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
