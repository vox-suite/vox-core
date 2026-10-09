CREATE TABLE timeline_groups (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    value TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    ui_hint JSONB NOT NULL DEFAULT '{}'::jsonb,
    sort_order INTEGER NOT NULL DEFAULT 0,
    CONSTRAINT timeline_groups_value_not_empty CHECK (length(btrim(value)) > 0),
    CONSTRAINT timeline_groups_ui_hint_is_object CHECK (jsonb_typeof(ui_hint) = 'object')
);

CREATE TABLE timeline_event_types (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    value TEXT NOT NULL CHECK (length(btrim(value)) > 0),
    version INTEGER NOT NULL DEFAULT 1 CHECK (version > 0),
    label TEXT NOT NULL CHECK (length(btrim(label)) > 0),
    group_id UUID NOT NULL REFERENCES timeline_groups(id) ON DELETE RESTRICT,
    description TEXT NOT NULL DEFAULT '',
    content_schema JSONB NOT NULL CHECK (jsonb_typeof(content_schema) = 'object'),
    analytics_definition JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(analytics_definition) = 'object'),
    ui_hint JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(ui_hint) = 'object'),
    state TEXT NOT NULL DEFAULT 'published' CHECK (state IN ('draft', 'published', 'deprecated')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT timeline_event_types_id_group_uniq UNIQUE (id, group_id)
);

CREATE UNIQUE INDEX timeline_event_types_global_uniq
    ON timeline_event_types (value, version)
    WHERE owner_user_id IS NULL;

CREATE UNIQUE INDEX timeline_event_types_user_uniq
    ON timeline_event_types (owner_user_id, value, version)
    WHERE owner_user_id IS NOT NULL;

CREATE OR REPLACE FUNCTION immutable_published_event_type() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.state = 'published' AND (NEW.content_schema IS DISTINCT FROM OLD.content_schema OR NEW.analytics_definition IS DISTINCT FROM OLD.analytics_definition OR NEW.value IS DISTINCT FROM OLD.value OR NEW.version IS DISTINCT FROM OLD.version OR NEW.group_id IS DISTINCT FROM OLD.group_id OR NEW.owner_user_id IS DISTINCT FROM OLD.owner_user_id) THEN
        RAISE EXCEPTION 'published timeline event type schemas are immutable';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER event_type_published_immutable BEFORE UPDATE ON timeline_event_types
    FOR EACH ROW EXECUTE FUNCTION immutable_published_event_type();

CREATE TABLE timeline_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    event_type_id UUID NOT NULL,
    group_id UUID NOT NULL,
    title TEXT NOT NULL CHECK (length(btrim(title)) > 0),
    summary TEXT,
    occurred_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ,
    time_precision TEXT NOT NULL DEFAULT 'second' CHECK (time_precision IN ('year', 'month', 'day', 'hour', 'minute', 'second', 'millisecond')),
    source_timezone TEXT,
    content JSONB NOT NULL DEFAULT '{}'::jsonb,
    record_state TEXT NOT NULL DEFAULT 'active' CHECK (record_state IN ('active', 'superseded', 'retracted')),
    confidence NUMERIC NOT NULL DEFAULT 1.0 CHECK (confidence >= 0 AND confidence <= 1),
    dedupe_key TEXT,
    revision BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT timeline_events_id_user_key UNIQUE (id, user_id),
    CONSTRAINT timeline_events_content_is_object CHECK (jsonb_typeof(content) = 'object'),
    CONSTRAINT timeline_events_type_group_fk FOREIGN KEY (event_type_id, group_id) REFERENCES timeline_event_types(id, group_id) ON DELETE RESTRICT,
    CONSTRAINT timeline_events_group_fk FOREIGN KEY (group_id) REFERENCES timeline_groups(id) ON DELETE RESTRICT,
    CONSTRAINT timeline_events_time_order CHECK (ended_at IS NULL OR ended_at >= occurred_at)
);

CREATE UNIQUE INDEX timeline_events_user_dedupe_uniq
    ON timeline_events (user_id, dedupe_key)
    WHERE dedupe_key IS NOT NULL;

CREATE INDEX timeline_events_user_occurred_idx ON timeline_events (user_id, occurred_at DESC);
CREATE INDEX timeline_events_user_group_occurred_idx ON timeline_events (user_id, group_id, occurred_at DESC);
CREATE INDEX timeline_events_user_type_occurred_idx ON timeline_events (user_id, event_type_id, occurred_at DESC);
CREATE INDEX timeline_events_user_state_idx ON timeline_events (user_id, record_state);

CREATE OR REPLACE FUNCTION validate_timeline_event_scope() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    type_owner UUID;
BEGIN
    SELECT owner_user_id INTO type_owner FROM timeline_event_types WHERE id = NEW.event_type_id;
    IF type_owner IS NOT NULL AND type_owner <> NEW.user_id THEN
        RAISE EXCEPTION 'timeline event user_id does not match event_type owner';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER timeline_event_scope_check BEFORE INSERT OR UPDATE ON timeline_events
    FOR EACH ROW EXECUTE FUNCTION validate_timeline_event_scope();

