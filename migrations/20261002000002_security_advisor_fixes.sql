ALTER TABLE IF EXISTS public._sqlx_migrations ENABLE ROW LEVEL SECURITY;

CREATE SCHEMA IF NOT EXISTS extensions;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_extension e
        JOIN pg_namespace n ON n.oid = e.extnamespace
        WHERE e.extname = 'vector' AND n.nspname = 'public'
    ) THEN
        ALTER EXTENSION vector SET SCHEMA extensions;
    END IF;
END
$$;

DO $$
DECLARE
    fn record;
BEGIN
    FOR fn IN
        SELECT p.oid::regprocedure AS signature
        FROM pg_proc p
        JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname = 'public'
          AND p.prokind = 'f'
          AND NOT EXISTS (
              SELECT 1 FROM pg_depend d
              WHERE d.classid = 'pg_proc'::regclass
                AND d.objid = p.oid
                AND d.deptype = 'e'
          )
          AND NOT EXISTS (
              SELECT 1 FROM unnest(COALESCE(p.proconfig, ARRAY[]::text[])) AS setting
              WHERE setting LIKE 'search_path=%'
          )
    LOOP
        EXECUTE format('ALTER FUNCTION %s SET search_path = public, pg_temp', fn.signature);
    END LOOP;
END
$$;
