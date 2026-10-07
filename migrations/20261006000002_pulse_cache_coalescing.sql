CREATE FUNCTION pulse_cached_aggregate(p_user uuid,p_key text,p_requests jsonb,p_at timestamptz,p_sql text,p_refresh boolean,p_arrived timestamptz)
RETURNS jsonb LANGUAGE plpgsql AS $$
DECLARE cached jsonb; computed jsonb; finished timestamptz;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(p_user::text||':'||p_key,0));
    SELECT payload INTO cached FROM pulse_cache
      WHERE user_id=p_user AND cache_key=p_key AND expires_at>clock_timestamp()
      AND (NOT p_refresh OR created_at>=p_arrived);
    IF cached IS NOT NULL THEN RETURN cached; END IF;
    PERFORM set_config('statement_timeout','5s',true);
    EXECUTE p_sql INTO computed USING p_user,p_requests,p_at;
    finished := clock_timestamp();
    computed := jsonb_build_object('rows',computed,'computed_at',finished);
    DELETE FROM pulse_cache WHERE user_id=p_user AND cache_key IN
      (SELECT cache_key FROM pulse_cache WHERE user_id=p_user ORDER BY expires_at DESC OFFSET 63);
    INSERT INTO pulse_cache(user_id,cache_key,payload,expires_at,created_at)
      VALUES(p_user,p_key,computed,finished+interval '60 seconds',finished)
      ON CONFLICT(user_id,cache_key) DO UPDATE SET payload=EXCLUDED.payload,expires_at=EXCLUDED.expires_at,created_at=EXCLUDED.created_at;
    RETURN computed;
END $$;
REVOKE ALL ON FUNCTION pulse_cached_aggregate(uuid,text,jsonb,timestamptz,text,boolean,timestamptz) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION pulse_cached_aggregate(uuid,text,jsonb,timestamptz,text,boolean,timestamptz) TO service_role;

CREATE OR REPLACE FUNCTION pulse_bump_revision() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE old_owner uuid; new_owner uuid; owner_id uuid;
BEGIN
    IF TG_OP <> 'INSERT' THEN old_owner := OLD.user_id; END IF;
    IF TG_OP <> 'DELETE' THEN new_owner := NEW.user_id; END IF;
    IF TG_TABLE_NAME='data_schemas' AND ((TG_OP<>'INSERT' AND old_owner IS NULL) OR (TG_OP<>'DELETE' AND new_owner IS NULL)) THEN
        UPDATE pulse_global_revision SET revision=revision+1;
    END IF;
    FOR owner_id IN SELECT DISTINCT u FROM unnest(ARRAY[old_owner,new_owner]) u WHERE u IS NOT NULL LOOP
        IF NOT EXISTS(SELECT 1 FROM users WHERE id=owner_id) THEN CONTINUE; END IF;
        INSERT INTO pulse_revisions(user_id,data_revision,discovery_revision)
        VALUES(owner_id,CASE WHEN TG_TABLE_NAME='spans' THEN 1 ELSE 0 END,1)
        ON CONFLICT(user_id) DO UPDATE SET
          data_revision=pulse_revisions.data_revision+CASE WHEN TG_TABLE_NAME='spans' THEN 1 ELSE 0 END,
          discovery_revision=pulse_revisions.discovery_revision+1;
    END LOOP;
    RETURN NULL;
END $$;
