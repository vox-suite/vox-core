-- E05 expansion over the consolidated consumer schema. The reserved legacy
-- scope records provenance for existing channel-owned users; it is never
-- accepted as proof that a host may act for that user.
INSERT INTO platform_deployments (external_key)
VALUES ('vox.legacy.deployment')
ON CONFLICT (external_key) DO NOTHING;

INSERT INTO host_apps (deployment_id, external_key)
SELECT id, 'vox.legacy.channel-host'
FROM platform_deployments
WHERE external_key = 'vox.legacy.deployment'
ON CONFLICT (deployment_id, external_key) DO NOTHING;

-- Verified first-party sign-in gets its own internal scope. It is not a
-- public host credential or a way to import a legacy channel identity.
INSERT INTO platform_deployments (external_key)
VALUES ('vox.standalone.deployment')
ON CONFLICT (external_key) DO NOTHING;

INSERT INTO host_apps (deployment_id, external_key)
SELECT id, 'vox.standalone.web'
FROM platform_deployments
WHERE external_key = 'vox.standalone.deployment'
ON CONFLICT (deployment_id, external_key) DO NOTHING;

INSERT INTO user_contexts (deployment_id, host_app_id, host_user_id, user_id)
SELECT d.id, h.id, u.id::text, u.id
FROM users u
JOIN platform_deployments d ON d.external_key = 'vox.legacy.deployment'
JOIN host_apps h ON h.deployment_id = d.id
    AND h.external_key = 'vox.legacy.channel-host'
LEFT JOIN user_contexts existing ON existing.user_id = u.id
WHERE existing.id IS NULL
ON CONFLICT (user_id) DO NOTHING;

ALTER TABLE user_contexts
    ADD CONSTRAINT user_contexts_id_user_id_key UNIQUE (id, user_id);

-- Every row carrying a user owner gets a stable context. NULL remains valid
-- only for genuinely global rows (jobs, schemas, events, and audit entries).
ALTER TABLE auth_identities ADD COLUMN user_context_id UUID;
ALTER TABLE channel_identities ADD COLUMN user_context_id UUID;
ALTER TABLE auth_sessions ADD COLUMN user_context_id UUID;
ALTER TABLE conversations ADD COLUMN user_context_id UUID;
ALTER TABLE collections ADD COLUMN user_context_id UUID;
ALTER TABLE tasks ADD COLUMN user_context_id UUID;
ALTER TABLE schedules ADD COLUMN user_context_id UUID;
ALTER TABLE jobs ADD COLUMN user_context_id UUID;
ALTER TABLE data_schemas ADD COLUMN user_context_id UUID;
ALTER TABLE records ADD COLUMN user_context_id UUID;
ALTER TABLE devices ADD COLUMN user_context_id UUID;
ALTER TABLE connections ADD COLUMN user_context_id UUID;
ALTER TABLE action_proposals ADD COLUMN user_context_id UUID;
ALTER TABLE action_approvals ADD COLUMN user_context_id UUID;
ALTER TABLE executions ADD COLUMN user_context_id UUID;
ALTER TABLE inbound_events ADD COLUMN user_context_id UUID;
ALTER TABLE audit_events ADD COLUMN user_context_id UUID;

DO $backfill$
DECLARE
    resource_name TEXT;
BEGIN
    FOREACH resource_name IN ARRAY ARRAY[
        'auth_identities', 'channel_identities', 'auth_sessions',
        'conversations', 'collections', 'tasks', 'schedules', 'jobs',
        'data_schemas', 'records', 'devices', 'connections',
        'action_proposals', 'action_approvals', 'executions',
        'inbound_events', 'audit_events'
    ] LOOP
        EXECUTE format(
            'UPDATE %I r SET user_context_id = c.id FROM user_contexts c
             WHERE r.user_id = c.user_id AND r.user_context_id IS NULL',
            resource_name
        );
    END LOOP;
END $backfill$;

-- Existing writers use user_id during rollout. A compatibility trigger fills
-- only the context already bound to that user; it cannot choose or change
-- the owner. Canonical callers may supply the context explicitly.
CREATE FUNCTION resource_context_compatibility() RETURNS trigger
LANGUAGE plpgsql AS $function$
BEGIN
    IF NEW.user_id IS NULL THEN
        IF TG_OP = 'UPDATE' AND TG_TABLE_NAME = 'audit_events'
           AND OLD.user_id IS NOT NULL THEN
            -- audit_events retains its row when users.user_id is SET NULL.
            NEW.user_context_id := NULL;
        ELSIF NEW.user_context_id IS NOT NULL THEN
            RAISE EXCEPTION 'global row cannot have user context'
                USING ERRCODE = '23514';
        END IF;
    ELSIF NEW.user_context_id IS NULL THEN
        SELECT id INTO NEW.user_context_id
        FROM user_contexts WHERE user_id = NEW.user_id;
        IF NEW.user_context_id IS NULL THEN
            RAISE EXCEPTION 'user % has no canonical context', NEW.user_id
                USING ERRCODE = '23503';
        END IF;
    END IF;
    RETURN NEW;
