ALTER TABLE vox_connections ADD CONSTRAINT vox_connections_id_user_key UNIQUE(id,user_id);

CREATE TABLE source_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    connector_id TEXT NOT NULL,
    source_account_id TEXT,
    source_record_id TEXT NOT NULL,
    record_hash TEXT NOT NULL,
    disposition TEXT NOT NULL DEFAULT 'retained' CHECK (disposition IN ('retained', 'deleted', 'purged', 'transient')),
    temporary_content_ref TEXT,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    raw_deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT source_records_user_source_unique UNIQUE (user_id, connector_id, source_record_id),
    CONSTRAINT source_records_id_user_key UNIQUE (id, user_id)
);

CREATE INDEX source_records_user_disposition_idx ON source_records (user_id, disposition);
CREATE INDEX source_records_hash_idx ON source_records (user_id, record_hash);

CREATE TABLE source_attachments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    source_record_id UUID NOT NULL,
    object_ref TEXT NOT NULL,
    storage_owner_id UUID NOT NULL,
    raw_deleted_at TIMESTAMPTZ,
    content_hash TEXT NOT NULL,
    encryption_metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    parse_state TEXT NOT NULL DEFAULT 'pending' CHECK (parse_state IN ('pending', 'processing', 'parsed', 'failed', 'waiting_user', 'skipped')),
    mime_type TEXT,
    size_bytes BIGINT,
    error TEXT,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT source_attachments_id_user_key UNIQUE (id, user_id),
    CONSTRAINT source_attachments_source_record_tenant_fk
        FOREIGN KEY (source_record_id, user_id) REFERENCES source_records(id, user_id) ON DELETE CASCADE
);

CREATE INDEX source_attachments_user_parse_idx ON source_attachments (user_id, parse_state);

CREATE TABLE connector_sync_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    connector_id TEXT NOT NULL,
    connection_id UUID NOT NULL,
    run_type TEXT NOT NULL CHECK (run_type IN ('live', 'backfill', 'import')),
    status TEXT NOT NULL DEFAULT 'running' CHECK (status IN ('queued', 'running', 'completed', 'failed', 'cancelled')),
    records_extracted INTEGER NOT NULL DEFAULT 0,
    records_ingested INTEGER NOT NULL DEFAULT 0,
    cursor_state JSONB NOT NULL DEFAULT '{}'::jsonb,
    error TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT connector_sync_runs_id_user_key UNIQUE (id, user_id),
    CONSTRAINT connector_sync_runs_connection_tenant_fk
        FOREIGN KEY (connection_id, user_id) REFERENCES vox_connections(id, user_id) ON DELETE CASCADE
);

CREATE INDEX connector_sync_runs_user_status_idx ON connector_sync_runs (user_id, status);

CREATE TABLE connector_coverage (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    connector_id TEXT NOT NULL,
    connection_id UUID NOT NULL,
    coverage_start TIMESTAMPTZ,
    coverage_end TIMESTAMPTZ,
    sync_mode TEXT NOT NULL DEFAULT 'standard',
    is_healthy BOOLEAN NOT NULL DEFAULT true,
    last_checked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT connector_coverage_user_connector_unique UNIQUE (user_id, connector_id),
    CONSTRAINT connector_coverage_id_user_key UNIQUE (id, user_id),
    CONSTRAINT connector_coverage_connection_tenant_fk
        FOREIGN KEY (connection_id, user_id) REFERENCES vox_connections(id, user_id) ON DELETE CASCADE
);

CREATE FUNCTION bind_attachment_storage_owner() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='INSERT' THEN NEW.storage_owner_id := NEW.user_id;
    ELSIF NEW.storage_owner_id IS DISTINCT FROM OLD.storage_owner_id THEN
        RAISE EXCEPTION 'attachment storage ownership cannot be changed';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER source_attachment_storage_owner BEFORE INSERT OR UPDATE ON source_attachments FOR EACH ROW EXECUTE FUNCTION bind_attachment_storage_owner();
