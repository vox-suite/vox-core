-- Disposable benchmark fixture. Run after examples/durable_performance.rs creates
-- a synthetic host. The database-name guard prevents accidental use elsewhere.
DO $$ BEGIN
  IF current_database() <> 'voxcore_perf' THEN
    RAISE EXCEPTION 'seed-queue.sql requires database voxcore_perf';
  END IF;
END $$;

-- Supply the desired row count with psql -v rows=10000.
WITH owner AS (
  SELECT user_id, id AS user_context_id
  FROM user_contexts
  ORDER BY created_at DESC
  LIMIT 1
), new_tasks AS (
  INSERT INTO tasks (id, user_id, user_context_id, title, instruction, status, execution_type)
  SELECT gen_random_uuid(), owner.user_id, owner.user_context_id,
         'Synthetic queue task', 'Local query-plan fixture', 'pending', 'interactive'
  FROM owner CROSS JOIN generate_series(1, :rows)
  RETURNING id, user_id, user_context_id
)
INSERT INTO jobs (user_id, user_context_id, kind, task_id, payload_reference_id, state)
SELECT user_id, user_context_id, 'execute_task', id, id, 'pending'
FROM new_tasks;

ANALYZE tasks;
ANALYZE jobs;
ANALYZE status_events;
