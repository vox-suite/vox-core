CREATE TABLE public.usage_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id uuid NOT NULL REFERENCES public.users(id) ON DELETE CASCADE,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    kind text NOT NULL CHECK (kind IN ('gemini', 'voice')),
    source_key text NOT NULL CHECK (length(source_key) BETWEEN 1 AND 256),
    channel text NOT NULL CHECK (length(channel) BETWEEN 1 AND 32),
    feature text NOT NULL CHECK (length(feature) BETWEEN 1 AND 64),
    model text CHECK (length(model) <= 128),
    input_tokens bigint CHECK (input_tokens >= 0),
    output_tokens bigint CHECK (output_tokens >= 0),
    reasoning_tokens bigint CHECK (reasoning_tokens >= 0),
    cached_input_tokens bigint CHECK (cached_input_tokens >= 0),
    session_seconds double precision CHECK (session_seconds >= 0),
    input_audio_seconds double precision CHECK (input_audio_seconds >= 0),
    output_audio_seconds double precision CHECK (output_audio_seconds >= 0),
    tts_chars bigint CHECK (tts_chars >= 0),
    UNIQUE (user_id, kind, source_key)
);
CREATE INDEX usage_events_user_occurred_idx ON public.usage_events (user_id, occurred_at);
ALTER TABLE public.usage_events ENABLE ROW LEVEL SECURITY;
CREATE POLICY usage_events_owner_read ON public.usage_events FOR SELECT TO authenticated USING (user_id = auth.uid());