END $function$;

DO $constraints$
DECLARE
    resource_name TEXT;
BEGIN
    FOREACH resource_name IN ARRAY ARRAY[
        'auth_identities', 'channel_identities', 'auth_sessions',
        'conversations', 'collections', 'tasks', 'schedules', 'jobs',
        'data_schemas', 'records', 'devices', 'connections',
        'action_proposals', 'action_approvals', 'executions',
        'inbound_events', 'audit_events'
    ] LOOP
        EXECUTE format(
            'ALTER TABLE %I ADD CONSTRAINT %I
             FOREIGN KEY (user_context_id, user_id)
             REFERENCES user_contexts(id, user_id) ON DELETE RESTRICT',
            resource_name, resource_name || '_context_owner_fk'
        );
        EXECUTE format(
            'CREATE TRIGGER %I BEFORE INSERT OR UPDATE OF user_id, user_context_id
             ON %I FOR EACH ROW EXECUTE FUNCTION resource_context_compatibility()',
            resource_name || '_context_compatibility', resource_name
        );
        EXECUTE format(
            'ALTER TABLE %I ADD CONSTRAINT %I
             CHECK ((user_id IS NULL) = (user_context_id IS NULL))',
            resource_name, resource_name || '_context_presence'
        );
        EXECUTE format(
            'CREATE INDEX %I ON %I (user_context_id)',
            resource_name || '_context_idx', resource_name
        );
    END LOOP;
END $constraints$;

-- The old global conversation key made two users sharing a host-supplied
-- conversation identifier collide. Ownership is part of the key now.
ALTER TABLE conversations DROP CONSTRAINT conversations_channel_external_key;
ALTER TABLE conversations ADD CONSTRAINT conversations_context_channel_external_key
    UNIQUE (user_context_id, channel, external_conversation_id);

