-- On databases which have not yet retired the runtime, preserve the existing
-- authorized state and encrypted storage across the historical retirement.
-- Already-retired databases have no active external_connections: do nothing.
DO $$
DECLARE name TEXT;
BEGIN
    IF to_regclass('public.external_connections') IS NOT NULL
       AND to_regclass('public.retired_external_connections') IS NULL THEN
        CREATE SCHEMA connector_upgrade_snapshot;
        FOREACH name IN ARRAY ARRAY['external_connections','agent_capability_grants','mcp_oauth_clients','mcp_authorization_sessions',
            'remote_extension_credentials','connected_app_pending_actions','connector_packages','connector_package_installations',
            'connector_setups','connector_skill_installations','playstation_accounts'] LOOP
            IF to_regclass('public.' || name) IS NOT NULL THEN
                EXECUTE format('CREATE TABLE connector_upgrade_snapshot.%I AS TABLE public.%I',name,name);
            END IF;
        END LOOP;
    END IF;
END $$;
