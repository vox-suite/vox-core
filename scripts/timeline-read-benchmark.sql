\set ON_ERROR_STOP on
-- Synthetic fixtures only. Requires the full Core migrations and a database
-- named timeline_perf. No production credentials or data are needed.
DO $$ BEGIN
    IF current_database() <> 'timeline_perf' THEN
        RAISE EXCEPTION 'This benchmark requires the isolated timeline_perf database';
    END IF;
END $$;
INSERT INTO users(id) VALUES ('11111111-1111-4111-8111-111111111111') ON CONFLICT DO NOTHING;
INSERT INTO timeline_events(user_id,event_type_id,group_id,title,occurred_at,content)
SELECT '11111111-1111-4111-8111-111111111111', t.id,t.group_id,'Synthetic purchase',
       timestamptz '2026-01-01' + ((n % 180) * interval '1 day'),
       '{"is_spending":true,"merchant":"Synthetic Cafe"}'::jsonb
FROM generate_series(1,200000) n
CROSS JOIN (SELECT id,group_id FROM timeline_event_types WHERE value='transaction' AND owner_user_id IS NULL LIMIT 1) t
WHERE NOT EXISTS (SELECT 1 FROM timeline_events WHERE user_id='11111111-1111-4111-8111-111111111111');
ANALYZE timeline_events;
SET plan_cache_mode=force_generic_plan;
BEGIN;
DROP INDEX timeline_events_active_cursor_idx;
DROP INDEX timeline_events_active_group_cursor_idx;
CREATE INDEX timeline_events_user_active_occurred_idx ON timeline_events(user_id,occurred_at DESC) WHERE record_state='active';
PREPARE timeline_old(uuid,uuid,text,uuid,text,timestamptz,timestamptz,text,timestamptz,uuid,bigint,boolean,text,text) AS SELECT e.id, e.user_id, e.event_type_id, e.group_id, e.title, e.summary, e.occurred_at, e.ended_at, e.time_precision, e.source_timezone, e.content, e.record_state, e.confidence::float8 AS confidence, e.dedupe_key, e.revision, e.created_at, e.updated_at FROM timeline_events e JOIN timeline_groups g ON g.id = e.group_id JOIN timeline_event_types et ON et.id = e.event_type_id WHERE e.user_id = $1 AND ($2::uuid IS NULL OR e.group_id = $2) AND ($3::text IS NULL OR g.value = $3) AND ($4::uuid IS NULL OR e.event_type_id = $4) AND ($5::text IS NULL OR et.value = $5) AND ($6::timestamptz IS NULL OR e.occurred_at >= $6) AND ($7::timestamptz IS NULL OR e.occurred_at < $7) AND ($8::text IS NULL OR e.record_state = $8) AND ($9::timestamptz IS NULL OR (e.occurred_at, e.id) < ($9, $10)) AND (NOT coalesce($12::boolean,false) OR (et.value='transaction' AND e.content->>'is_spending'='true' AND lower(coalesce(e.content->>'status','')) NOT IN ('pending','failed','declined','authorization','authorized','attempted'))) AND ($13::text IS NULL OR strpos(lower(coalesce(e.content->>'merchant','')),lower($13))>0) AND ($14::text IS NULL OR lower(coalesce(e.content->>'category',''))=lower($14)) ORDER BY e.occurred_at DESC, e.id DESC LIMIT $11;
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE timeline_old('11111111-1111-4111-8111-111111111111',NULL,NULL,NULL,NULL,'2026-02-01','2026-02-02','active',NULL,NULL,31,NULL,NULL,NULL);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE timeline_old('11111111-1111-4111-8111-111111111111',NULL,NULL,NULL,NULL,'2026-02-01','2026-02-02','active',NULL,NULL,31,NULL,NULL,NULL);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE timeline_old('11111111-1111-4111-8111-111111111111',NULL,NULL,NULL,NULL,'2026-02-01','2026-02-02','active',NULL,NULL,31,NULL,NULL,NULL);
ROLLBACK;
PREPARE timeline_new(uuid,timestamptz,timestamptz,bigint) AS
SELECT e.id,e.user_id,e.event_type_id,e.group_id,e.title,e.summary,e.occurred_at,e.ended_at,e.time_precision,e.source_timezone,e.content,e.record_state,e.confidence::float8 AS confidence,e.dedupe_key,e.revision,e.created_at,e.updated_at
FROM timeline_events e WHERE e.user_id=$1 AND e.record_state='active' AND e.occurred_at >= $2 AND e.occurred_at < $3 ORDER BY e.occurred_at DESC,e.id DESC LIMIT $4;
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE timeline_new('11111111-1111-4111-8111-111111111111','2026-02-01','2026-02-02',31);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE timeline_new('11111111-1111-4111-8111-111111111111','2026-02-01','2026-02-02',31);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE timeline_new('11111111-1111-4111-8111-111111111111','2026-02-01','2026-02-02',31);
