DROP TABLE IF EXISTS connection_authorization_sessions;
DROP TABLE IF EXISTS job_attempts;

ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_owned_refs_have_user;
ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_assigned_device_fk;
ALTER TABLE jobs DROP CONSTRAINT IF EXISTS jobs_device_owner_fk;
ALTER TABLE jobs DROP COLUMN IF EXISTS assigned_device_id;
ALTER TABLE jobs ADD CONSTRAINT jobs_owned_refs_have_user
    CHECK ((span_id IS NULL AND schedule_id IS NULL AND source_event_id IS NULL) OR user_id IS NOT NULL);
