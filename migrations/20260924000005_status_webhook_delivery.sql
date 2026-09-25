-- Status hints use an outbox so a committed state transition cannot be lost
-- when an API or worker process stops before delivery.
ALTER TABLE status_webhook_subscriptions
    DROP CONSTRAINT status_webhook_subscriptions_state_check;
ALTER TABLE status_webhook_subscriptions
    ADD CONSTRAINT status_webhook_subscriptions_state_check
    CHECK (state IN ('enabled', 'unhealthy', 'disabled'));

CREATE TABLE status_webhook_secrets (
    subscription_id UUID PRIMARY KEY REFERENCES status_webhook_subscriptions(id) ON DELETE CASCADE,
    ciphertext BYTEA NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE status_webhook_deliveries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subscription_id UUID NOT NULL REFERENCES status_webhook_subscriptions(id) ON DELETE CASCADE,
    event_cursor BIGINT NOT NULL REFERENCES status_events(cursor) ON DELETE CASCADE,
    state TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'sending', 'sent', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    available_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_until TIMESTAMPTZ,
    lease_token UUID,
    lease_owner TEXT,
    delivered_at TIMESTAMPTZ,
    last_http_status INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (subscription_id, event_cursor)
);
CREATE INDEX status_webhook_deliveries_ready_idx
    ON status_webhook_deliveries (available_at, id)
    WHERE state IN ('queued', 'sending');

CREATE FUNCTION enqueue_status_webhooks() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO status_webhook_deliveries (subscription_id, event_cursor)
    SELECT id, NEW.cursor FROM status_webhook_subscriptions
    WHERE user_context_id = NEW.user_context_id AND state = 'enabled';
    RETURN NEW;
END;
$$;
CREATE TRIGGER status_event_webhook_outbox AFTER INSERT ON status_events
    FOR EACH ROW EXECUTE FUNCTION enqueue_status_webhooks();

-- A provider event's replay identity is committed atomically with its
-- normalized execution transition; no raw provider payload is retained.
CREATE TABLE verified_integration_events (
    integration_external_key TEXT NOT NULL,
    external_account_hash TEXT NOT NULL,
    provider_event_id TEXT NOT NULL,
    execution_id UUID NOT NULL REFERENCES executions(id),
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (integration_external_key, external_account_hash, provider_event_id)
);

-- These are operational or credential records, never directly readable by a
-- host user. Only the trusted service role may access them.
ALTER TABLE status_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE status_webhook_subscriptions ENABLE ROW LEVEL SECURITY;
ALTER TABLE status_webhook_secrets ENABLE ROW LEVEL SECURITY;
ALTER TABLE status_webhook_deliveries ENABLE ROW LEVEL SECURITY;
ALTER TABLE verified_integration_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY service_role_status_events ON status_events
    FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY service_role_status_webhook_subscriptions ON status_webhook_subscriptions
    FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY service_role_status_webhook_secrets ON status_webhook_secrets
    FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY service_role_status_webhook_deliveries ON status_webhook_deliveries
    FOR ALL TO service_role USING (true) WITH CHECK (true);
CREATE POLICY service_role_verified_integration_events ON verified_integration_events
    FOR ALL TO service_role USING (true) WITH CHECK (true);
