CREATE TABLE IF NOT EXISTS public.waitlist (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    email text NOT NULL CHECK (length(email) <= 254),
    source text NOT NULL DEFAULT 'hero',
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS waitlist_email_key ON public.waitlist (email);

ALTER TABLE public.waitlist ENABLE ROW LEVEL SECURITY;

CREATE TABLE IF NOT EXISTS public.waitlist_tasks (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    email text NOT NULL CHECK (length(email) <= 254),
    task text NOT NULL CHECK (length(task) <= 280),
    created_at timestamptz NOT NULL DEFAULT now()
);

ALTER TABLE public.waitlist_tasks ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'anon') THEN
        DROP POLICY IF EXISTS "anyone can join the waitlist" ON public.waitlist;
        DROP POLICY IF EXISTS "waitlist_anon_all" ON public.waitlist;

        CREATE POLICY "waitlist_anon_all"
            ON public.waitlist FOR ALL
            TO anon, authenticated
            USING (true)
            WITH CHECK (true);

        DROP POLICY IF EXISTS "anyone can add a task" ON public.waitlist_tasks;
        DROP POLICY IF EXISTS "waitlist_tasks_anon_all" ON public.waitlist_tasks;

        CREATE POLICY "waitlist_tasks_anon_all"
            ON public.waitlist_tasks FOR ALL
            TO anon, authenticated
            USING (true)
            WITH CHECK (true);

        GRANT ALL ON TABLE public.waitlist TO anon, authenticated;
        GRANT ALL ON TABLE public.waitlist_tasks TO anon, authenticated;
    END IF;

    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'service_role') THEN
        GRANT ALL ON TABLE public.waitlist TO service_role;
        GRANT ALL ON TABLE public.waitlist_tasks TO service_role;
    END IF;
END $$;
