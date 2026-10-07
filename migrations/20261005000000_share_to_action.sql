CREATE TABLE integration_grants (
 id uuid PRIMARY KEY DEFAULT gen_random_uuid(), user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 client_id text NOT NULL, collection_ids uuid[] NOT NULL, board_ids uuid[] NOT NULL,
 span_from timestamptz, span_to timestamptz, allow_create_plans boolean NOT NULL DEFAULT false,
 expires_at timestamptz NOT NULL DEFAULT now() + interval '90 days', revoked_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(),
 CHECK ((span_from IS NULL AND span_to IS NULL) OR (span_from IS NOT NULL AND span_to IS NOT NULL AND span_to > span_from AND span_to <= span_from + interval '365 days'))
);
CREATE TABLE integration_codes (
 code_hash text PRIMARY KEY, grant_id uuid NOT NULL REFERENCES integration_grants(id) ON DELETE CASCADE,
 redirect_uri text NOT NULL, code_challenge text NOT NULL, expires_at timestamptz NOT NULL, consumed_at timestamptz
);
CREATE TABLE integration_tokens (
 access_hash text PRIMARY KEY, refresh_hash text NOT NULL UNIQUE, grant_id uuid NOT NULL REFERENCES integration_grants(id) ON DELETE CASCADE,
 access_expires_at timestamptz NOT NULL, refresh_expires_at timestamptz NOT NULL, rotated_at timestamptz
);
CREATE TABLE integration_plan_requests (
 user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE, client_id text NOT NULL,
 request_id uuid NOT NULL, payload_hash text NOT NULL, span_id uuid NOT NULL REFERENCES spans(id) ON DELETE CASCADE,
 PRIMARY KEY (user_id, client_id, request_id)
);
ALTER TABLE integration_grants ENABLE ROW LEVEL SECURITY;
ALTER TABLE integration_codes ENABLE ROW LEVEL SECURITY;
ALTER TABLE integration_tokens ENABLE ROW LEVEL SECURITY;
ALTER TABLE integration_plan_requests ENABLE ROW LEVEL SECURITY;
