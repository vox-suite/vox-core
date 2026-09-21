INSERT INTO platform_deployments (external_key)
VALUES ('vox.legacy.deployment')
ON CONFLICT (external_key) DO NOTHING;

INSERT INTO host_apps (deployment_id, external_key)
SELECT id, 'vox.legacy.channel-host'
FROM platform_deployments
WHERE external_key = 'vox.legacy.deployment'
ON CONFLICT (deployment_id, external_key) DO NOTHING;

INSERT INTO user_contexts (deployment_id, host_app_id, host_user_id, user_id)
SELECT d.id, h.id, u.id::text, u.id
FROM users u
JOIN platform_deployments d
    ON d.external_key = 'vox.legacy.deployment'
JOIN host_apps h
    ON h.deployment_id = d.id
   AND h.external_key = 'vox.legacy.channel-host'
LEFT JOIN user_contexts uc ON uc.user_id = u.id
WHERE uc.id IS NULL
ON CONFLICT (user_id) DO NOTHING;

ALTER TABLE user_contexts
    ADD CONSTRAINT user_contexts_id_user_id_key UNIQUE (id, user_id);

ALTER TABLE conversations ADD COLUMN user_context_id UUID;
ALTER TABLE scheduled_tasks ADD COLUMN user_context_id UUID;
ALTER TABLE tasks ADD COLUMN user_context_id UUID;
ALTER TABLE actions ADD COLUMN user_context_id UUID;

UPDATE conversations r SET user_context_id = c.id
FROM user_contexts c
WHERE c.user_id = r.user_id AND r.user_context_id IS NULL;

UPDATE scheduled_tasks r SET user_context_id = c.id
FROM user_contexts c
WHERE c.user_id = r.user_id AND r.user_context_id IS NULL;

UPDATE tasks r SET user_context_id = c.id
FROM user_contexts c
WHERE c.user_id = r.user_id AND r.user_context_id IS NULL;

UPDATE actions r SET user_context_id = c.id
FROM user_contexts c
WHERE c.user_id = r.user_id AND r.user_context_id IS NULL;

ALTER TABLE conversations
    ADD CONSTRAINT conversations_user_context_owner_fkey
    FOREIGN KEY (user_context_id, user_id)
    REFERENCES user_contexts(id, user_id)
    ON DELETE RESTRICT
    NOT VALID;
ALTER TABLE conversations VALIDATE CONSTRAINT conversations_user_context_owner_fkey;

ALTER TABLE scheduled_tasks
    ADD CONSTRAINT scheduled_tasks_user_context_owner_fkey
    FOREIGN KEY (user_context_id, user_id)
    REFERENCES user_contexts(id, user_id)
    ON DELETE RESTRICT
    NOT VALID;
ALTER TABLE scheduled_tasks VALIDATE CONSTRAINT scheduled_tasks_user_context_owner_fkey;

ALTER TABLE tasks
    ADD CONSTRAINT tasks_user_context_owner_fkey
    FOREIGN KEY (user_context_id, user_id)
    REFERENCES user_contexts(id, user_id)
    ON DELETE RESTRICT
    NOT VALID;
ALTER TABLE tasks VALIDATE CONSTRAINT tasks_user_context_owner_fkey;

ALTER TABLE actions
    ADD CONSTRAINT actions_user_context_owner_fkey
    FOREIGN KEY (user_context_id, user_id)
    REFERENCES user_contexts(id, user_id)
    ON DELETE RESTRICT
    NOT VALID;
ALTER TABLE actions VALIDATE CONSTRAINT actions_user_context_owner_fkey;

ALTER TABLE projects ADD CONSTRAINT projects_id_user_id_key UNIQUE (id, user_id);
ALTER TABLE scheduled_tasks
    ADD CONSTRAINT scheduled_tasks_id_user_id_key UNIQUE (id, user_id);
ALTER TABLE tasks ADD CONSTRAINT tasks_id_user_id_key UNIQUE (id, user_id);
ALTER TABLE events ADD CONSTRAINT events_id_user_id_key UNIQUE (id, user_id);
ALTER TABLE client_devices
    ADD CONSTRAINT client_devices_id_user_id_key UNIQUE (id, user_id);

ALTER TABLE tasks
    ADD CONSTRAINT tasks_project_owner_fkey
    FOREIGN KEY (project_id, user_id)
    REFERENCES projects(id, user_id)
    NOT VALID;
ALTER TABLE tasks VALIDATE CONSTRAINT tasks_project_owner_fkey;

ALTER TABLE actions
    ADD CONSTRAINT actions_schedule_owner_fkey
    FOREIGN KEY (schedule_id, user_id)
    REFERENCES scheduled_tasks(id, user_id)
    NOT VALID;
ALTER TABLE actions VALIDATE CONSTRAINT actions_schedule_owner_fkey;

ALTER TABLE actions
    ADD CONSTRAINT actions_task_owner_fkey
    FOREIGN KEY (task_id, user_id)
    REFERENCES tasks(id, user_id)
    NOT VALID;
ALTER TABLE actions VALIDATE CONSTRAINT actions_task_owner_fkey;

ALTER TABLE actions
    ADD CONSTRAINT actions_event_owner_fkey
    FOREIGN KEY (event_id, user_id)
    REFERENCES events(id, user_id)
    NOT VALID;
ALTER TABLE actions VALIDATE CONSTRAINT actions_event_owner_fkey;

ALTER TABLE actions
    ADD CONSTRAINT actions_target_device_owner_fkey
    FOREIGN KEY (target_device_id, user_id)
    REFERENCES client_devices(id, user_id)
    NOT VALID;
ALTER TABLE actions VALIDATE CONSTRAINT actions_target_device_owner_fkey;

ALTER TABLE conversations DROP CONSTRAINT conversations_channel_external_key;
ALTER TABLE conversations
    ADD CONSTRAINT conversations_context_channel_external_key
    UNIQUE (user_context_id, channel, external_id);
CREATE UNIQUE INDEX conversations_legacy_channel_external_key
    ON conversations (channel, external_id)
    WHERE user_context_id IS NULL;

CREATE INDEX conversations_user_context_started_idx
    ON conversations (user_context_id, started_at DESC);
CREATE INDEX scheduled_tasks_user_context_state_idx
    ON scheduled_tasks (user_context_id, state);
CREATE INDEX tasks_user_context_status_idx
    ON tasks (user_context_id, status);
CREATE INDEX actions_user_context_state_idx
    ON actions (user_context_id, state);

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM conversations WHERE user_context_id IS NULL
        UNION ALL
        SELECT 1 FROM scheduled_tasks WHERE user_context_id IS NULL
        UNION ALL
        SELECT 1 FROM tasks WHERE user_context_id IS NULL
        UNION ALL
        SELECT 1 FROM actions WHERE user_context_id IS NULL
    ) THEN
        RAISE EXCEPTION 'resource user-context backfill left orphaned rows';
    END IF;
END $$;