-- A matching owner on the child row is also required for references to
-- another user-owned resource. The older single-column foreign keys remain
-- for their existing delete behavior; these keys add owner integrity.
ALTER TABLE auth_identities ADD CONSTRAINT auth_identities_id_user_key UNIQUE (id, user_id);
ALTER TABLE channel_identities ADD CONSTRAINT channel_identities_id_user_key UNIQUE (id, user_id);
ALTER TABLE collections ADD CONSTRAINT collections_id_user_key UNIQUE (id, user_id);
ALTER TABLE tasks ADD CONSTRAINT tasks_id_user_key UNIQUE (id, user_id);
ALTER TABLE schedules ADD CONSTRAINT schedules_id_user_key UNIQUE (id, user_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_id_user_key UNIQUE (id, user_id);
ALTER TABLE devices ADD CONSTRAINT devices_id_user_key UNIQUE (id, user_id);
ALTER TABLE connections ADD CONSTRAINT connections_id_user_key UNIQUE (id, user_id);
ALTER TABLE action_proposals ADD CONSTRAINT action_proposals_id_user_key UNIQUE (id, user_id);
ALTER TABLE action_approvals ADD CONSTRAINT action_approvals_id_user_key UNIQUE (id, user_id);
ALTER TABLE executions ADD CONSTRAINT executions_id_user_key UNIQUE (id, user_id);
ALTER TABLE inbound_events ADD CONSTRAINT inbound_events_id_user_key UNIQUE (id, user_id);

ALTER TABLE auth_sessions ADD CONSTRAINT auth_sessions_identity_owner_fk
    FOREIGN KEY (auth_identity_id, user_id) REFERENCES auth_identities(id, user_id)
    ON DELETE SET NULL (auth_identity_id);
ALTER TABLE auth_sessions ADD CONSTRAINT auth_sessions_device_owner_fk
    FOREIGN KEY (device_id, user_id) REFERENCES devices(id, user_id)
    ON DELETE SET NULL (device_id);
ALTER TABLE conversations ADD CONSTRAINT conversations_channel_identity_owner_fk
    FOREIGN KEY (channel_identity_id, user_id) REFERENCES channel_identities(id, user_id)
    ON DELETE SET NULL (channel_identity_id);
ALTER TABLE tasks ADD CONSTRAINT tasks_collection_owner_fk
    FOREIGN KEY (collection_id, user_id) REFERENCES collections(id, user_id)
    ON DELETE SET NULL (collection_id);
ALTER TABLE schedules ADD CONSTRAINT schedules_task_owner_fk
    FOREIGN KEY (task_id, user_id) REFERENCES tasks(id, user_id)
    ON DELETE SET NULL (task_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_task_owner_fk
    FOREIGN KEY (task_id, user_id) REFERENCES tasks(id, user_id)
    ON DELETE SET NULL (task_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_schedule_owner_fk
    FOREIGN KEY (schedule_id, user_id) REFERENCES schedules(id, user_id)
    ON DELETE SET NULL (schedule_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_device_owner_fk
    FOREIGN KEY (assigned_device_id, user_id) REFERENCES devices(id, user_id)
    ON DELETE SET NULL (assigned_device_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_source_event_owner_fk
    FOREIGN KEY (source_event_id, user_id) REFERENCES inbound_events(id, user_id)
    ON DELETE SET NULL (source_event_id);
ALTER TABLE jobs ADD CONSTRAINT jobs_owned_refs_have_user
    CHECK ((task_id IS NULL AND schedule_id IS NULL AND assigned_device_id IS NULL
        AND source_event_id IS NULL) OR user_id IS NOT NULL);
ALTER TABLE records ADD CONSTRAINT records_collection_owner_fk
    FOREIGN KEY (collection_id, user_id) REFERENCES collections(id, user_id)
    ON DELETE SET NULL (collection_id);
ALTER TABLE records ADD CONSTRAINT records_source_event_owner_fk
    FOREIGN KEY (source_event_id, user_id) REFERENCES inbound_events(id, user_id)
    ON DELETE SET NULL (source_event_id);
ALTER TABLE action_proposals ADD CONSTRAINT action_proposals_task_owner_fk
    FOREIGN KEY (task_id, user_id) REFERENCES tasks(id, user_id)
    ON DELETE SET NULL (task_id);
ALTER TABLE action_proposals ADD CONSTRAINT action_proposals_job_owner_fk
    FOREIGN KEY (job_id, user_id) REFERENCES jobs(id, user_id)
    ON DELETE SET NULL (job_id);
ALTER TABLE action_proposals ADD CONSTRAINT action_proposals_connection_owner_fk
    FOREIGN KEY (connection_id, user_id) REFERENCES connections(id, user_id)
    ON DELETE SET NULL (connection_id);
ALTER TABLE action_approvals ADD CONSTRAINT action_approvals_proposal_owner_fk
    FOREIGN KEY (proposal_id, user_id) REFERENCES action_proposals(id, user_id)
    ON DELETE CASCADE;
ALTER TABLE executions ADD CONSTRAINT executions_proposal_owner_fk
    FOREIGN KEY (proposal_id, user_id) REFERENCES action_proposals(id, user_id)
    ON DELETE RESTRICT;
ALTER TABLE executions ADD CONSTRAINT executions_approval_owner_fk
    FOREIGN KEY (approval_id, user_id) REFERENCES action_approvals(id, user_id)
    ON DELETE RESTRICT;
ALTER TABLE executions ADD CONSTRAINT executions_connection_owner_fk
    FOREIGN KEY (connection_id, user_id) REFERENCES connections(id, user_id)
    ON DELETE SET NULL (connection_id);
ALTER TABLE inbound_events ADD CONSTRAINT inbound_events_execution_owner_fk
    FOREIGN KEY (execution_id, user_id) REFERENCES executions(id, user_id)
    ON DELETE SET NULL (execution_id);
ALTER TABLE inbound_events ADD CONSTRAINT inbound_events_execution_has_user
    CHECK (execution_id IS NULL OR user_id IS NOT NULL);

-- Stop the migration if a malformed preexisting row escaped the backfill.
DO $reconcile$
DECLARE
    resource_name TEXT;
    orphan_count BIGINT;
BEGIN
    FOREACH resource_name IN ARRAY ARRAY[
        'auth_identities', 'channel_identities', 'auth_sessions',
        'conversations', 'collections', 'tasks', 'schedules', 'jobs',
        'data_schemas', 'records', 'devices', 'connections',
        'action_proposals', 'action_approvals', 'executions',
        'inbound_events', 'audit_events'
    ] LOOP
        EXECUTE format(
            'SELECT count(*) FROM %I r LEFT JOIN user_contexts c
             ON c.id = r.user_context_id AND c.user_id = r.user_id
             WHERE r.user_id IS NOT NULL AND c.id IS NULL',
            resource_name
        ) INTO orphan_count;
        IF orphan_count > 0 THEN
            RAISE EXCEPTION '% has % owner orphans', resource_name, orphan_count;
        END IF;
    END LOOP;
END $reconcile$;
