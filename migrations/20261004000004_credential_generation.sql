ALTER TABLE vox_connections ADD COLUMN credential_generation uuid NOT NULL DEFAULT gen_random_uuid();
