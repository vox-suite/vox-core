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
        IF NOT EXISTS (
            SELECT 1 FROM pg_policies WHERE schemaname = 'public' AND tablename = 'waitlist' AND policyname = 'anyone can join the waitlist'
        ) THEN
            CREATE POLICY "anyone can join the waitlist"
                ON public.waitlist FOR INSERT
                TO anon
                WITH CHECK (true);
        END IF;

        IF NOT EXISTS (
            SELECT 1 FROM pg_policies WHERE schemaname = 'public' AND tablename = 'waitlist_tasks' AND policyname = 'anyone can add a task'
        ) THEN
            CREATE POLICY "anyone can add a task"
                ON public.waitlist_tasks FOR INSERT
                TO anon
                WITH CHECK (true);
        END IF;

        GRANT SELECT, INSERT ON TABLE public.waitlist TO anon;
        GRANT SELECT, INSERT ON TABLE public.waitlist_tasks TO anon;
    END IF;

    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'service_role') THEN
        GRANT ALL ON TABLE public.waitlist TO service_role;
        GRANT ALL ON TABLE public.waitlist_tasks TO service_role;
    END IF;
END $$;
