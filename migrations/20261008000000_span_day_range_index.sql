CREATE EXTENSION IF NOT EXISTS btree_gist;

CREATE OR REPLACE FUNCTION span_range(s timestamptz, e timestamptz) RETURNS tstzrange
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS
$$ SELECT tstzrange(s, GREATEST(COALESCE(e, s), s), '[]') $$;

CREATE INDEX IF NOT EXISTS spans_user_range_gist
    ON spans USING gist (user_id, span_range(start_at, end_at))
    WHERE start_at IS NOT NULL;
