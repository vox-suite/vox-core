CREATE TABLE client_devices (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_identifier TEXT NOT NULL,
    platform TEXT NOT NULL,
    device_name TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT true,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    telemetry JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT client_devices_user_identifier_key UNIQUE (user_id, device_identifier),
    CONSTRAINT client_devices_identifier_not_empty CHECK (length(btrim(device_identifier)) > 0),
    CONSTRAINT client_devices_platform_not_empty CHECK (length(btrim(platform)) > 0)
);
