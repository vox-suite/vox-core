ALTER TABLE jobs ADD COLUMN priority SMALLINT NOT NULL DEFAULT 0;

DROP INDEX jobs_queued_idx;
CREATE INDEX jobs_queued_idx ON jobs (priority DESC, available_at, id) WHERE state = 'pending';
