-- Public servers retain inventory without an OAuth credential.
ALTER TABLE remote_extension_credentials ADD COLUMN auth_mode TEXT NOT NULL DEFAULT 'oauth' CHECK (auth_mode IN ('oauth','none'));
ALTER TABLE remote_extension_credentials ALTER COLUMN access_token_ciphertext DROP NOT NULL;
ALTER TABLE remote_extension_credentials ADD CONSTRAINT credential_matches_auth CHECK ((auth_mode='oauth' AND access_token_ciphertext IS NOT NULL) OR (auth_mode='none' AND access_token_ciphertext IS NULL AND refresh_token_ciphertext IS NULL));
ALTER TABLE external_connections DROP CONSTRAINT external_connections_custody_valid;
ALTER TABLE external_connections ADD CONSTRAINT external_connections_custody_valid CHECK (credential_custody IN ('platform_held','external_operator','none'));
