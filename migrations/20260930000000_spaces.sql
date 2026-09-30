CREATE TABLE spaces (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title TEXT NOT NULL CHECK (length(btrim(title)) > 0),
    intent TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('ideating', 'planned', 'committed', 'dropped')),
    agent_spec JSONB NOT NULL DEFAULT '{}'::jsonb,
    committed_collection_id UUID REFERENCES collections(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE space_nodes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    space_id UUID NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    kind TEXT NOT NULL DEFAULT 'node',
    title TEXT NOT NULL,
    body TEXT NOT NULL DEFAULT '',
    data JSONB NOT NULL DEFAULT '{}'::jsonb,
    state TEXT NOT NULL CHECK (state IN ('running', 'done', 'stale', 'rejected')),
    position JSONB NOT NULL DEFAULT '{"x": 0.0, "y": 0.0}'::jsonb,
    derived_from UUID[] NOT NULL DEFAULT '{}',
    provenance JSONB NOT NULL DEFAULT '{}'::jsonb,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE space_edges (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    space_id UUID NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    from_node UUID NOT NULL REFERENCES space_nodes(id) ON DELETE CASCADE,
    to_node UUID NOT NULL REFERENCES space_nodes(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT space_edges_unique_edge UNIQUE (space_id, from_node, to_node)
);

CREATE INDEX spaces_user_created_idx ON spaces (user_id, created_at DESC);
CREATE INDEX space_nodes_space_idx ON space_nodes (space_id, created_at ASC);
CREATE INDEX space_edges_space_idx ON space_edges (space_id);

ALTER TABLE jobs DROP CONSTRAINT jobs_kind_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind_check CHECK (kind IN (
    'process_event', 'run_schedule', 'dispatch_action', 'summarize_conversation',
    'evaluate_span', 'execute_span', 'run_space'));
