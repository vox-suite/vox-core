CREATE TABLE agent_delegation_permissions (
 id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
 user_context_id uuid NOT NULL REFERENCES user_contexts(id),
 requester_agent_id uuid NOT NULL,
 specialist_agent_id uuid NOT NULL,
 shared_preferences jsonb NOT NULL DEFAULT '[]' CHECK(jsonb_typeof(shared_preferences)='array' AND jsonb_array_length(shared_preferences)<=8),
 scope jsonb NOT NULL CHECK(jsonb_typeof(scope)='object'),
 parent_run_id uuid REFERENCES assigned_task_runs(job_id),
 mode text NOT NULL CHECK(mode IN ('once','remembered')),
 state text NOT NULL DEFAULT 'enabled' CHECK(state IN ('enabled','revoked')),
 used boolean NOT NULL DEFAULT false,
 created_at timestamptz NOT NULL DEFAULT now(),
 revoked_at timestamptz,
 FOREIGN KEY(user_context_id,requester_agent_id) REFERENCES agent_definitions(owner_user_context_id,id),
 FOREIGN KEY(user_context_id,specialist_agent_id) REFERENCES agent_definitions(owner_user_context_id,id),
 CHECK(requester_agent_id<>specialist_agent_id),
 CHECK((mode='once')=(parent_run_id IS NOT NULL)),
 CHECK(jsonb_typeof(scope->'capabilities')='array' AND jsonb_array_length(scope->'capabilities') BETWEEN 1 AND 32),
 CHECK(scope->'skills'='[]'::jsonb)
);
CREATE INDEX delegation_context_idx ON agent_delegation_permissions(user_context_id,created_at DESC);
CREATE FUNCTION immutable_delegation_scope() RETURNS trigger LANGUAGE plpgsql SET search_path = public, pg_temp AS $$
BEGIN
 IF ROW(NEW.user_context_id,NEW.requester_agent_id,NEW.specialist_agent_id,NEW.scope,NEW.shared_preferences,NEW.parent_run_id,NEW.mode)
 IS DISTINCT FROM ROW(OLD.user_context_id,OLD.requester_agent_id,OLD.specialist_agent_id,OLD.scope,OLD.shared_preferences,OLD.parent_run_id,OLD.mode) THEN
 RAISE EXCEPTION 'delegation consent scope is immutable';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER delegation_scope_immutable BEFORE UPDATE ON agent_delegation_permissions FOR EACH ROW EXECUTE FUNCTION immutable_delegation_scope();
ALTER TABLE assigned_task_runs ADD CONSTRAINT assigned_delegation_permission_fk FOREIGN KEY(delegation_permission_id) REFERENCES agent_delegation_permissions(id);
ALTER TABLE assigned_task_runs ADD COLUMN result_received_at timestamptz;

ALTER TABLE agent_delegation_permissions ENABLE ROW LEVEL SECURITY;
REVOKE ALL ON agent_delegation_permissions FROM PUBLIC;
ALTER TABLE assigned_task_runs ADD CONSTRAINT assigned_context_job_unique UNIQUE(user_context_id,job_id);
ALTER TABLE agent_delegation_permissions ADD CONSTRAINT delegation_parent_context_fk FOREIGN KEY(user_context_id,parent_run_id) REFERENCES assigned_task_runs(user_context_id,job_id);
ALTER TABLE assigned_task_runs ADD CONSTRAINT assigned_parent_context_fk FOREIGN KEY(user_context_id,parent_run_id) REFERENCES assigned_task_runs(user_context_id,job_id);
ALTER TABLE agent_delegation_permissions ADD CONSTRAINT delegation_context_id_unique UNIQUE(user_context_id,id);
ALTER TABLE assigned_task_runs ADD CONSTRAINT assigned_permission_context_fk FOREIGN KEY(user_context_id,delegation_permission_id) REFERENCES agent_delegation_permissions(user_context_id,id);
