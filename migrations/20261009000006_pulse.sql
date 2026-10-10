CREATE TABLE pulse_charts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    idempotency_key UUID NOT NULL,
    UNIQUE (user_id, idempotency_key),
    title TEXT NOT NULL CHECK (length(btrim(title)) > 0),
    chart_type TEXT NOT NULL CHECK (length(btrim(chart_type)) > 0),
    definition JSONB NOT NULL DEFAULT '{}'::jsonb,
    sort_order INTEGER NOT NULL DEFAULT 0,
    is_pinned BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pulse_charts_id_user_key UNIQUE (id, user_id),
    CONSTRAINT pulse_charts_definition_is_object CHECK (jsonb_typeof(definition) = 'object')
);

CREATE TABLE pulse_dismissals (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    suggestion_key TEXT NOT NULL CHECK (length(btrim(suggestion_key)) > 0),
    dismissed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, suggestion_key)
);

CREATE TABLE pulse_daily_aggregates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_id UUID NOT NULL REFERENCES timeline_groups(id) ON DELETE CASCADE,
    event_type_id UUID NOT NULL REFERENCES timeline_event_types(id) ON DELETE CASCADE,
    day DATE NOT NULL,
    metric_key TEXT NOT NULL CHECK (length(btrim(metric_key)) > 0),
    aggregate_value NUMERIC NOT NULL DEFAULT 0,
    count_events BIGINT NOT NULL DEFAULT 0,
    timezone TEXT NOT NULL,
    currency TEXT NOT NULL DEFAULT '',
    dimension_value TEXT NOT NULL DEFAULT '',
    data_revision BIGINT NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pulse_daily_aggregates_user_day_metric_uniq UNIQUE (user_id, day, metric_key, group_id, event_type_id, timezone, currency, dimension_value),
    CONSTRAINT pulse_daily_aggregates_id_user_key UNIQUE (id, user_id),
    CONSTRAINT pulse_daily_aggregates_metadata_is_object CHECK (jsonb_typeof(metadata) = 'object')
);

CREATE INDEX pulse_daily_aggregates_user_day_idx ON pulse_daily_aggregates (user_id, day DESC);
CREATE INDEX pulse_daily_aggregates_user_metric_idx ON pulse_daily_aggregates (user_id, metric_key, day DESC);

CREATE TABLE pulse_invalidations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    reason TEXT NOT NULL CHECK (length(btrim(reason)) > 0),
    range_start TIMESTAMPTZ,
    range_end TIMESTAMPTZ,
    processed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT pulse_invalidations_id_user_key UNIQUE (id, user_id)
);

CREATE TABLE pulse_revisions (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    data_revision BIGINT NOT NULL DEFAULT 0,
    discovery_revision BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE pulse_cache (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    cache_key TEXT NOT NULL,
    payload JSONB NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, cache_key)
);

CREATE FUNCTION pulse_bump_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO pulse_revisions(user_id,data_revision,discovery_revision)
    SELECT DISTINCT user_id,1,1 FROM changed WHERE user_id IN (SELECT id FROM users)
    ON CONFLICT(user_id) DO UPDATE SET
      data_revision=pulse_revisions.data_revision+1,
      discovery_revision=pulse_revisions.discovery_revision+1,updated_at=now();
    INSERT INTO pulse_invalidations(user_id,reason,range_start,range_end)
    SELECT user_id,'timeline_change',min(occurred_at),max(occurred_at) FROM changed
    WHERE user_id IN (SELECT id FROM users) GROUP BY user_id;
    PERFORM pg_notify('vox_timeline_updated',json_build_object('user_id',user_id,'type','timeline_updated')::text) FROM (SELECT DISTINCT user_id FROM changed) affected;
    RETURN NULL;
END $$;
CREATE TRIGGER pulse_timeline_insert AFTER INSERT ON timeline_events
    REFERENCING NEW TABLE AS changed FOR EACH STATEMENT EXECUTE FUNCTION pulse_bump_revision();
CREATE TRIGGER pulse_timeline_update AFTER UPDATE ON timeline_events
    REFERENCING NEW TABLE AS changed FOR EACH STATEMENT EXECUTE FUNCTION pulse_bump_revision();
CREATE TRIGGER pulse_timeline_delete AFTER DELETE ON timeline_events
    REFERENCING OLD TABLE AS changed FOR EACH STATEMENT EXECUTE FUNCTION pulse_bump_revision();

CREATE INDEX IF NOT EXISTS timeline_events_user_active_occurred_idx ON timeline_events (user_id, occurred_at DESC) WHERE record_state = 'active';

