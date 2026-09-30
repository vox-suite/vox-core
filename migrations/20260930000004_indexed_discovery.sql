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
ALTER TABLE skill_package_versions ADD COLUMN search_external_key TEXT;
UPDATE skill_package_versions v SET search_external_key=s.external_key FROM skill_packages s WHERE s.id=v.skill_id;
ALTER TABLE skill_package_versions ALTER COLUMN search_external_key SET NOT NULL;
CREATE FUNCTION index_skill_external_key() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    SELECT external_key INTO NEW.search_external_key FROM skill_packages WHERE id=NEW.skill_id;
    RETURN NEW;
END $$;
CREATE TRIGGER skill_version_search_key BEFORE INSERT OR UPDATE OF skill_id ON skill_package_versions
    FOR EACH ROW EXECUTE FUNCTION index_skill_external_key();
ALTER TABLE skill_package_versions ADD COLUMN search_document TSVECTOR GENERATED ALWAYS AS
    (to_tsvector('simple',replace(search_external_key,'-',' ') || ' ' || title || ' ' || summary)) STORED;
CREATE INDEX skill_package_versions_search ON skill_package_versions USING GIN(search_document);
