CREATE TABLE desktop_voice_sessions (
 id uuid PRIMARY KEY,
 user_id uuid NOT NULL REFERENCES public.users(id) ON DELETE CASCADE,
 device_id uuid REFERENCES public.devices(id),
 ticket_hash text NOT NULL UNIQUE,
 expires_at timestamptz NOT NULL,
 redeemed_at timestamptz,
 state text NOT NULL DEFAULT 'active' CHECK(state IN ('active','completed','failed')),
 created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX desktop_voice_sessions_user ON desktop_voice_sessions(user_id,created_at DESC);
ALTER TABLE desktop_voice_sessions ENABLE ROW LEVEL SECURITY;
