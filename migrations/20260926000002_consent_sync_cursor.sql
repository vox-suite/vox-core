ALTER TABLE data_source_consents ADD COLUMN IF NOT EXISTS synced_until TIMESTAMPTZ;