CREATE TABLE timeline_evidence (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    timeline_event_id UUID NOT NULL,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    source_record_id UUID,
    source_attachment_id UUID,
    source_type TEXT NOT NULL,
    source_id TEXT,
    raw_reference TEXT,
    observation_metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT timeline_evidence_id_user_key UNIQUE (id, user_id),
    CONSTRAINT timeline_evidence_event_tenant_fk
        FOREIGN KEY (timeline_event_id, user_id) REFERENCES timeline_events(id, user_id) ON DELETE CASCADE,
    CONSTRAINT timeline_evidence_source_record_tenant_fk
        FOREIGN KEY (source_record_id, user_id) REFERENCES source_records(id, user_id) ON DELETE SET NULL (source_record_id),
    CONSTRAINT timeline_evidence_source_attachment_tenant_fk
        FOREIGN KEY (source_attachment_id, user_id) REFERENCES source_attachments(id, user_id) ON DELETE SET NULL (source_attachment_id)
);

CREATE INDEX timeline_evidence_event_idx ON timeline_evidence (timeline_event_id);
CREATE INDEX timeline_evidence_user_idx ON timeline_evidence (user_id);

CREATE TABLE timeline_revisions (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE OR REPLACE FUNCTION bump_timeline_revisions() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO timeline_revisions (user_id, revision, updated_at)
    SELECT DISTINCT user_id, 1, now() FROM changed
    ON CONFLICT (user_id) DO UPDATE SET revision = timeline_revisions.revision + 1, updated_at = now();
    RETURN NULL;
END $$;

CREATE TRIGGER timeline_events_rev_insert AFTER INSERT ON timeline_events
    REFERENCING NEW TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION bump_timeline_revisions();

CREATE TRIGGER timeline_events_rev_update AFTER UPDATE ON timeline_events
    REFERENCING NEW TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION bump_timeline_revisions();

CREATE TRIGGER timeline_events_rev_delete AFTER DELETE ON timeline_events
    REFERENCING OLD TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION bump_timeline_revisions();

INSERT INTO timeline_groups (value, label, ui_hint, sort_order) VALUES
('finance', 'Finance', '{"color":"emerald","icon":"wallet"}'::jsonb, 1),
('activity', 'Activity', '{"color":"orange","icon":"map-pin"}'::jsonb, 2),
('entertainment', 'Entertainment', '{"color":"purple","icon":"tv"}'::jsonb, 3),
('work', 'Work', '{"color":"blue","icon":"briefcase"}'::jsonb, 4),
('health', 'Health', '{"color":"rose","icon":"heart"}'::jsonb, 5),
('personal', 'Personal', '{"color":"amber","icon":"user"}'::jsonb, 6)
ON CONFLICT (value) DO NOTHING;

INSERT INTO timeline_event_types (value, version, label, group_id, description, content_schema, analytics_definition, ui_hint, state)
SELECT
    seed.val,
    1,
    seed.lbl,
    g.id,
    seed.descr,
    '{"type":"object","properties":{"amount":{"type":["number","null"]},"total_amount":{"type":["number","null"]},"currency":{"type":["string","null"],"pattern":"^[A-Z]{3}$"},"direction":{"type":["string","null"]},"is_spending":{"type":"boolean"},"merchant":{"type":["string","null"],"maxLength":500},"reference":{"type":["string","null"],"maxLength":256},"account_hint":{"type":["string","null"]},"duration_seconds":{"type":["number","null"],"minimum":0},"distance_meters":{"type":["number","null"],"minimum":0},"calories":{"type":["number","null"],"minimum":0},"game":{"type":["string","null"]},"platform":{"type":["string","null"]},"artist":{"type":["string","null"]},"album":{"type":["string","null"]},"track":{"type":["string","null"]},"channel":{"type":["string","null"]},"restaurant_name":{"type":["string","null"]},"status":{"type":["string","null"]},"place_name":{"type":["string","null"]},"activity_type":{"type":["string","null"]},"timing":{"type":["string","null"]}},"maxProperties":150}'::jsonb || CASE WHEN seed.val IN ('transaction','refund','transfer','bill','statement','repayment') THEN '{"required":["amount","is_spending","direction"]}'::jsonb ELSE '{}'::jsonb END,
    CASE seed.val
        WHEN 'transaction' THEN '{"metrics":[{"key":"spending","field":"amount","unit":"currency","aggregation":"sum","title":"Spending"}],"recommended_chart":"bar"}'::jsonb
        WHEN 'refund' THEN '{"metrics":[{"key":"refund","field":"amount","unit":"currency","aggregation":"sum","title":"Refunds"}],"recommended_chart":"bar"}'::jsonb
        WHEN 'order' THEN '{"metrics":[{"key":"order_count","field":null,"unit":"orders","aggregation":"count","title":"Orders"},{"key":"order_total","field":"total_amount","unit":"currency","aggregation":"sum","title":"Order Spend"}],"recommended_chart":"bar"}'::jsonb
        WHEN 'workout' THEN '{"metrics":[{"key":"workout_count","field":null,"unit":"workouts","aggregation":"count","title":"Workouts"},{"key":"duration_seconds","field":"duration_seconds","unit":"seconds","aggregation":"sum","title":"Active Time"},{"key":"calories","field":"calories","unit":"kcal","aggregation":"sum","title":"Calories Burned"}],"recommended_chart":"bar"}'::jsonb
        WHEN 'music' THEN '{"metrics":[{"key":"play_count","field":null,"unit":"plays","aggregation":"count","title":"Tracks Played"},{"key":"duration_seconds","field":"duration_seconds","unit":"seconds","aggregation":"sum","title":"Listening Duration"}],"recommended_chart":"bar"}'::jsonb
        WHEN 'gaming' THEN '{"metrics":[{"key":"session_count","field":null,"unit":"sessions","aggregation":"count","title":"Gaming Sessions"},{"key":"duration_seconds","field":"duration_seconds","unit":"seconds","aggregation":"sum","title":"Play Time"}],"recommended_chart":"bar"}'::jsonb
        WHEN 'video_watch' THEN '{"metrics":[{"key":"watch_count","field":null,"unit":"views","aggregation":"count","title":"Videos Watched"},{"key":"duration_seconds","field":"duration_seconds","unit":"seconds","aggregation":"sum","title":"Watch Time"}],"recommended_chart":"bar"}'::jsonb
        WHEN 'visit' THEN '{"metrics":[{"key":"visit_count","field":null,"unit":"visits","aggregation":"count","title":"Visits"}],"recommended_chart":"bar"}'::jsonb
        ELSE '{"metrics":[{"key":"count","field":null,"unit":"events","aggregation":"count","title":"Count"}],"recommended_chart":"bar"}'::jsonb
    END || jsonb_build_object('dimensions',CASE seed.val
        WHEN 'transaction' THEN '["merchant","direction"]'::jsonb
        WHEN 'refund' THEN '["merchant"]'::jsonb
        WHEN 'order' THEN '["restaurant_name","status"]'::jsonb
        WHEN 'music' THEN '["artist","album","track"]'::jsonb
        WHEN 'gaming' THEN '["game","platform"]'::jsonb
        WHEN 'video_watch' THEN '["channel"]'::jsonb
        WHEN 'visit' THEN '["place_name"]'::jsonb
        WHEN 'travel' THEN '["activity_type"]'::jsonb
        ELSE '[]'::jsonb END),
    '{}'::jsonb,
    'published'
FROM (VALUES
    ('transaction', 'Transaction', 'finance', 'Financial transaction purchase, debit, or credit'),
    ('refund', 'Refund', 'finance', 'Refund or payment reversal'),
    ('transfer', 'Transfer', 'finance', 'Account or peer to peer transfer'),
    ('bill', 'Bill', 'finance', 'Upcoming or overdue bill or invoice'),
    ('statement', 'Statement', 'finance', 'Financial account summary or statement'),
    ('repayment', 'Repayment', 'finance', 'Loan or debt repayment'),
    ('order', 'Order', 'activity', 'Merchant or ecommerce purchase order'),
    ('delivery', 'Delivery', 'activity', 'Package or item delivery status'),
    ('visit', 'Visit', 'activity', 'Physical place or venue visit'),
    ('travel', 'Travel', 'activity', 'Flight, transit, or trip travel event'),
    ('workout', 'Workout', 'health', 'Physical exercise or workout activity'),
    ('appointment', 'Appointment', 'work', 'Scheduled meeting or appointment'),
    ('work', 'Work Task', 'work', 'Work session or professional task'),
    ('personal', 'Personal Event', 'personal', 'Personal log or diary entry'),
    ('music', 'Music Listen', 'entertainment', 'Music playback or listening session'),
    ('gaming', 'Game Session', 'entertainment', 'Gaming session or trophy unlock'),
    ('video_watch', 'Video Watch', 'entertainment', 'Movie, show, or stream viewing'),
    ('video_like', 'Video Like', 'entertainment', 'Explicit video like; does not establish a watch'),
    ('video_playlist_addition', 'Playlist Addition', 'entertainment', 'Video added to a playlist; does not establish a watch')
) AS seed(val, lbl, grp_val, descr)
JOIN timeline_groups g ON g.value = seed.grp_val
WHERE NOT EXISTS (
    SELECT 1 FROM timeline_event_types existing
    WHERE existing.owner_user_id IS NULL AND existing.value = seed.val AND existing.version = 1
);
