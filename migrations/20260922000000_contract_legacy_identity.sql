-- Pre-launch authority contraction. Legacy-owned rows are intentionally not
-- migrated: rebuild the development/test database from the canonical schema.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM conversations WHERE user_context_id IS NULL)
       OR EXISTS (SELECT 1 FROM scheduled_tasks WHERE user_context_id IS NULL)
       OR EXISTS (SELECT 1 FROM tasks WHERE user_context_id IS NULL)
       OR EXISTS (SELECT 1 FROM outbound_calls WHERE user_context_id IS NULL)
    THEN
        RAISE EXCEPTION USING
            MESSAGE = 'null-context runtime records remain; rebuild this pre-launch database from a clean baseline',
            ERRCODE = 'check_violation';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM user_contexts uc
        JOIN platform_deployments d ON d.id = uc.deployment_id
        JOIN host_apps h ON h.id = uc.host_app_id
        WHERE d.external_key = 'vox.legacy.deployment'
           OR h.external_key = 'vox.legacy.channel-host'
    ) THEN
        RAISE EXCEPTION USING
            MESSAGE = 'legacy-owned records remain; rebuild this pre-launch database from a clean baseline',
            ERRCODE = 'check_violation';
    END IF;
END $$;

ALTER TABLE events ADD COLUMN user_context_id UUID;
UPDATE events e
SET user_context_id = uc.id
FROM user_contexts uc
WHERE uc.user_id = e.user_id;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM events WHERE user_context_id IS NULL) THEN
        RAISE EXCEPTION USING
            MESSAGE = 'legacy events remain; rebuild this pre-launch database from a clean baseline',
            ERRCODE = 'check_violation';
    END IF;
END $$;

ALTER TABLE events ALTER COLUMN user_context_id SET NOT NULL;
ALTER TABLE events
    ADD CONSTRAINT events_user_context_owner_fkey
    FOREIGN KEY (user_context_id, user_id)
    REFERENCES user_contexts(id, user_id)
    ON DELETE RESTRICT;
ALTER TABLE events DROP CONSTRAINT events_idempotency_key_key;
ALTER TABLE events
    ADD CONSTRAINT events_context_idempotency_key
    UNIQUE (user_context_id, idempotency_key);
CREATE INDEX events_user_context_occurred_idx
    ON events (user_context_id, occurred_at DESC);

ALTER TABLE user_identities RENAME TO user_contact_points;
ALTER TABLE user_contact_points ADD COLUMN user_context_id UUID;
UPDATE user_contact_points cp
SET user_context_id = uc.id
FROM user_contexts uc
WHERE uc.user_id = cp.user_id;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM user_contact_points WHERE user_context_id IS NULL) THEN
        RAISE EXCEPTION USING
            MESSAGE = 'legacy contact points remain; rebuild this pre-launch database from a clean baseline',
            ERRCODE = 'check_violation';
    END IF;
END $$;

ALTER TABLE user_contact_points ALTER COLUMN user_context_id SET NOT NULL;
ALTER TABLE user_contact_points
    ADD CONSTRAINT user_contact_points_context_owner_fkey
    FOREIGN KEY (user_context_id, user_id)
    REFERENCES user_contexts(id, user_id)
    ON DELETE CASCADE;
ALTER TABLE user_contact_points
    DROP CONSTRAINT user_identities_channel_external_key;
ALTER TABLE user_contact_points
    ADD CONSTRAINT user_contact_points_context_channel_external_key
    UNIQUE (user_context_id, channel, external_id);
ALTER TABLE user_contact_points
    RENAME CONSTRAINT user_identities_channel_not_empty TO user_contact_points_channel_not_empty;
ALTER TABLE user_contact_points
    RENAME CONSTRAINT user_identities_external_not_empty TO user_contact_points_external_not_empty;
CREATE INDEX user_contact_points_owner_idx
    ON user_contact_points (user_context_id, user_id, channel);

DROP INDEX conversations_legacy_channel_external_key;
ALTER TABLE conversations ALTER COLUMN user_context_id SET NOT NULL;
ALTER TABLE conversations
    DROP COLUMN active_user_id,
    DROP COLUMN verification_state;
ALTER TABLE scheduled_tasks ALTER COLUMN user_context_id SET NOT NULL;
ALTER TABLE tasks ALTER COLUMN user_context_id SET NOT NULL;
ALTER TABLE outbound_calls ALTER COLUMN user_context_id SET NOT NULL;

DELETE FROM host_apps
WHERE external_key = 'vox.legacy.channel-host'
  AND deployment_id IN (
      SELECT id FROM platform_deployments WHERE external_key = 'vox.legacy.deployment'
  );
DELETE FROM platform_deployments WHERE external_key = 'vox.legacy.deployment';
