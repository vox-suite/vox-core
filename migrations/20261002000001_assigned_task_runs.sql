-- Assigned work is executed only by the governed worker. Actor/authority never
-- comes from a mutable checkpoint; proposal-only jobs have no assigned binding.
ALTER TABLE jobs ADD CONSTRAINT jobs_context_id_unique UNIQUE(user_context_id,id);
ALTER TABLE agent_model_configurations ADD CONSTRAINT model_actor_version_unique UNIQUE(agent_definition_id,id,version);
CREATE TABLE assigned_task_runs (
    job_id uuid PRIMARY KEY,
    user_context_id uuid NOT NULL,
    agent_id uuid NOT NULL,
    instruction_version integer NOT NULL,
    model_configuration_id uuid NOT NULL,
    model_version integer NOT NULL,
    actor_snapshot jsonb NOT NULL CHECK(jsonb_typeof(actor_snapshot)='object'),
    authority jsonb NOT NULL CHECK(jsonb_typeof(authority)='object'),
    task_instruction text NOT NULL CHECK(octet_length(task_instruction) BETWEEN 1 AND 20000),
    parent_run_id uuid REFERENCES assigned_task_runs(job_id) ON DELETE RESTRICT,
    delegation_permission_id uuid,
    deadline_at timestamptz NOT NULL,
    max_tool_calls integer NOT NULL DEFAULT 24 CHECK(max_tool_calls BETWEEN 1 AND 64),
    tool_calls integer NOT NULL DEFAULT 0 CHECK(tool_calls>=0),
    pending_proposal_id uuid REFERENCES action_proposals(id) ON DELETE RESTRICT,
    result jsonb NOT NULL DEFAULT '{}'::jsonb CHECK(jsonb_typeof(result)='object'),
    FOREIGN KEY(user_context_id,job_id) REFERENCES jobs(user_context_id,id) ON DELETE CASCADE,
    FOREIGN KEY(user_context_id,agent_id) REFERENCES agent_definitions(owner_user_context_id,id) ON DELETE RESTRICT,
    FOREIGN KEY(agent_id,instruction_version) REFERENCES agent_instruction_versions(agent_id,version) ON DELETE RESTRICT,
    FOREIGN KEY(agent_id,model_configuration_id,model_version) REFERENCES agent_model_configurations(agent_definition_id,id,version) ON DELETE RESTRICT,
    CHECK(actor_snapshot->'definition'->>'id'=agent_id::text),
    CHECK((actor_snapshot->'definition'->>'instruction_version')::integer=instruction_version),
    CHECK(actor_snapshot->'model_configuration'->>'id'=model_configuration_id::text),
    CHECK((actor_snapshot->'model_configuration'->>'version')::integer=model_version),
    CHECK(jsonb_typeof(authority->'capabilities')='array' AND jsonb_array_length(authority->'capabilities')<=256),
    CHECK(jsonb_typeof(authority->'skills')='array' AND jsonb_array_length(authority->'skills')<=128),
    CHECK(parent_run_id IS NULL OR parent_run_id<>job_id),
    CHECK(delegation_permission_id IS NULL OR parent_run_id IS NOT NULL)
);
CREATE INDEX assigned_runs_parent_idx ON assigned_task_runs(parent_run_id) WHERE parent_run_id IS NOT NULL;
CREATE FUNCTION immutable_assigned_run_actor() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF ROW(NEW.job_id,NEW.user_context_id,NEW.agent_id,NEW.instruction_version,
           NEW.model_configuration_id,NEW.model_version,NEW.actor_snapshot,NEW.authority,
           NEW.task_instruction,NEW.parent_run_id,NEW.delegation_permission_id,NEW.deadline_at,NEW.max_tool_calls)
       IS DISTINCT FROM
       ROW(OLD.job_id,OLD.user_context_id,OLD.agent_id,OLD.instruction_version,
           OLD.model_configuration_id,OLD.model_version,OLD.actor_snapshot,OLD.authority,
           OLD.task_instruction,OLD.parent_run_id,OLD.delegation_permission_id,OLD.deadline_at,OLD.max_tool_calls) THEN
        RAISE EXCEPTION 'assigned run actor and authority are immutable';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER assigned_run_actor_immutable BEFORE UPDATE ON assigned_task_runs
    FOR EACH ROW EXECUTE FUNCTION immutable_assigned_run_actor();
