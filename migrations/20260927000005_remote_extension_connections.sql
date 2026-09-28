-- A connection belongs to either a deployment integration or one user-owned
-- remote extension. OAuth credentials remain separate, encrypted custody data.
ALTER TABLE remote_extensions
    ADD CONSTRAINT remote_extensions_id_context_key UNIQUE (id, user_context_id);

ALTER TABLE external_connections ALTER COLUMN integration_id DROP NOT NULL;
ALTER TABLE external_connections
    ADD COLUMN remote_extension_id UUID;
ALTER TABLE external_connections
    ADD CONSTRAINT external_connections_one_source
        CHECK ((integration_id IS NOT NULL) <> (remote_extension_id IS NOT NULL));
ALTER TABLE external_connections
    ADD CONSTRAINT external_connections_remote_extension_context_fk
        FOREIGN KEY (remote_extension_id, user_context_id)
        REFERENCES remote_extensions(id, user_context_id) ON DELETE RESTRICT;
CREATE UNIQUE INDEX external_connections_one_account_per_remote_extension
    ON external_connections(user_context_id, remote_extension_id)
    WHERE remote_extension_id IS NOT NULL;
