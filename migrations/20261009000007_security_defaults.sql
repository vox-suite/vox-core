DO $$
DECLARE item record;
BEGIN
    FOR item IN SELECT tablename FROM pg_tables WHERE schemaname='public' LOOP
        EXECUTE format('ALTER TABLE public.%I ENABLE ROW LEVEL SECURITY',item.tablename);
        EXECUTE format('CREATE POLICY baseline_service_role ON public.%I FOR ALL TO service_role USING(true) WITH CHECK(true)',item.tablename);
    END LOOP;
END $$;
CREATE POLICY timeline_groups_read ON timeline_groups FOR SELECT TO authenticated USING(true);
CREATE POLICY timeline_event_types_read ON timeline_event_types FOR SELECT TO authenticated USING(owner_user_id IS NULL OR owner_user_id=auth.uid());
REVOKE ALL ON ALL TABLES IN SCHEMA public FROM anon,authenticated;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM anon,authenticated;
REVOKE EXECUTE ON ALL FUNCTIONS IN SCHEMA public FROM anon,authenticated;
GRANT SELECT ON timeline_groups,timeline_event_types TO authenticated;
GRANT ALL ON ALL TABLES IN SCHEMA public TO service_role;
GRANT ALL ON ALL SEQUENCES IN SCHEMA public TO service_role;
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO service_role;

DO $$
DECLARE relation record;
BEGIN
    FOR relation IN SELECT c.conrelid::regclass AS table_name,c.conname FROM pg_constraint c
        WHERE c.contype='f' AND c.connamespace='public'::regnamespace AND NOT c.condeferrable
    LOOP
        EXECUTE format('ALTER TABLE %s ALTER CONSTRAINT %I DEFERRABLE INITIALLY IMMEDIATE',relation.table_name,relation.conname);
    END LOOP;
END $$;
