-- Recovery is forward-only: no revoked authority or expired approval is revived.
ALTER TABLE retired_agent_capability_grants RENAME TO agent_capability_grants;
ALTER TABLE retired_external_connections RENAME TO external_connections;
ALTER TABLE retired_integration_definitions RENAME TO integration_definitions;
ALTER TABLE retired_remote_extension_conformance_runs RENAME TO remote_extension_conformance_runs;
ALTER TABLE retired_remote_extension_versions RENAME TO remote_extension_versions;
ALTER TABLE retired_remote_extensions RENAME TO remote_extensions;
CREATE TABLE mcp_oauth_clients (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    issuer text NOT NULL,
    redirect_uri text NOT NULL,
    client_id text NOT NULL,
    client_secret_ciphertext bytea,
    token_endpoint_auth_method text DEFAULT 'none'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);
ALTER TABLE ONLY mcp_oauth_clients
    ADD CONSTRAINT mcp_oauth_clients_issuer_redirect_unique UNIQUE (issuer, redirect_uri);
ALTER TABLE ONLY mcp_oauth_clients
    ADD CONSTRAINT mcp_oauth_clients_pkey PRIMARY KEY (id);

CREATE TABLE mcp_authorization_sessions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    extension_id uuid NOT NULL,
    user_context_id uuid NOT NULL,
    state_hash text NOT NULL,
    code_verifier_ciphertext bytea NOT NULL,
    issuer text NOT NULL,
    token_endpoint text NOT NULL,
    client_id text NOT NULL,
    redirect_uri text NOT NULL,
    resource text NOT NULL,
    endpoint_url text NOT NULL,
    extension_version integer NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    consumed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT mcp_authorization_sessions_version_positive CHECK ((extension_version > 0))
);
ALTER TABLE ONLY mcp_authorization_sessions
    ADD CONSTRAINT mcp_authorization_sessions_pkey PRIMARY KEY (id);
ALTER TABLE ONLY mcp_authorization_sessions
    ADD CONSTRAINT mcp_authorization_sessions_state_hash_key UNIQUE (state_hash);
ALTER TABLE ONLY mcp_authorization_sessions
    ADD CONSTRAINT mcp_authorization_sessions_extension_id_fkey FOREIGN KEY (extension_id) REFERENCES remote_extensions(id) ON DELETE CASCADE;
ALTER TABLE ONLY mcp_authorization_sessions
    ADD CONSTRAINT mcp_authorization_sessions_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES user_contexts(id) ON DELETE CASCADE;

CREATE TABLE remote_extension_credentials (
    extension_id uuid NOT NULL,
    issuer text NOT NULL,
    token_endpoint text NOT NULL,
    client_id text NOT NULL,
    resource text NOT NULL,
    access_token_ciphertext bytea NOT NULL,
    refresh_token_ciphertext bytea,
    scope text,
    expires_at timestamp with time zone,
    server_info jsonb DEFAULT '{}'::jsonb NOT NULL,
    tools jsonb DEFAULT '[]'::jsonb NOT NULL,
    connected_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    tools_refreshed_at timestamp with time zone DEFAULT now() NOT NULL
);
ALTER TABLE ONLY remote_extension_credentials
    ADD CONSTRAINT remote_extension_credentials_pkey PRIMARY KEY (extension_id);
ALTER TABLE ONLY remote_extension_credentials
    ADD CONSTRAINT remote_extension_credentials_extension_id_fkey FOREIGN KEY (extension_id) REFERENCES remote_extensions(id) ON DELETE CASCADE;

