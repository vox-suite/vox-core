-- Preserve account IDs and encrypted bytes. Ambiguous legacy ownership stays inaccessible.
ALTER TABLE user_contexts DROP CONSTRAINT IF EXISTS user_contexts_user_id_key;
ALTER TABLE user_contexts ADD CONSTRAINT user_contexts_id_user_key UNIQUE(id,user_id);
ALTER TABLE vox_connections ADD COLUMN user_context_id UUID REFERENCES user_contexts(id);
ALTER TABLE vox_connection_setups ADD COLUMN user_context_id UUID REFERENCES user_contexts(id);
UPDATE vox_connections c SET user_context_id=(SELECT min(id::text)::uuid FROM user_contexts u WHERE u.user_id=c.user_id)
WHERE (SELECT count(*) FROM user_contexts u WHERE u.user_id=c.user_id)=1;
UPDATE vox_connection_setups s SET user_context_id=(SELECT min(id::text)::uuid FROM user_contexts u WHERE u.user_id=s.user_id)
WHERE (SELECT count(*) FROM user_contexts u WHERE u.user_id=s.user_id)=1;
UPDATE vox_connections SET failure_code='scope_reassociation_required',lease_token=NULL,lease_until=NULL WHERE user_context_id IS NULL;
ALTER TABLE vox_connections DROP CONSTRAINT vox_connections_user_connector_unique;
ALTER TABLE vox_connections ADD CONSTRAINT vox_connections_context_connector_unique UNIQUE(user_context_id,connector_id);
ALTER TABLE vox_connections ADD CONSTRAINT vox_connections_context_owner FOREIGN KEY(user_context_id,user_id) REFERENCES user_contexts(id,user_id);
ALTER TABLE vox_connection_setups ADD CONSTRAINT vox_setups_context_owner FOREIGN KEY(user_context_id,user_id) REFERENCES user_contexts(id,user_id);

-- Legacy writers may infer a context only when exactly one exists. Ingestion
-- supplies its verified connection context within the checkpoint transaction.
CREATE OR REPLACE FUNCTION resource_context_compatibility() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE hint UUID; candidates INTEGER;
BEGIN
    IF NEW.user_id IS NULL THEN
        IF TG_OP='UPDATE' AND TG_TABLE_NAME='audit_events' AND OLD.user_id IS NOT NULL THEN NEW.user_context_id:=NULL;
        ELSIF NEW.user_context_id IS NOT NULL THEN RAISE EXCEPTION 'global row cannot have user context' USING ERRCODE='23514'; END IF;
    ELSIF NEW.user_context_id IS NULL THEN
        hint:=NULLIF(current_setting('vox.connection_context',true),'')::UUID;
        IF hint IS NOT NULL AND EXISTS(SELECT 1 FROM user_contexts WHERE id=hint AND user_id=NEW.user_id) THEN
            NEW.user_context_id:=hint;
        ELSE
            SELECT count(*),min(id::text)::uuid INTO candidates,NEW.user_context_id FROM user_contexts WHERE user_id=NEW.user_id;
            IF candidates<>1 THEN RAISE EXCEPTION 'explicit user context required' USING ERRCODE='23514'; END IF;
        END IF;
    END IF;
    RETURN NEW;
END $$;
