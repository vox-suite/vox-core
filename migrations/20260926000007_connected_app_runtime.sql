-- When each app's tool list was last read from its server, and when the
-- agent last used the app (used to keep an in-progress flow's tools loaded).
ALTER TABLE remote_extension_credentials
    ADD COLUMN tools_refreshed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN last_used_at TIMESTAMPTZ;

-- Pending actions keep their exact arguments, so a confirmation runs what
-- the user was shown instead of whatever the model regenerates.
ALTER TABLE connected_app_pending_actions
    ADD COLUMN arguments JSONB NOT NULL DEFAULT '{}'::jsonb;
