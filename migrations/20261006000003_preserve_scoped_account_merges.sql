-- Preserve host contexts, connection IDs and encryption interpretation during
-- verified-email / phone account unification. Conflicts abort, never delete data.
ALTER TABLE vox_connections ADD COLUMN credential_user_id UUID;
DO $$ DECLARE fk record; BEGIN
 FOR fk IN SELECT conrelid::regclass AS tbl, conname FROM pg_constraint
 WHERE contype='f' AND connamespace='public'::regnamespace AND NOT condeferrable
 AND EXISTS(SELECT 1 FROM pg_attribute p WHERE p.attrelid=confrelid AND p.attnum=ANY(confkey) AND p.attname='user_id')
 LOOP EXECUTE format('ALTER TABLE %s ALTER CONSTRAINT %I DEFERRABLE INITIALLY IMMEDIATE',fk.tbl,fk.conname); END LOOP;
END $$;
CREATE OR REPLACE FUNCTION merge_user_accounts(old_user UUID,new_user UUID) RETURNS VOID
LANGUAGE plpgsql SET search_path=public,pg_temp AS $$
DECLARE fk record;
BEGIN
 IF old_user=new_user THEN RETURN; END IF;
 PERFORM 1 FROM users WHERE id IN(old_user,new_user) ORDER BY id FOR UPDATE;
 IF NOT EXISTS(SELECT 1 FROM users WHERE id=old_user) OR NOT EXISTS(SELECT 1 FROM users WHERE id=new_user) THEN
   RAISE EXCEPTION 'both account owners must exist';
 END IF;
 SET CONSTRAINTS ALL DEFERRED;
 -- Disposable caches and revision counters are not authority or user history.
 DELETE FROM pulse_cache WHERE user_id IN(old_user,new_user);
 INSERT INTO pulse_dismissals(user_id,definition_hash)
 SELECT new_user,definition_hash FROM pulse_dismissals WHERE user_id=old_user ON CONFLICT DO NOTHING;
 DELETE FROM pulse_dismissals WHERE user_id=old_user;
 -- Deliberately no FK to users: deleting the old account must not delete AAD provenance.
 UPDATE vox_connections SET credential_user_id=COALESCE(credential_user_id,user_id) WHERE user_id=old_user;
 -- Each resource keeps its context, including its existing agents and grants.
 -- Update user references in all FK shapes, without remapping any context ID.
 FOR fk IN
   SELECT DISTINCT c.conrelid::regclass AS tbl,a.attname AS col
   FROM pg_constraint c
   CROSS JOIN LATERAL unnest(c.conkey,c.confkey) AS k(child_att,parent_att)
   JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=k.child_att
   JOIN pg_attribute p ON p.attrelid=c.confrelid AND p.attnum=k.parent_att
   WHERE c.contype='f' AND c.connamespace='public'::regnamespace
    AND c.conrelid NOT IN('pulse_revisions'::regclass,'pulse_cache'::regclass,'pulse_dismissals'::regclass)
    AND ((c.confrelid='users'::regclass AND p.attname='id') OR p.attname='user_id')
   ORDER BY tbl,col
 LOOP
   EXECUTE format('UPDATE %s SET %I=$1 WHERE %I=$2',fk.tbl,fk.col,fk.col) USING new_user,old_user;
 END LOOP;
 DELETE FROM pulse_revisions WHERE user_id=old_user;
 DELETE FROM users WHERE id=old_user;
 SET CONSTRAINTS ALL IMMEDIATE;
END $$;
