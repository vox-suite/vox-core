CREATE TABLE user_insights (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    domain TEXT NOT NULL,
    summary TEXT NOT NULL,
    reasoning TEXT NOT NULL,
    source_record_ids UUID[] NOT NULL DEFAULT '{}'::uuid[],
    outcome_status TEXT NOT NULL DEFAULT 'pending',
    valid_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_insights_domain_not_empty CHECK (length(btrim(domain)) > 0),
    CONSTRAINT user_insights_summary_not_empty CHECK (length(btrim(summary)) > 0),
    CONSTRAINT user_insights_reasoning_not_empty CHECK (length(btrim(reasoning)) > 0),
    CONSTRAINT user_insights_outcome_status_valid CHECK (outcome_status IN ('pending', 'notified', 'acknowledged', 'resolved', 'dismissed'))
);
