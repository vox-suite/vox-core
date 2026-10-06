CREATE TABLE pulse_revisions (
    user_id uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    data_revision bigint NOT NULL DEFAULT 0,
    discovery_revision bigint NOT NULL DEFAULT 0
);
CREATE TABLE pulse_global_revision (id boolean PRIMARY KEY DEFAULT true CHECK(id), revision bigint NOT NULL DEFAULT 0);
INSERT INTO pulse_global_revision DEFAULT VALUES;
CREATE TABLE pulse_saved_charts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    idempotency_key uuid NOT NULL,
    title text NOT NULL CHECK(length(btrim(title)) BETWEEN 1 AND 120),
    definition jsonb NOT NULL CHECK(jsonb_typeof(definition)='object'),
    definition_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(user_id,idempotency_key)
);
CREATE INDEX pulse_saved_charts_user_id_idx ON pulse_saved_charts(user_id,id);
CREATE TABLE pulse_dismissals (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    definition_hash text NOT NULL,
    PRIMARY KEY(user_id,definition_hash)
);
CREATE TABLE pulse_cache (
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    cache_key text NOT NULL,
    payload jsonb NOT NULL CHECK(pg_column_size(payload) <= 2097152),
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(user_id,cache_key)
);
CREATE INDEX pulse_cache_expiry_idx ON pulse_cache(user_id,expires_at);
CREATE FUNCTION pulse_bump_revision() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE old_owner uuid; new_owner uuid; owner_id uuid;
BEGIN
    IF TG_OP <> 'INSERT' THEN old_owner := OLD.user_id; END IF;
    IF TG_OP <> 'DELETE' THEN new_owner := NEW.user_id; END IF;
    IF TG_TABLE_NAME='data_schemas' AND (old_owner IS NULL OR new_owner IS NULL) THEN
        UPDATE pulse_global_revision SET revision=revision+1;
    END IF;
    FOR owner_id IN SELECT DISTINCT u FROM unnest(ARRAY[old_owner,new_owner]) u WHERE u IS NOT NULL LOOP
        INSERT INTO pulse_revisions(user_id,data_revision,discovery_revision)
        VALUES(owner_id,CASE WHEN TG_TABLE_NAME='spans' THEN 1 ELSE 0 END,1)
        ON CONFLICT(user_id) DO UPDATE SET
          data_revision=pulse_revisions.data_revision+CASE WHEN TG_TABLE_NAME='spans' THEN 1 ELSE 0 END,
          discovery_revision=pulse_revisions.discovery_revision+1;
    END LOOP;
    RETURN NULL;
END $$;
CREATE TRIGGER pulse_spans_revision AFTER INSERT OR UPDATE OR DELETE ON spans FOR EACH ROW EXECUTE FUNCTION pulse_bump_revision();
CREATE TRIGGER pulse_schema_revision AFTER INSERT OR UPDATE OR DELETE ON data_schemas FOR EACH ROW EXECUTE FUNCTION pulse_bump_revision();
CREATE TRIGGER pulse_connections_revision AFTER INSERT OR UPDATE OR DELETE ON vox_connections FOR EACH ROW EXECUTE FUNCTION pulse_bump_revision();
CREATE TRIGGER pulse_saved_revision AFTER INSERT OR UPDATE OR DELETE ON pulse_saved_charts FOR EACH ROW EXECUTE FUNCTION pulse_bump_revision();
CREATE TRIGGER pulse_dismissal_revision AFTER INSERT OR DELETE ON pulse_dismissals FOR EACH ROW EXECUTE FUNCTION pulse_bump_revision();
ALTER TABLE pulse_revisions ENABLE ROW LEVEL SECURITY;
ALTER TABLE pulse_saved_charts ENABLE ROW LEVEL SECURITY;
ALTER TABLE pulse_dismissals ENABLE ROW LEVEL SECURITY;
ALTER TABLE pulse_cache ENABLE ROW LEVEL SECURITY;
CREATE POLICY pulse_revisions_service ON pulse_revisions FOR ALL TO service_role USING(true) WITH CHECK(true);
CREATE POLICY pulse_charts_service ON pulse_saved_charts FOR ALL TO service_role USING(true) WITH CHECK(true);
CREATE POLICY pulse_dismissals_service ON pulse_dismissals FOR ALL TO service_role USING(true) WITH CHECK(true);
CREATE POLICY pulse_cache_service ON pulse_cache FOR ALL TO service_role USING(true) WITH CHECK(true);
