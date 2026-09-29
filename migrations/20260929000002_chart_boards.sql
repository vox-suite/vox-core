CREATE TABLE chart_boards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (length(btrim(name)) > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE charts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    board_id UUID NOT NULL REFERENCES chart_boards(id) ON DELETE CASCADE,
    title TEXT NOT NULL CHECK (length(btrim(title)) > 0),
    chart_type TEXT NOT NULL CHECK (chart_type IN ('line', 'bar', 'pie', 'area')),
    schema_ids UUID[] NOT NULL CHECK (cardinality(schema_ids) > 0),
    query_spec JSONB NOT NULL CHECK (jsonb_typeof(query_spec) = 'object'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX chart_boards_user_created_idx ON chart_boards (user_id, created_at DESC);
CREATE INDEX charts_board_id_idx ON charts (board_id);
