\set ON_ERROR_STOP on
-- Run only on a disposable database after all Core migrations.
DO $$ BEGIN
 IF current_database() <> 'spaces_insights_perf' THEN
  RAISE EXCEPTION 'Use the isolated spaces_insights_perf database';
 END IF;
END $$;
INSERT INTO users(id) VALUES ('33333333-3333-4333-8333-333333333333') ON CONFLICT DO NOTHING;
INSERT INTO spaces(id,user_id,title,intent,state,agent_spec,created_at,updated_at)
SELECT md5('spaces-benchmark-'||n)::uuid,'33333333-3333-4333-8333-333333333333',
 'Synthetic Plan '||n,'Synthetic planning intent',CASE WHEN n%10=0 THEN 'dropped' WHEN n%3=0 THEN 'committed' ELSE 'ideating' END,
 jsonb_build_object('context',(SELECT string_agg(md5(n||':'||k),'') FROM generate_series(1,256) k)),
 timestamptz '2026-01-01'+n*interval '1 minute',timestamptz '2026-02-01'-n*interval '1 minute'
FROM generate_series(1,10000) n
WHERE NOT EXISTS(SELECT 1 FROM spaces WHERE user_id='33333333-3333-4333-8333-333333333333');
ANALYZE spaces;
SELECT count(*) AS spaces,round(avg(octet_length(agent_spec::text))) AS expanded_spec_bytes FROM spaces WHERE user_id='33333333-3333-4333-8333-333333333333';
SET plan_cache_mode=force_generic_plan;
BEGIN;
DROP INDEX spaces_active_created_cursor_idx;
DROP INDEX spaces_active_updated_cursor_idx;
PREPARE spaces_old(uuid,bigint,bigint,text,text) AS
SELECT id,user_id,title,intent,state,agent_spec,committed_collection_id,project_id,run_state,run_error,created_at,updated_at
FROM spaces WHERE user_id=$1 AND state!='dropped'
AND ($4::text IS NULL OR strpos(lower(title||' '||intent),lower($4))>0)
AND ($5::text IS NULL OR ($5='committed' AND state='committed') OR ($5='active' AND state!='committed'))
ORDER BY updated_at DESC,id DESC LIMIT $2 OFFSET $3;
\echo OLD_UPDATED_LIST
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE spaces_old('33333333-3333-4333-8333-333333333333',25,0,NULL,NULL);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE spaces_old('33333333-3333-4333-8333-333333333333',25,0,NULL,NULL);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE spaces_old('33333333-3333-4333-8333-333333333333',25,0,NULL,NULL);
ROLLBACK;
PREPARE spaces_new(uuid,bigint,bigint) AS
SELECT id,user_id,title,intent,state,'{}'::jsonb AS agent_spec,committed_collection_id,project_id,run_state,run_error,created_at,updated_at
FROM spaces WHERE user_id=$1 AND state!='dropped' ORDER BY updated_at DESC,id DESC LIMIT $2 OFFSET $3;
\echo NEW_UPDATED_SUMMARIES
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE spaces_new('33333333-3333-4333-8333-333333333333',25,0);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE spaces_new('33333333-3333-4333-8333-333333333333',25,0);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE spaces_new('33333333-3333-4333-8333-333333333333',25,0);
-- Insights rollup/cache statement counts are measured by the companion Rust test
-- using pg_stat_statements, not by extrapolating these Spaces plans.
