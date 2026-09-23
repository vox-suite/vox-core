CREATE TABLE remote_extensions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id) ON DELETE RESTRICT,
    external_key TEXT NOT NULL,
    display_name TEXT NOT NULL,
    protocol TEXT NOT NULL CHECK (protocol IN ('mcp', 'direct')),
    endpoint_url TEXT NOT NULL,
    operator_id TEXT NOT NULL,
    operator_name TEXT NOT NULL,
    support_email TEXT,
    terms_url TEXT,
    current_version INTEGER NOT NULL DEFAULT 1 CHECK (current_version > 0),
    conformance_status TEXT NOT NULL DEFAULT 'pending' CHECK (conformance_status IN ('pending', 'passed', 'failed')),
    operator_enabled BOOLEAN NOT NULL DEFAULT false,
    consent_status TEXT NOT NULL DEFAULT 'consented' CHECK (consent_status IN ('consented', 'consent_required')),
    lifecycle_state TEXT NOT NULL DEFAULT 'installed' CHECK (lifecycle_state IN ('installed', 'active', 'quarantined', 'disabled', 'removed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT remote_extensions_user_key_unique UNIQUE (user_context_id, external_key)
);

CREATE TABLE remote_extension_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    extension_id UUID NOT NULL REFERENCES remote_extensions(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL CHECK (version > 0),
    endpoint_url TEXT NOT NULL,
    operator_id TEXT NOT NULL,
    operator_name TEXT NOT NULL,
    capabilities JSONB NOT NULL DEFAULT '[]'::jsonb,
    conformance_status TEXT NOT NULL DEFAULT 'pending' CHECK (conformance_status IN ('pending', 'passed', 'failed')),
    conformance_report JSONB NOT NULL DEFAULT '{}'::jsonb,
    consent_granted_at TIMESTAMPTZ,
    quarantined_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT remote_extension_versions_unique UNIQUE (extension_id, version)
);

CREATE TABLE remote_extension_conformance_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    extension_id UUID NOT NULL REFERENCES remote_extensions(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('passed', 'failed')),
    report JSONB NOT NULL DEFAULT '{}'::jsonb,
    run_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX remote_extensions_user_state_idx ON remote_extensions (user_context_id, lifecycle_state);
