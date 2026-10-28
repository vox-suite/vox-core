-- No transcript/audio storage here. Receipts survive task deletion so retries
-- cannot recreate a task the user removed. Creation is service-mediated only.
CREATE TABLE personal_task_requests (
    user_id uuid NOT NULL REFERENCES public.users(id) ON DELETE CASCADE,
    request_id text NOT NULL CHECK (length(request_id) BETWEEN 1 AND 200),
    payload_hash text NOT NULL CHECK (length(payload_hash) = 64),
    span_id uuid REFERENCES public.spans(id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, request_id)
);
ALTER TABLE personal_task_requests ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON personal_task_requests FROM anon, authenticated;
GRANT ALL ON personal_task_requests TO service_role;
CREATE INDEX personal_task_requests_span_idx ON personal_task_requests(span_id) WHERE span_id IS NOT NULL;
