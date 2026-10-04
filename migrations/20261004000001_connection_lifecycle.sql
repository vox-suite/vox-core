ALTER TABLE vox_connections ADD COLUMN generation UUID NOT NULL DEFAULT gen_random_uuid();
ALTER TABLE vox_connections ADD COLUMN lease_token UUID;
ALTER TABLE vox_connections ADD COLUMN lease_until TIMESTAMPTZ;
ALTER TABLE vox_connections ADD COLUMN consented_at TIMESTAMPTZ;
ALTER TABLE vox_connection_setups ADD COLUMN verifier_ciphertext BYTEA;
ALTER TABLE vox_connection_setups ADD COLUMN redirect_uri TEXT;
ALTER TABLE vox_connection_setups ADD COLUMN consented_at TIMESTAMPTZ;
UPDATE vox_connections SET authorization_state='expired', failure_code='consent_required', access_ciphertext=NULL, refresh_ciphertext=NULL;
UPDATE vox_connection_setups SET status='expired' WHERE status='pending';
