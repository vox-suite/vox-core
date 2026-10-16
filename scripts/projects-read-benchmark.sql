\set ON_ERROR_STOP on
-- Disposable database only; full Core migrations must already be applied.
DO $$ BEGIN
 IF current_database() <> 'projects_perf' THEN
  RAISE EXCEPTION 'Use an isolated projects_perf database';
 END IF;
END $$;
INSERT INTO users(id) VALUES ('22222222-2222-4222-8222-222222222222') ON CONFLICT DO NOTHING;
INSERT INTO projects(id,user_id,name,updated_at,data)
SELECT md5('projects-benchmark-'||n)::uuid,'22222222-2222-4222-8222-222222222222',
       'Synthetic project '||n, timestamptz '2026-01-01' + n * interval '1 minute',
       jsonb_build_object('template','engineering','notes','Synthetic project summary','sources','[]'::jsonb,
        'nodes',(SELECT jsonb_agg(jsonb_build_object('id',md5(n||':'||k)::uuid,'kind','component','title','Component '||k,'body',(SELECT string_agg(md5(n||':'||k||':'||j),'') FROM generate_series(1,16) j),'origin','extracted','updated_at','2026-01-01T00:00:00Z')) FROM generate_series(1,32) k),
        'messages',(SELECT jsonb_agg(jsonb_build_object('id',md5(n||':message:'||k)::uuid,'role','assistant','body',(SELECT string_agg(md5(n||':message:'||k||':'||j),'') FROM generate_series(1,16) j),'at','2026-01-01T00:00:00Z')) FROM generate_series(1,32) k),
        'activity','[]'::jsonb)
FROM generate_series(1,2000) n
WHERE NOT EXISTS(SELECT 1 FROM projects WHERE user_id='22222222-2222-4222-8222-222222222222');
ANALYZE projects;
SELECT count(*) AS projects, round(avg(pg_column_size(data))) AS stored_json_bytes,
       round(avg(octet_length(data::text))) AS expanded_json_bytes
FROM projects WHERE user_id='22222222-2222-4222-8222-222222222222';
SET plan_cache_mode=force_generic_plan;
PREPARE projects_old(uuid,uuid) AS
SELECT id,user_id,name,(data - 'nodes' - 'messages' - 'activity') AS data,revision,created_at,updated_at
FROM projects WHERE user_id=$1 AND NOT archived
AND ($2::uuid IS NULL OR (updated_at,id)<(SELECT updated_at,id FROM projects WHERE id=$2 AND user_id=$1))
ORDER BY updated_at DESC,id DESC LIMIT 50;
PREPARE projects_new(uuid,uuid) AS
SELECT id,user_id,name,(data - ARRAY['nodes','messages','activity']) AS data,revision,created_at,updated_at
FROM projects WHERE user_id=$1 AND NOT archived
AND (updated_at,id)<(SELECT updated_at,id FROM projects WHERE id=$2 AND user_id=$1)
ORDER BY updated_at DESC,id DESC LIMIT 51;
\echo OLD_DEEP_PAGE
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE projects_old('22222222-2222-4222-8222-222222222222',md5('projects-benchmark-100')::uuid);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE projects_old('22222222-2222-4222-8222-222222222222',md5('projects-benchmark-100')::uuid);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE projects_old('22222222-2222-4222-8222-222222222222',md5('projects-benchmark-100')::uuid);
\echo NEW_DEEP_PAGE
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE projects_new('22222222-2222-4222-8222-222222222222',md5('projects-benchmark-100')::uuid);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE projects_new('22222222-2222-4222-8222-222222222222',md5('projects-benchmark-100')::uuid);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE projects_new('22222222-2222-4222-8222-222222222222',md5('projects-benchmark-100')::uuid);
\echo FULL_PROJECT_FOR_AUTHORIZATION
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) SELECT * FROM projects WHERE user_id='22222222-2222-4222-8222-222222222222' AND id=md5('projects-benchmark-100')::uuid AND NOT archived;
\echo OWNERSHIP_EXISTS
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) SELECT EXISTS(SELECT 1 FROM projects WHERE user_id='22222222-2222-4222-8222-222222222222' AND id=md5('projects-benchmark-100')::uuid AND NOT archived);
-- EXPLAIN does not measure row transfer or Rust JSON decoding. See repository benchmark.

\echo FIRST_PAGE_PROJECTION_COMPARISON
PREPARE projects_first_new(uuid) AS
SELECT id,user_id,name,(data - ARRAY['nodes','messages','activity']) AS data,revision,created_at,updated_at
FROM projects WHERE user_id=$1 AND NOT archived ORDER BY updated_at DESC,id DESC LIMIT 51;
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE projects_old('22222222-2222-4222-8222-222222222222',NULL);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE projects_old('22222222-2222-4222-8222-222222222222',NULL);
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE projects_first_new('22222222-2222-4222-8222-222222222222');
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF) EXECUTE projects_first_new('22222222-2222-4222-8222-222222222222');
\echo COUNT_AND_REVISION
EXPLAIN (ANALYZE,BUFFERS,TIMING OFF)
SELECT count(*)::bigint AS count,coalesce(md5(string_agg(id::text||':'||revision::text,',' ORDER BY id)),'empty') AS rev
FROM projects WHERE user_id='22222222-2222-4222-8222-222222222222' AND NOT archived;
