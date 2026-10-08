CREATE TABLE IF NOT EXISTS span_revisions (
    user_id uuid PRIMARY KEY,
    revision bigint NOT NULL DEFAULT 0
);

CREATE OR REPLACE FUNCTION bump_span_revisions() RETURNS trigger
LANGUAGE plpgsql AS
$$
BEGIN
    INSERT INTO span_revisions (user_id, revision)
    SELECT DISTINCT user_id, 1 FROM changed
    ON CONFLICT (user_id) DO UPDATE SET revision = span_revisions.revision + 1;
    RETURN NULL;
END
$$;

DROP TRIGGER IF EXISTS spans_revision_insert ON spans;
CREATE TRIGGER spans_revision_insert AFTER INSERT ON spans
    REFERENCING NEW TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION bump_span_revisions();

DROP TRIGGER IF EXISTS spans_revision_update ON spans;
CREATE TRIGGER spans_revision_update AFTER UPDATE ON spans
    REFERENCING NEW TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION bump_span_revisions();

DROP TRIGGER IF EXISTS spans_revision_delete ON spans;
CREATE TRIGGER spans_revision_delete AFTER DELETE ON spans
    REFERENCING OLD TABLE AS changed
    FOR EACH STATEMENT EXECUTE FUNCTION bump_span_revisions();
