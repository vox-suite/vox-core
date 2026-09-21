CREATE TABLE status_events (
    cursor BIGSERIAL PRIMARY KEY,
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    aggregate_type TEXT NOT NULL CHECK (aggregate_type IN ('task','run','execution')),
    aggregate_id UUID NOT NULL,
    event_type TEXT NOT NULL,
    state TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    committed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deduplication_key TEXT NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT status_events_payload_object CHECK (jsonb_typeof(payload) = 'object'),
    CONSTRAINT status_events_deduplication UNIQUE (user_context_id, deduplication_key)
);
CREATE INDEX status_events_context_cursor_idx ON status_events (user_context_id, cursor);

CREATE TABLE status_webhook_subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    endpoint TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'enabled' CHECK (state IN ('enabled','disabled','unhealthy')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX status_webhook_subscriptions_context_idx ON status_webhook_subscriptions (user_context_id, state);

CREATE TABLE status_webhook_deliveries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id UUID NOT NULL REFERENCES status_webhook_subscriptions(id) ON DELETE RESTRICT,
    status_cursor BIGINT NOT NULL REFERENCES status_events(cursor) ON DELETE RESTRICT,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','leased','delivered','failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0 AND attempts <= 8),
    lease_owner TEXT,
    lease_expires_at TIMESTAMPTZ,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT status_webhook_deliveries_once UNIQUE (subscription_id,status_cursor)
);
CREATE INDEX status_webhook_deliveries_claim_idx ON status_webhook_deliveries (state,next_attempt_at);

-- Provider events are an adapter-only replay ledger. They deliberately carry
-- no raw body, credentials, or user-visible content.
CREATE TABLE integration_external_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    execution_id UUID NOT NULL REFERENCES executions(id) ON DELETE RESTRICT,
    integration_external_key TEXT NOT NULL,
    provider_event_id TEXT NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT integration_external_events_key_not_empty CHECK (
        length(btrim(integration_external_key)) BETWEEN 1 AND 255
        AND length(btrim(provider_event_id)) BETWEEN 1 AND 255
    ),
    CONSTRAINT integration_external_events_dedup UNIQUE (integration_external_key, provider_event_id)
);
CREATE INDEX integration_external_events_execution_idx ON integration_external_events (execution_id, received_at);

CREATE OR REPLACE FUNCTION enqueue_status_webhook_deliveries() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO status_webhook_deliveries (subscription_id,status_cursor)
  SELECT id,NEW.cursor FROM status_webhook_subscriptions
  WHERE user_context_id=NEW.user_context_id AND state='enabled';
  RETURN NEW;
END; $$;
CREATE TRIGGER status_event_delivery_outbox AFTER INSERT ON status_events FOR EACH ROW EXECUTE FUNCTION enqueue_status_webhook_deliveries();

CREATE OR REPLACE FUNCTION append_task_status_event() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'INSERT' OR NEW.status IS DISTINCT FROM OLD.status THEN
    INSERT INTO status_events (user_context_id,aggregate_type,aggregate_id,event_type,state,occurred_at,deduplication_key)
    VALUES (NEW.user_context_id,'task',NEW.id,'task.state_changed',NEW.status,now(),gen_random_uuid()::text);
  END IF;
  RETURN NEW;
END; $$;
CREATE TRIGGER task_status_event AFTER INSERT OR UPDATE OF status ON tasks FOR EACH ROW EXECUTE FUNCTION append_task_status_event();

CREATE OR REPLACE FUNCTION append_run_status_event() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE context_id UUID;
BEGIN
  IF TG_OP = 'INSERT' OR NEW.state IS DISTINCT FROM OLD.state THEN
    SELECT user_context_id INTO context_id FROM tasks WHERE id=NEW.task_id;
    INSERT INTO status_events (user_context_id,aggregate_type,aggregate_id,event_type,state,occurred_at,deduplication_key)
    VALUES (context_id,'run',NEW.id,'run.state_changed',NEW.state,now(),gen_random_uuid()::text);
  END IF;
  RETURN NEW;
END; $$;
CREATE TRIGGER run_status_event AFTER INSERT OR UPDATE OF state ON task_runs FOR EACH ROW EXECUTE FUNCTION append_run_status_event();

CREATE OR REPLACE FUNCTION append_execution_status_event() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'INSERT' OR NEW.state IS DISTINCT FROM OLD.state THEN
    INSERT INTO status_events (user_context_id,aggregate_type,aggregate_id,event_type,state,occurred_at,deduplication_key)
    VALUES (NEW.user_context_id,'execution',NEW.id,'execution.state_changed',NEW.state,now(),gen_random_uuid()::text);
  END IF;
  RETURN NEW;
END; $$;
CREATE TRIGGER execution_status_event AFTER INSERT OR UPDATE OF state ON executions FOR EACH ROW EXECUTE FUNCTION append_execution_status_event();