CREATE TABLE connected_app_pending_actions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    extension_id UUID NOT NULL REFERENCES remote_extensions(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    arguments_hash TEXT NOT NULL,
    proposed_turn UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
ALTER TABLE connected_app_pending_actions ADD COLUMN arguments JSONB NOT NULL DEFAULT '{}'::jsonb;
CREATE INDEX connected_app_pending_actions_lookup
    ON connected_app_pending_actions (user_id, extension_id, tool_name, arguments_hash);
-- Public servers retain inventory without an OAuth credential.
ALTER TABLE remote_extension_credentials ADD COLUMN auth_mode TEXT NOT NULL DEFAULT 'oauth' CHECK (auth_mode IN ('oauth','none'));
ALTER TABLE remote_extension_credentials ALTER COLUMN access_token_ciphertext DROP NOT NULL;
ALTER TABLE remote_extension_credentials ADD CONSTRAINT credential_matches_auth CHECK ((auth_mode='oauth' AND access_token_ciphertext IS NOT NULL) OR (auth_mode='none' AND access_token_ciphertext IS NULL AND refresh_token_ciphertext IS NULL));
ALTER TABLE external_connections DROP CONSTRAINT external_connections_custody_valid;
ALTER TABLE external_connections ADD CONSTRAINT external_connections_custody_valid CHECK (credential_custody IN ('platform_held','external_operator','none'));

-- Immutable deployment-reviewed connector packages. Installations carry no authority.
CREATE TABLE connector_packages (
    deployment_id UUID NOT NULL REFERENCES platform_deployments(id),
    external_key TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    digest TEXT NOT NULL,
    manifest JSONB NOT NULL,
    metadata JSONB NOT NULL,
    review JSONB NOT NULL CHECK (jsonb_typeof(review)='object' AND review <> '{}'::jsonb),
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (deployment_id, external_key, version)
);
CREATE INDEX connector_packages_catalog ON connector_packages(deployment_id, external_key, version DESC) WHERE enabled;
CREATE TABLE connector_package_installations (
    extension_id UUID PRIMARY KEY REFERENCES remote_extensions(id),
    deployment_id UUID NOT NULL,
    external_key TEXT NOT NULL,
    version INTEGER NOT NULL,
    installed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (deployment_id,external_key,version) REFERENCES connector_packages(deployment_id,external_key,version)
);
CREATE INDEX connector_package_installations_version ON connector_package_installations(deployment_id,external_key,version);

CREATE TABLE connector_skill_installations (
    extension_id UUID NOT NULL REFERENCES remote_extensions(id),
    skill_id UUID NOT NULL REFERENCES skill_packages(id),
    user_context_id UUID NOT NULL REFERENCES user_contexts(id),
    version INTEGER NOT NULL,
    PRIMARY KEY(extension_id,skill_id),
    FOREIGN KEY(skill_id,version) REFERENCES skill_package_versions(skill_id,version)
);

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

CREATE TABLE playstation_accounts (
    connection_id UUID PRIMARY KEY REFERENCES external_connections(id) ON DELETE CASCADE,
    generation UUID NOT NULL DEFAULT gen_random_uuid(),
    account_id TEXT NOT NULL CHECK (length(account_id) BETWEEN 1 AND 128),
    access_ciphertext BYTEA NOT NULL,
    refresh_ciphertext BYTEA NOT NULL,
    access_expires_at TIMESTAMPTZ NOT NULL,
    refresh_expires_at TIMESTAMPTZ NOT NULL,
    capture_enabled BOOLEAN NOT NULL DEFAULT false,
    snapshots JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(snapshots) = 'object'),
    last_synced_at TIMESTAMPTZ,
    next_sync_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    failure_code TEXT,
    failure_count INTEGER NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX playstation_accounts_due ON playstation_accounts(next_sync_at) WHERE capture_enabled;
ALTER TABLE playstation_accounts ENABLE ROW LEVEL SECURITY;

-- Search compact reviewed metadata; credentials, schemas and skill bodies are
-- not copied into this index. All authority is joined at query time.
CREATE TABLE connector_tool_metadata (
    extension_id UUID NOT NULL,
    version INTEGER NOT NULL,
    external_key TEXT NOT NULL,
    display_name TEXT NOT NULL,
    effect TEXT NOT NULL,
    consequential BOOLEAN NOT NULL,
    search_document TSVECTOR GENERATED ALWAYS AS
      (to_tsvector('simple',replace(replace(external_key,'.',' '),'_',' ') || ' ' || display_name)) STORED,
    PRIMARY KEY(extension_id,version,external_key),
    FOREIGN KEY(extension_id,version) REFERENCES remote_extension_versions(extension_id,version) ON DELETE CASCADE
);
CREATE INDEX connector_tool_metadata_search ON connector_tool_metadata USING GIN(search_document);
CREATE FUNCTION index_connector_tool_metadata() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM connector_tool_metadata WHERE extension_id=NEW.extension_id AND version=NEW.version;
    INSERT INTO connector_tool_metadata(extension_id,version,external_key,display_name,effect,consequential)
    SELECT NEW.extension_id,NEW.version,cap->>'external_key',cap->>'display_name',cap->>'effect',
           COALESCE((cap->>'consequential')::boolean,false)
    FROM jsonb_array_elements(NEW.capabilities) cap;
    RETURN NEW;
END $$;
CREATE TRIGGER connector_tool_metadata_index AFTER INSERT OR UPDATE OF capabilities ON remote_extension_versions
    FOR EACH ROW EXECUTE FUNCTION index_connector_tool_metadata();
INSERT INTO connector_tool_metadata(extension_id,version,external_key,display_name,effect,consequential)
SELECT v.extension_id,v.version,cap->>'external_key',cap->>'display_name',cap->>'effect',COALESCE((cap->>'consequential')::boolean,false)
FROM remote_extension_versions v CROSS JOIN LATERAL jsonb_array_elements(v.capabilities) cap;

-- Restore only the pre-retirement upgrade snapshot created by this recovery.
-- A database already retired before recovery has no snapshot: revoked access stays revoked.
DO $$
DECLARE name TEXT; cols TEXT;
BEGIN
    IF EXISTS(SELECT 1 FROM pg_namespace WHERE nspname='connector_upgrade_snapshot') THEN
        FOREACH name IN ARRAY ARRAY['mcp_oauth_clients','mcp_authorization_sessions','remote_extension_credentials',
            'connected_app_pending_actions','connector_packages','connector_package_installations',
            'connector_setups','connector_skill_installations','playstation_accounts'] LOOP
            IF to_regclass('connector_upgrade_snapshot.' || name) IS NOT NULL THEN
                SELECT string_agg(quote_ident(column_name),',' ORDER BY ordinal_position) INTO cols
                FROM information_schema.columns WHERE table_schema='connector_upgrade_snapshot' AND table_name=name;
                EXECUTE format('INSERT INTO public.%I (%s) SELECT %s FROM connector_upgrade_snapshot.%I ON CONFLICT DO NOTHING',name,cols,cols,name);
            END IF;
        END LOOP;
        UPDATE external_connections c SET authorization_state=s.authorization_state,revoked_at=s.revoked_at,expires_at=s.expires_at,updated_at=s.updated_at
        FROM connector_upgrade_snapshot.external_connections s WHERE c.id=s.id;
        UPDATE agent_capability_grants g SET state=s.state,revoked_at=s.revoked_at,updated_at=s.updated_at
        FROM connector_upgrade_snapshot.agent_capability_grants s WHERE g.id=s.id;
        DROP SCHEMA connector_upgrade_snapshot CASCADE;
    END IF;
END $$;
