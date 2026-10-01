ALTER TABLE auth_sessions
    ADD COLUMN scope TEXT NOT NULL DEFAULT 'full' CHECK (scope IN ('full', 'web'));
