-- A connection record is not proof that Core holds a provider credential.
ALTER TABLE connections ALTER COLUMN secret_reference DROP NOT NULL;
UPDATE connections SET secret_reference = NULL WHERE secret_reference = 'vault-' || id::text;
