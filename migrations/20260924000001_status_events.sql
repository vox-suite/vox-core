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
