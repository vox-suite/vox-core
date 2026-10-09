CREATE TABLE updates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('briefing', 'email_notice', 'processing_issue', 'connection_status', 'daily_plan')),
    content_version INTEGER NOT NULL DEFAULT 1 CHECK (content_version > 0),
    category TEXT NOT NULL CHECK (length(btrim(category)) > 0),
    title TEXT NOT NULL CHECK (length(btrim(title)) > 0),
    summary TEXT,
    content JSONB NOT NULL DEFAULT '{}'::jsonb,
    ui_hint JSONB NOT NULL DEFAULT '{}'::jsonb,
    priority TEXT NOT NULL DEFAULT 'standard' CHECK (priority IN ('low', 'standard', 'high', 'urgent')),
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'resolved', 'dismissed')),
    read_at TIMESTAMPTZ,
    source_job_id UUID,
    dedupe_key TEXT,
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    resolved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT updates_id_user_key UNIQUE (id, user_id),
    CONSTRAINT updates_content_is_object CHECK (jsonb_typeof(content) = 'object'),
    CONSTRAINT updates_ui_hint_is_object CHECK (jsonb_typeof(ui_hint) = 'object'),
    CONSTRAINT updates_job_tenant_fk FOREIGN KEY (source_job_id, user_id) REFERENCES jobs(id, user_id) ON DELETE SET NULL (source_job_id)
);

CREATE UNIQUE INDEX updates_user_dedupe_uniq
    ON updates (user_id, dedupe_key)
    WHERE dedupe_key IS NOT NULL;

CREATE INDEX updates_user_status_published_idx
    ON updates (user_id, status, published_at DESC);

CREATE INDEX updates_user_kind_idx
    ON updates (user_id, kind, status);
