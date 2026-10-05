-- Curated records share the platform connection ID, but linking creates no agent grants.
CREATE FUNCTION project_curated_connection() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE integration UUID; deployment UUID; cap TEXT; state TEXT;
BEGIN
    IF TG_OP='DELETE' THEN
        UPDATE external_connections SET authorization_state='revoked',revoked_at=now(),expires_at=NULL,updated_at=now() WHERE id=OLD.id;
        UPDATE agent_capability_grants SET state='revoked',revoked_at=now(),updated_at=now() WHERE connection_id=OLD.id;
        RETURN OLD;
    END IF;
    IF NEW.user_context_id IS NULL THEN RETURN NEW; END IF;
    SELECT deployment_id INTO deployment FROM user_contexts WHERE id=NEW.user_context_id AND user_id=NEW.user_id;
    IF deployment IS NULL THEN RAISE EXCEPTION 'invalid account context'; END IF;
    cap := 'curated_' || NEW.connector_id || '.read';
    INSERT INTO integration_definitions(deployment_id,external_key,protocol,display_name,declaration_version,state)
    VALUES(deployment,'curated_' || NEW.connector_id,'direct',NEW.connector_id,1,'enabled')
    ON CONFLICT(deployment_id,external_key) DO NOTHING;
    SELECT id INTO integration FROM integration_definitions WHERE deployment_id=deployment AND external_key='curated_' || NEW.connector_id;
    INSERT INTO integration_capability_declarations(integration_id,external_key,effect,access_needs,data_recipients,regions,failure_modes,optional_guarantees)
    VALUES(integration,'read','read',ARRAY['account_read'],ARRAY[NEW.connector_id],ARRAY[]::text[],ARRAY['reconnect_required'],'{}')
    ON CONFLICT(integration_id,external_key) DO NOTHING;
    state := CASE WHEN NEW.authorization_state='authorized' AND NEW.consented_at IS NOT NULL THEN 'authorized' ELSE 'expired' END;
    INSERT INTO external_connections(id,user_context_id,integration_id,external_account_hash,credential_custody,authorization_state,authorized_capabilities,account_display_id,expires_at)
    VALUES(NEW.id,NEW.user_context_id,integration,decode(md5(NEW.id::text)||md5(NEW.user_context_id::text),'hex'),'platform_held',state,
        CASE WHEN state='authorized' AND NEW.assistant_read THEN ARRAY[cap] ELSE ARRAY[]::text[] END,NEW.account_display_id,NULL)
    ON CONFLICT(id) DO UPDATE SET authorization_state=EXCLUDED.authorization_state,authorized_capabilities=EXCLUDED.authorized_capabilities,
        account_display_id=EXCLUDED.account_display_id,expires_at=NULL,revoked_at=NULL,updated_at=now();
    RETURN NEW;
END $$;
CREATE TRIGGER curated_connection_authority AFTER INSERT OR UPDATE OF authorization_state,assistant_read,consented_at,user_context_id,account_display_id ON vox_connections
FOR EACH ROW EXECUTE FUNCTION project_curated_connection();
CREATE TRIGGER curated_connection_disconnect AFTER DELETE ON vox_connections FOR EACH ROW EXECUTE FUNCTION project_curated_connection();

CREATE FUNCTION revoke_curated_credentials() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.authorization_state <> 'authorized' AND OLD.authorization_state='authorized' THEN
        UPDATE vox_connections SET authorization_state='expired',access_ciphertext=NULL,refresh_ciphertext=NULL,
            generation=gen_random_uuid(),credential_generation=gen_random_uuid(),lease_token=NULL,lease_until=NULL,
            failure_code='reconnect_required' WHERE id=NEW.id AND authorization_state='authorized';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER curated_platform_disconnect AFTER UPDATE OF authorization_state ON external_connections
FOR EACH ROW EXECUTE FUNCTION revoke_curated_credentials();
-- Project migrated records without granting anything or rewriting credentials.
UPDATE vox_connections SET user_context_id=user_context_id WHERE user_context_id IS NOT NULL;
