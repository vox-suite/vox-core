-- Consent survives provider redirects and is consumed atomically with enablement.
ALTER TABLE mcp_authorization_sessions ADD COLUMN completed_at TIMESTAMPTZ;
CREATE TABLE connector_setups (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id),
    extension_id UUID NOT NULL REFERENCES remote_extensions(id),
    extension_version INTEGER NOT NULL,
    deployment_id UUID NOT NULL,
    package_key TEXT NOT NULL,
    package_version INTEGER NOT NULL,
    package_digest TEXT NOT NULL,
    consent JSONB,
    authorization_session_id UUID UNIQUE REFERENCES mcp_authorization_sessions(id) ON DELETE SET NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','complete','needs_review')),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + interval '20 minutes',
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (deployment_id,package_key,package_version) REFERENCES connector_packages(deployment_id,external_key,version)
);
CREATE INDEX connector_setups_context ON connector_setups(user_context_id,created_at DESC);
