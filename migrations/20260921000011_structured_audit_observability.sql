CREATE TABLE audit_events (
    cursor BIGSERIAL PRIMARY KEY,
    schema_version INTEGER NOT NULL DEFAULT 1 CHECK (schema_version = 1),
    user_context_id UUID REFERENCES user_contexts(id) ON DELETE RESTRICT,
    actor_type TEXT NOT NULL CHECK (actor_type IN ('user','host_app','system','operator')),
    actor_reference TEXT,
    event_type TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    aggregate_id UUID,
    task_id UUID REFERENCES tasks(id) ON DELETE RESTRICT,
    task_run_id UUID REFERENCES task_runs(id) ON DELETE RESTRICT,
    proposal_id UUID REFERENCES action_proposals(id) ON DELETE RESTRICT,
    approval_id UUID REFERENCES action_approvals(id) ON DELETE RESTRICT,
    execution_id UUID REFERENCES executions(id) ON DELETE RESTRICT,
    attempt_id UUID REFERENCES execution_attempts(id) ON DELETE RESTRICT,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT audit_events_details_object CHECK (jsonb_typeof(details) = 'object'),
    CONSTRAINT audit_events_type_not_empty CHECK (length(btrim(event_type)) BETWEEN 1 AND 255)
);
CREATE INDEX audit_events_context_cursor_idx ON audit_events (user_context_id, cursor);
CREATE INDEX audit_events_aggregate_idx ON audit_events (aggregate_type, aggregate_id, cursor);

CREATE TABLE audit_sink_definitions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    endpoint_origin TEXT NOT NULL,
    delivery_secret_key TEXT NOT NULL,
    exported_categories TEXT[] NOT NULL,
    state TEXT NOT NULL DEFAULT 'disabled' CHECK (state IN ('enabled','disabled','unhealthy')),
    schema_version INTEGER NOT NULL DEFAULT 1 CHECK (schema_version = 1),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT audit_sinks_name_not_empty CHECK (length(btrim(name)) BETWEEN 1 AND 255),
    CONSTRAINT audit_sinks_key_not_empty CHECK (length(btrim(delivery_secret_key)) BETWEEN 1 AND 255)
);

CREATE TABLE audit_sink_deliveries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    sink_id UUID NOT NULL REFERENCES audit_sink_definitions(id) ON DELETE RESTRICT,
    audit_cursor BIGINT NOT NULL REFERENCES audit_events(cursor) ON DELETE RESTRICT,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','leased','delivered','failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0 AND attempts <= 8),
    lease_owner TEXT,
    lease_expires_at TIMESTAMPTZ,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT audit_sink_delivery_once UNIQUE (sink_id, audit_cursor)
);
CREATE INDEX audit_sink_deliveries_claim_idx ON audit_sink_deliveries (state, next_attempt_at);

CREATE OR REPLACE FUNCTION enqueue_audit_sink_deliveries() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO audit_sink_deliveries (sink_id,audit_cursor)
  SELECT id,NEW.cursor FROM audit_sink_definitions WHERE state='enabled';
  RETURN NEW;
END; $$;
CREATE TRIGGER audit_event_delivery_outbox AFTER INSERT ON audit_events FOR EACH ROW EXECUTE FUNCTION enqueue_audit_sink_deliveries();

CREATE OR REPLACE FUNCTION audit_task_transition() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='INSERT' OR NEW.status IS DISTINCT FROM OLD.status THEN
    INSERT INTO audit_events (user_context_id,actor_type,event_type,aggregate_type,aggregate_id,task_id,details)
    VALUES (NEW.user_context_id,'system','task.state_changed','task',NEW.id,NEW.id,jsonb_build_object('state',NEW.status));
  END IF;
  RETURN NEW;
END; $$;
CREATE TRIGGER audit_tasks AFTER INSERT OR UPDATE OF status ON tasks FOR EACH ROW EXECUTE FUNCTION audit_task_transition();

CREATE OR REPLACE FUNCTION audit_proposal_transition() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='INSERT' OR NEW.state IS DISTINCT FROM OLD.state THEN
    INSERT INTO audit_events (user_context_id,actor_type,event_type,aggregate_type,aggregate_id,task_id,task_run_id,proposal_id,details)
    VALUES (NEW.user_context_id,'system','proposal.state_changed','proposal',NEW.id,NEW.task_id,NEW.task_run_id,NEW.id,
      jsonb_strip_nulls(jsonb_build_object('state',NEW.state,'capability',NEW.capability_external_key,'details_hash',encode(NEW.details_hash,'hex'),'provider',NEW.details#>>'{execution,provider_external_key}','model',NEW.details#>>'{execution,model_identifier}','account_reference_token',encode(digest(coalesce(NEW.details#>>'{execution,account_reference}',''),'sha256'),'hex'),'connection_id',NEW.details#>>'{execution,connection_id}','price_amount_minor',NEW.details#>>'{execution,price_amount_minor}','price_currency',NEW.details#>>'{execution,price_currency}','expires_at',NEW.expires_at)));
  END IF;
  RETURN NEW;
END; $$;
CREATE TRIGGER audit_proposals AFTER INSERT OR UPDATE OF state ON action_proposals FOR EACH ROW EXECUTE FUNCTION audit_proposal_transition();

CREATE OR REPLACE FUNCTION audit_execution_transition() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='INSERT' OR NEW.state IS DISTINCT FROM OLD.state THEN
    INSERT INTO audit_events (user_context_id,actor_type,event_type,aggregate_type,aggregate_id,proposal_id,approval_id,execution_id,details)
    VALUES (NEW.user_context_id,'system','execution.state_changed','execution',NEW.id,NEW.proposal_id,NEW.approval_id,NEW.id,
      jsonb_strip_nulls(jsonb_build_object('state',NEW.state,'integration',NEW.integration_external_key,'capability',NEW.capability_external_key,'connection_id',NEW.connection_id,'provider_reference',NEW.provider_reference,'error_code',NEW.error_code,'policy_hash',encode(digest(NEW.policy_snapshot::text,'sha256'),'hex'))));
  END IF;
  RETURN NEW;
END; $$;
CREATE TRIGGER audit_executions AFTER INSERT OR UPDATE OF state ON executions FOR EACH ROW EXECUTE FUNCTION audit_execution_transition();

CREATE OR REPLACE FUNCTION audit_grant_transition() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='INSERT' OR NEW.state IS DISTINCT FROM OLD.state THEN
    INSERT INTO audit_events (user_context_id,actor_type,event_type,aggregate_type,aggregate_id,details)
    VALUES (NEW.user_context_id,'system','grant.state_changed','grant',NEW.id,jsonb_build_object('state',NEW.state,'connection_id',NEW.connection_id,'capability',NEW.capability_external_key,'agent_definition_id',NEW.agent_definition_id));
  END IF;
  RETURN NEW;
END; $$;
CREATE TRIGGER audit_grants AFTER INSERT OR UPDATE OF state ON agent_capability_grants FOR EACH ROW EXECUTE FUNCTION audit_grant_transition();
