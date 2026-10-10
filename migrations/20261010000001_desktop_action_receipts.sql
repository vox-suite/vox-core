CREATE TABLE desktop_action_receipts (
 user_id uuid NOT NULL REFERENCES public.users(id) ON DELETE CASCADE,
 command_id uuid NOT NULL,
 request_digest text NOT NULL,
 result jsonb,
 created_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(user_id,command_id)
);
ALTER TABLE desktop_action_receipts ENABLE ROW LEVEL SECURITY;
