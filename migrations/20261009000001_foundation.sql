CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE SCHEMA IF NOT EXISTS extensions;
CREATE EXTENSION IF NOT EXISTS vector WITH SCHEMA extensions;
CREATE EXTENSION IF NOT EXISTS btree_gist;
SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

CREATE FUNCTION public.bind_owned_context() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $$
BEGIN
    IF NEW.user_id IS NOT NULL AND NEW.user_context_id IS NULL THEN
        SELECT id INTO NEW.user_context_id FROM user_contexts WHERE user_id=NEW.user_id;
        IF NEW.user_context_id IS NULL THEN RAISE EXCEPTION 'user context is required'; END IF;
    END IF;
    RETURN NEW;
END $$;

CREATE FUNCTION public.bump_span_revisions() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    INSERT INTO span_revisions (user_id, revision)
    SELECT DISTINCT user_id, 1 FROM changed
    ON CONFLICT (user_id) DO UPDATE SET revision = span_revisions.revision + 1;
    RETURN NULL;
END
$$;

CREATE FUNCTION public.enqueue_status_webhooks() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $$
BEGIN
    INSERT INTO status_webhook_deliveries (subscription_id, event_cursor)
    SELECT id, NEW.cursor FROM status_webhook_subscriptions
    WHERE user_context_id = NEW.user_context_id AND state = 'enabled';
    RETURN NEW;
END;
$$;

CREATE FUNCTION public.immutable_assigned_run_actor() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $$
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

CREATE FUNCTION public.immutable_delegation_scope() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $$
BEGIN
 IF ROW(NEW.user_context_id,NEW.requester_agent_id,NEW.specialist_agent_id,NEW.scope,NEW.shared_preferences,NEW.parent_run_id,NEW.mode)
 IS DISTINCT FROM ROW(OLD.user_context_id,OLD.requester_agent_id,OLD.specialist_agent_id,OLD.scope,OLD.shared_preferences,OLD.parent_run_id,OLD.mode) THEN
 RAISE EXCEPTION 'delegation consent scope is immutable';
 END IF;
 RETURN NEW;
END $$;

CREATE FUNCTION public.index_skill_external_key() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $$
BEGIN
    SELECT external_key INTO NEW.search_external_key FROM skill_packages WHERE id=NEW.skill_id;
    RETURN NEW;
END $$;

CREATE FUNCTION public.merge_user_accounts(old_user uuid, new_user uuid) RETURNS void
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $_$
DECLARE
    old_ctx UUID;
    new_ctx UUID;
    fk record;
    set_parts TEXT[];
    where_parts TEXT[];
    set_clause TEXT;
    where_clause TEXT;
    parent_col TEXT;
    child_col TEXT;
    idx INT;
    row_ref record;
BEGIN
    IF old_user = new_user THEN
        RETURN;
    END IF;

    PERFORM 1 FROM users WHERE id IN (old_user, new_user) ORDER BY id FOR UPDATE;
    SELECT id INTO old_ctx FROM user_contexts WHERE user_id = old_user;
    SELECT id INTO new_ctx FROM user_contexts WHERE user_id = new_user;
    IF new_ctx IS NULL THEN
        RAISE EXCEPTION 'target user % has no context', new_user;
    END IF;

    DELETE FROM timeline_revisions WHERE user_id=old_user;
    DELETE FROM pulse_revisions WHERE user_id=old_user;
    DELETE FROM pulse_daily_aggregates WHERE user_id IN (old_user,new_user);
    DELETE FROM pulse_cache WHERE user_id IN (old_user,new_user);
    DELETE FROM pulse_dismissals WHERE user_id=old_user;
    PERFORM set_config('vox.merge_old_user',old_user::text,true);
    PERFORM set_config('vox.merge_new_user',new_user::text,true);
    SET CONSTRAINTS ALL DEFERRED;

    FOR fk IN
        SELECT c.conrelid::regclass::text AS tbl,
               CASE
                   WHEN c.confrelid = 'users'::regclass THEN 'users'
                   WHEN c.confrelid = 'user_contexts'::regclass THEN 'ctx'
                   ELSE 'other'
               END AS kind,
               array_agg(a.attname::text ORDER BY k.ord) AS child_cols,
               array_agg(pa.attname::text ORDER BY k.ord) AS parent_cols
        FROM pg_constraint c
        CROSS JOIN LATERAL unnest(c.conkey, c.confkey) WITH ORDINALITY AS k(child_att, parent_att, ord)
        JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.child_att
        JOIN pg_attribute pa ON pa.attrelid = c.confrelid AND pa.attnum = k.parent_att
        WHERE c.contype = 'f'
          AND c.connamespace = 'public'::regnamespace
          AND c.conrelid NOT IN ('users'::regclass, 'user_contexts'::regclass)
        GROUP BY c.oid, c.conrelid, c.confrelid
        HAVING c.confrelid IN ('users'::regclass, 'user_contexts'::regclass)
            OR 'user_id' = ANY (array_agg(pa.attname::text))
        ORDER BY
            CASE
                WHEN c.confrelid = 'user_contexts'::regclass
                     AND 'user_id' = ANY (array_agg(pa.attname::text)) THEN 0
                WHEN c.confrelid = 'user_contexts'::regclass THEN 1
                WHEN c.confrelid = 'users'::regclass THEN 2
                ELSE 3
            END,
            c.conrelid::regclass::text
    LOOP
        set_parts := ARRAY[]::TEXT[];
        where_parts := ARRAY[]::TEXT[];
        FOR idx IN 1 .. cardinality(fk.child_cols) LOOP
            child_col := fk.child_cols[idx];
            parent_col := fk.parent_cols[idx];
            IF fk.kind = 'users' THEN
                set_parts := set_parts || format('%I = %L::uuid', child_col, new_user);
                where_parts := where_parts || format('%I = %L::uuid', child_col, old_user);
            ELSIF fk.kind = 'ctx' AND parent_col = 'id' THEN
                set_parts := set_parts || format('%I = %L::uuid', child_col, new_ctx);
                where_parts := where_parts || format('%I = %L::uuid', child_col, old_ctx);
            ELSIF parent_col = 'user_id' THEN
                set_parts := set_parts || format('%I = %L::uuid', child_col, new_user);
                where_parts := where_parts || format('%I = %L::uuid', child_col, old_user);
            END IF;
        END LOOP;
        CONTINUE WHEN cardinality(set_parts) = 0;
        set_clause := array_to_string(set_parts, ', ');
        where_clause := array_to_string(where_parts, ' AND ');

        EXECUTE format('UPDATE %s SET %s WHERE %s', fk.tbl, set_clause, where_clause);
    END LOOP;

    SET CONSTRAINTS ALL IMMEDIATE;

    DELETE FROM user_contexts WHERE user_id = old_user;
    DELETE FROM users WHERE id = old_user;
    PERFORM set_config('vox.merge_old_user','',true);
    PERFORM set_config('vox.merge_new_user','',true);
END
$_$;

CREATE FUNCTION public.record_run_status_event() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $$
BEGIN
    IF NEW.user_context_id IS NOT NULL AND
       (TG_OP = 'INSERT' OR NEW.state IS DISTINCT FROM OLD.state OR
        NEW.wait_reason IS DISTINCT FROM OLD.wait_reason) THEN
        PERFORM pg_advisory_xact_lock(73125, hashtext(NEW.user_context_id::text));
        INSERT INTO status_events (user_context_id, aggregate_type, aggregate_id,
            event_type, state, deduplication_key, occurred_at)
        VALUES (NEW.user_context_id, 'run', NEW.id, 'run.state_changed',
            NEW.state, gen_random_uuid()::text, now());
        INSERT INTO audit_events (user_id, user_context_id, actor, event_type, affected_ids, details)
        VALUES (NEW.user_id, NEW.user_context_id, 'core', 'run.state_changed',
            jsonb_build_array(jsonb_build_object('type','task_run','id',NEW.id)),
            jsonb_build_object('state',NEW.state));
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION public.record_span_status_event() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $$
BEGIN
    IF NEW.user_context_id IS NOT NULL AND NEW.execution_type IS NOT NULL AND
       (TG_OP = 'INSERT' OR NEW.status IS DISTINCT FROM OLD.status) THEN
        PERFORM pg_advisory_xact_lock(73125, hashtext(NEW.user_context_id::text));
        INSERT INTO status_events (user_context_id, aggregate_type, aggregate_id,
            event_type, state, deduplication_key, occurred_at)
        VALUES (NEW.user_context_id, 'span', NEW.id, 'span.state_changed',
            NEW.status, gen_random_uuid()::text, now());
        INSERT INTO audit_events (user_id, user_context_id, actor, event_type, affected_ids, details)
        VALUES (NEW.user_id, NEW.user_context_id, 'core', 'span.state_changed',
            jsonb_build_array(jsonb_build_object('type','span','id',NEW.id)),
            jsonb_build_object('state',NEW.status));
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION public.record_task_status_event() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $$
BEGIN
    IF NEW.user_context_id IS NOT NULL AND
       (TG_OP = 'INSERT' OR NEW.status IS DISTINCT FROM OLD.status) THEN
        PERFORM pg_advisory_xact_lock(73125, hashtext(NEW.user_context_id::text));
        INSERT INTO status_events (user_context_id, aggregate_type, aggregate_id,
            event_type, state, deduplication_key, occurred_at)
        VALUES (NEW.user_context_id, 'task', NEW.id, 'task.state_changed',
            NEW.status, gen_random_uuid()::text, now());
        INSERT INTO audit_events (user_id, user_context_id, actor, event_type, affected_ids, details)
        VALUES (NEW.user_id, NEW.user_context_id, 'core', 'task.state_changed',
            jsonb_build_array(jsonb_build_object('type','task','id',NEW.id)),
            jsonb_build_object('state',NEW.status));
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION public.resource_context_compatibility() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $$
BEGIN
    IF NEW.user_id IS NULL THEN
        IF TG_OP = 'UPDATE' AND TG_TABLE_NAME = 'audit_events'
           AND OLD.user_id IS NOT NULL THEN
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
END $$;

CREATE FUNCTION public.span_range(s timestamp with time zone, e timestamp with time zone) RETURNS tstzrange
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$ SELECT tstzrange(s, GREATEST(COALESCE(e, s), s), '[]') $$;

CREATE FUNCTION public.sync_reminder_span() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'public', 'pg_temp'
    AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO spans (user_id, user_context_id, title, notes, category, source, source_ref, status, start_at)
        SELECT uc.user_id, uc.id, NEW.title, NEW.message, 'reminder', 'reminder', NEW.id::text,
            'planned', NEW.next_trigger_at
        FROM user_contexts uc WHERE uc.id = NEW.user_context_id
        RETURNING id INTO NEW.span_id;
    ELSIF NEW.span_id IS NOT NULL THEN
        UPDATE spans SET
            title = NEW.title,
            notes = NEW.message,
            status = CASE NEW.status
                WHEN 'scheduled' THEN 'planned'
                WHEN 'delivered_to_channel' THEN 'done'
                WHEN 'cancelled' THEN 'cancelled'
                ELSE 'failed' END,
            start_at = CASE WHEN NEW.status = 'scheduled' THEN NEW.next_trigger_at
                ELSE COALESCE(NEW.delivered_at, NEW.last_attempt_at, start_at) END,
            updated_at = now()
        WHERE id = NEW.span_id;
    END IF;
    RETURN NEW;
END;
$$;

SET default_tablespace = '';

SET default_table_access_method = heap;

CREATE TABLE public.action_approvals (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    proposal_id uuid NOT NULL,
    user_id uuid NOT NULL,
    approved_details_hash text NOT NULL,
    session_evidence jsonb DEFAULT '{}'::jsonb NOT NULL,
    approved_at timestamp with time zone DEFAULT now() NOT NULL,
    consumed_execution_id uuid,
    user_context_id uuid NOT NULL,
    CONSTRAINT action_approvals_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL)))
);

CREATE TABLE public.action_proposals (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    span_id uuid,
    job_id uuid,
    actor_key text NOT NULL,
    connection_id uuid,
    capability text NOT NULL,
    details jsonb NOT NULL,
    details_hash text NOT NULL,
    state text DEFAULT 'proposed'::text NOT NULL,
    expires_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    user_context_id uuid NOT NULL,
    CONSTRAINT action_proposals_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT action_proposals_details_check CHECK ((jsonb_typeof(details) = 'object'::text)),
    CONSTRAINT action_proposals_state_check CHECK ((state = ANY (ARRAY['proposed'::text, 'approved'::text, 'rejected'::text, 'expired'::text])))
);

CREATE TABLE public.agent_definitions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    deployment_id uuid NOT NULL,
    external_key text NOT NULL,
    purpose text NOT NULL,
    requested_capability_categories text[] DEFAULT '{}'::text[] NOT NULL,
    state text DEFAULT 'enabled'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    owner_user_context_id uuid,
    template_id uuid,
    display_name text DEFAULT 'Assistant'::text NOT NULL,
    is_default boolean DEFAULT false NOT NULL,
    instruction_version integer DEFAULT 1 NOT NULL,
    CONSTRAINT agent_default_owned CHECK (((NOT is_default) OR ((owner_user_context_id IS NOT NULL) AND (state = 'enabled'::text)))),
    CONSTRAINT agent_definitions_capability_categories_size CHECK ((cardinality(requested_capability_categories) <= 64)),
    CONSTRAINT agent_definitions_key_not_empty CHECK (((length(btrim(external_key)) >= 1) AND (length(btrim(external_key)) <= 255))),
    CONSTRAINT agent_definitions_purpose_not_empty CHECK (((length(btrim(purpose)) >= 1) AND (length(btrim(purpose)) <= 2048))),
    CONSTRAINT agent_definitions_state_valid CHECK ((state = ANY (ARRAY['enabled'::text, 'disabled'::text]))),
    CONSTRAINT agent_name_valid CHECK (((length(btrim(display_name)) >= 1) AND (length(btrim(display_name)) <= 100)))
);

CREATE TABLE public.agent_delegation_permissions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_context_id uuid NOT NULL,
    requester_agent_id uuid NOT NULL,
    specialist_agent_id uuid NOT NULL,
    shared_preferences jsonb DEFAULT '[]'::jsonb NOT NULL,
    scope jsonb NOT NULL,
    parent_run_id uuid,
    mode text NOT NULL,
    state text DEFAULT 'enabled'::text NOT NULL,
    used boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    revoked_at timestamp with time zone,
    CONSTRAINT agent_delegation_permissions_check CHECK ((requester_agent_id <> specialist_agent_id)),
    CONSTRAINT agent_delegation_permissions_check1 CHECK (((mode = 'once'::text) = (parent_run_id IS NOT NULL))),
    CONSTRAINT agent_delegation_permissions_mode_check CHECK ((mode = ANY (ARRAY['once'::text, 'remembered'::text]))),
    CONSTRAINT agent_delegation_permissions_scope_check CHECK ((jsonb_typeof(scope) = 'object'::text)),
    CONSTRAINT agent_delegation_permissions_scope_check1 CHECK (((jsonb_typeof((scope -> 'capabilities'::text)) = 'array'::text) AND ((jsonb_array_length((scope -> 'capabilities'::text)) >= 0) AND (jsonb_array_length((scope -> 'capabilities'::text)) <= 32)))),
    CONSTRAINT agent_delegation_permissions_scope_check2 CHECK (((scope -> 'skills'::text) = '[]'::jsonb)),
    CONSTRAINT agent_delegation_permissions_shared_preferences_check CHECK (((jsonb_typeof(shared_preferences) = 'array'::text) AND (jsonb_array_length(shared_preferences) <= 8))),
    CONSTRAINT agent_delegation_permissions_state_check CHECK ((state = ANY (ARRAY['enabled'::text, 'revoked'::text])))
);

CREATE TABLE public.agent_instruction_versions (
    agent_id uuid NOT NULL,
    version integer NOT NULL,
    instructions text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT agent_instruction_versions_instructions_check CHECK (((length(btrim(instructions)) >= 1) AND (length(btrim(instructions)) <= 2048))),
    CONSTRAINT agent_instruction_versions_version_check CHECK ((version > 0))
);

CREATE TABLE public.agent_memories (
    user_context_id uuid NOT NULL,
    agent_id uuid NOT NULL,
    facts jsonb DEFAULT '{}'::jsonb NOT NULL,
    version bigint DEFAULT 1 NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    retention_enabled boolean DEFAULT true NOT NULL,
    cleared_at timestamp with time zone DEFAULT '1970-01-01 00:00:00+00'::timestamp with time zone NOT NULL,
    CONSTRAINT agent_memories_facts_check CHECK (((jsonb_typeof(facts) = 'object'::text) AND (octet_length((facts)::text) <= 8192))),
    CONSTRAINT agent_memories_version_check CHECK ((version > 0))
);

CREATE TABLE public.agent_model_configurations (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    agent_definition_id uuid NOT NULL,
    version integer NOT NULL,
    model_adapter text NOT NULL,
    model text NOT NULL,
    configuration jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT agent_model_configurations_adapter_not_empty CHECK (((length(btrim(model_adapter)) >= 1) AND (length(btrim(model_adapter)) <= 255))),
    CONSTRAINT agent_model_configurations_configuration_object CHECK ((jsonb_typeof(configuration) = 'object'::text)),
    CONSTRAINT agent_model_configurations_model_not_empty CHECK (((length(btrim(model)) >= 1) AND (length(btrim(model)) <= 255)))
);

CREATE TABLE public.assigned_task_runs (
    job_id uuid NOT NULL,
    user_context_id uuid NOT NULL,
    agent_id uuid NOT NULL,
    instruction_version integer NOT NULL,
    model_configuration_id uuid NOT NULL,
    model_version integer NOT NULL,
    actor_snapshot jsonb NOT NULL,
    authority jsonb NOT NULL,
    task_instruction text NOT NULL,
    parent_run_id uuid,
    delegation_permission_id uuid,
    deadline_at timestamp with time zone NOT NULL,
    max_tool_calls integer DEFAULT 24 NOT NULL,
    tool_calls integer DEFAULT 0 NOT NULL,
    pending_proposal_id uuid,
    result jsonb DEFAULT '{}'::jsonb NOT NULL,
    result_received_at timestamp with time zone,
    CONSTRAINT assigned_task_runs_actor_snapshot_check CHECK ((jsonb_typeof(actor_snapshot) = 'object'::text)),
    CONSTRAINT assigned_task_runs_authority_check CHECK ((jsonb_typeof(authority) = 'object'::text)),
    CONSTRAINT assigned_task_runs_authority_check1 CHECK (((jsonb_typeof((authority -> 'capabilities'::text)) = 'array'::text) AND (jsonb_array_length((authority -> 'capabilities'::text)) <= 256))),
    CONSTRAINT assigned_task_runs_authority_check2 CHECK (((jsonb_typeof((authority -> 'skills'::text)) = 'array'::text) AND (jsonb_array_length((authority -> 'skills'::text)) <= 128))),
    CONSTRAINT assigned_task_runs_check CHECK ((((actor_snapshot -> 'definition'::text) ->> 'id'::text) = (agent_id)::text)),
    CONSTRAINT assigned_task_runs_check1 CHECK (((((actor_snapshot -> 'definition'::text) ->> 'instruction_version'::text))::integer = instruction_version)),
    CONSTRAINT assigned_task_runs_check2 CHECK ((((actor_snapshot -> 'model_configuration'::text) ->> 'id'::text) = (model_configuration_id)::text)),
    CONSTRAINT assigned_task_runs_check3 CHECK (((((actor_snapshot -> 'model_configuration'::text) ->> 'version'::text))::integer = model_version)),
    CONSTRAINT assigned_task_runs_check4 CHECK (((parent_run_id IS NULL) OR (parent_run_id <> job_id))),
    CONSTRAINT assigned_task_runs_check5 CHECK (((delegation_permission_id IS NULL) OR (parent_run_id IS NOT NULL))),
    CONSTRAINT assigned_task_runs_max_tool_calls_check CHECK (((max_tool_calls >= 1) AND (max_tool_calls <= 64))),
    CONSTRAINT assigned_task_runs_result_check CHECK ((jsonb_typeof(result) = 'object'::text)),
    CONSTRAINT assigned_task_runs_task_instruction_check CHECK (((octet_length(task_instruction) >= 1) AND (octet_length(task_instruction) <= 20000))),
    CONSTRAINT assigned_task_runs_tool_calls_check CHECK ((tool_calls >= 0))
);

CREATE TABLE public.audit_events (
    cursor_id bigint NOT NULL,
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid,
    actor text NOT NULL,
    event_type text NOT NULL,
    affected_ids jsonb DEFAULT '[]'::jsonb NOT NULL,
    occurred_at timestamp with time zone DEFAULT now() NOT NULL,
    details jsonb DEFAULT '{}'::jsonb NOT NULL,
    schema_version integer DEFAULT 1 NOT NULL,
    user_context_id uuid,
    CONSTRAINT audit_events_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL)))
);

CREATE SEQUENCE public.audit_events_cursor_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.audit_events_cursor_id_seq OWNED BY public.audit_events.cursor_id;

CREATE TABLE public.auth_identities (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    issuer text NOT NULL,
    subject text NOT NULL,
    profile_metadata jsonb DEFAULT '{}'::jsonb NOT NULL,
    verified_at timestamp with time zone DEFAULT now() NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    user_context_id uuid,
    CONSTRAINT auth_identities_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT auth_identities_issuer_not_empty CHECK ((length(btrim(issuer)) > 0)),
    CONSTRAINT auth_identities_subject_not_empty CHECK ((length(btrim(subject)) > 0))
);

CREATE TABLE public.auth_sessions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    auth_identity_id uuid,
    device_id uuid,
    token_hash text NOT NULL,
    family_id uuid NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    revoked_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    user_context_id uuid,
    scope text DEFAULT 'full'::text NOT NULL,
    CONSTRAINT auth_sessions_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT auth_sessions_scope_check CHECK ((scope = ANY (ARRAY['full'::text, 'web'::text]))),
    CONSTRAINT auth_sessions_token_hash_not_empty CHECK ((length(btrim(token_hash)) > 0))
);

CREATE TABLE public.channel_identities (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    channel text NOT NULL,
    provider_scope text DEFAULT 'global'::text NOT NULL,
    normalized_external_id text NOT NULL,
    verified_at timestamp with time zone,
    revoked_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    user_context_id uuid,
    otp_verified_at timestamp with time zone,
    CONSTRAINT channel_identities_channel_not_empty CHECK ((length(btrim(channel)) > 0)),
    CONSTRAINT channel_identities_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT channel_identities_external_not_empty CHECK ((length(btrim(normalized_external_id)) > 0))
);

CREATE TABLE public.devices (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    device_identifier text NOT NULL,
    platform text NOT NULL,
    label text DEFAULT ''::text NOT NULL,
    public_key text,
    capabilities jsonb DEFAULT '{}'::jsonb NOT NULL,
    execution_consent boolean DEFAULT false NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    last_seen_at timestamp with time zone DEFAULT now() NOT NULL,
    revoked_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    user_context_id uuid,
    CONSTRAINT devices_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT devices_device_identifier_check CHECK ((length(btrim(device_identifier)) > 0)),
    CONSTRAINT devices_platform_check CHECK ((length(btrim(platform)) > 0))
);

CREATE VIEW public.client_devices WITH (security_invoker='true') AS
 SELECT id,
    user_id,
    device_identifier,
    platform,
    label AS device_name,
    is_active,
    last_seen_at,
    capabilities AS telemetry,
    created_at,
    updated_at
   FROM public.devices;

CREATE TABLE public.collection_spans (
    collection_id uuid NOT NULL,
    span_id uuid NOT NULL,
    user_id uuid NOT NULL,
    added_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.collections (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    name text NOT NULL,
    description text DEFAULT ''::text NOT NULL,
    kind text DEFAULT 'custom'::text NOT NULL,
    status text DEFAULT 'active'::text NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb NOT NULL,
    version integer DEFAULT 1 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    user_context_id uuid,
    starts_at timestamp with time zone,
    ends_at timestamp with time zone,
    CONSTRAINT collections_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT collections_kind_check CHECK ((kind = ANY (ARRAY['trip'::text, 'event'::text, 'course'::text, 'area'::text, 'custom'::text]))),
    CONSTRAINT collections_metadata_is_object CHECK ((jsonb_typeof(metadata) = 'object'::text)),
    CONSTRAINT collections_name_check CHECK ((length(btrim(name)) > 0)),
    CONSTRAINT collections_status_check CHECK ((status = ANY (ARRAY['active'::text, 'paused'::text, 'completed'::text, 'archived'::text]))),
    CONSTRAINT collections_window_order CHECK (((starts_at IS NULL) OR (ends_at IS NULL) OR (ends_at >= starts_at)))
);

CREATE TABLE public.conversations (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    channel_identity_id uuid,
    external_conversation_id text NOT NULL,
    channel text NOT NULL,
    state text DEFAULT 'active'::text NOT NULL,
    latest_summary jsonb DEFAULT '{}'::jsonb NOT NULL,
    summary_version integer DEFAULT 0 NOT NULL,
    summary_through_sequence bigint DEFAULT 0 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    user_context_id uuid,
    agent_external_key text NOT NULL,
    CONSTRAINT conversations_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT conversations_external_not_empty CHECK ((length(btrim(external_conversation_id)) > 0)),
    CONSTRAINT conversations_state_check CHECK ((state = ANY (ARRAY['active'::text, 'completed'::text, 'archived'::text])))
);

CREATE TABLE public.data_schemas (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid,
    owner_scope text GENERATED ALWAYS AS (COALESCE((user_id)::text, 'global'::text)) STORED,
    namespace text NOT NULL,
    name text NOT NULL,
    version integer DEFAULT 1 NOT NULL,
    description text DEFAULT ''::text NOT NULL,
    json_schema jsonb NOT NULL,
    embedding extensions.vector(768),
    state text DEFAULT 'active'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    user_context_id uuid,
    color_token integer DEFAULT 0 NOT NULL,
    icon_token integer DEFAULT 0 NOT NULL,
    CONSTRAINT data_schemas_color_token_check CHECK (((color_token >= 0) AND (color_token <= 23))),
    CONSTRAINT data_schemas_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT data_schemas_icon_token_check CHECK (((icon_token >= 0) AND (icon_token <= 23))),
    CONSTRAINT data_schemas_json_schema_check CHECK ((jsonb_typeof(json_schema) = 'object'::text)),
    CONSTRAINT data_schemas_name_check CHECK ((length(btrim(name)) > 0)),
    CONSTRAINT data_schemas_namespace_check CHECK ((length(btrim(namespace)) > 0)),
    CONSTRAINT data_schemas_state_check CHECK ((state = ANY (ARRAY['active'::text, 'deprecated'::text]))),
    CONSTRAINT data_schemas_version_check CHECK ((version > 0))
);

CREATE TABLE public.data_source_consents (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    data_source text NOT NULL,
    granted_at timestamp with time zone,
    revoked_at timestamp with time zone,
    retention_days integer DEFAULT 90 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    synced_until timestamp with time zone,
    CONSTRAINT data_source_consents_data_source_check CHECK ((data_source = 'sms'::text)),
    CONSTRAINT data_source_consents_retention_days_check CHECK ((retention_days > 0))
);

CREATE TABLE public.deployment_agent_selections (
    deployment_id uuid NOT NULL,
    agent_definition_id uuid NOT NULL,
    model_configuration_id uuid NOT NULL,
    selected_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.event_agent_decisions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    source_kind text NOT NULL,
    event_type text NOT NULL,
    outcome text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.inbound_events (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid,
    source_kind text NOT NULL,
    source_id text NOT NULL,
    external_event_id text NOT NULL,
    payload_hash text NOT NULL,
    event_type text NOT NULL,
    payload_version integer DEFAULT 1 NOT NULL,
    occurred_at timestamp with time zone NOT NULL,
    received_at timestamp with time zone DEFAULT now() NOT NULL,
    payload jsonb NOT NULL,
    execution_id uuid,
    processed_at timestamp with time zone,
    processing_error text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    user_context_id uuid,
    batch_id uuid,
    failed_at timestamp with time zone,
    retryable boolean DEFAULT false NOT NULL,
    requeue_count integer DEFAULT 0 NOT NULL,
    CONSTRAINT inbound_events_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT inbound_events_event_type_check CHECK ((length(btrim(event_type)) > 0)),
    CONSTRAINT inbound_events_execution_has_user CHECK (((execution_id IS NULL) OR (user_id IS NOT NULL)))
);

CREATE VIEW public.events WITH (security_invoker='true') AS
 SELECT id,
    user_id,
    external_event_id AS idempotency_key,
    event_type,
    occurred_at,
    payload,
    created_at,
    processed_at
   FROM public.inbound_events;

CREATE TABLE public.execution_attempts (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    execution_id uuid NOT NULL,
    attempt_number integer NOT NULL,
    request_hash text NOT NULL,
    state text NOT NULL,
    provider_reference text,
    error_details text,
    policy_decision_snapshot jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    CONSTRAINT execution_attempts_attempt_number_check CHECK ((attempt_number > 0)),
    CONSTRAINT execution_attempts_state_check CHECK ((state = ANY (ARRAY['started'::text, 'succeeded'::text, 'failed'::text, 'timeout'::text])))
);

CREATE TABLE public.executions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    proposal_id uuid NOT NULL,
    approval_id uuid NOT NULL,
    connection_id uuid,
    idempotency_key text NOT NULL,
    provider_snapshot jsonb DEFAULT '{}'::jsonb NOT NULL,
    state text DEFAULT 'pending'::text NOT NULL,
    provider_reference text,
    confirmation_evidence jsonb DEFAULT '{}'::jsonb NOT NULL,
    policy_snapshot jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    user_context_id uuid NOT NULL,
    CONSTRAINT executions_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT executions_state_check CHECK ((state = ANY (ARRAY['pending'::text, 'in_progress'::text, 'succeeded'::text, 'failed'::text, 'reconciling'::text])))
);

CREATE TABLE public.federated_identity_nonces (
    adapter_id uuid NOT NULL,
    nonce uuid NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.host_app_assertion_nonces (
    credential_id uuid NOT NULL,
    nonce uuid NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.host_app_credentials (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    deployment_id uuid NOT NULL,
    host_app_id uuid NOT NULL,
    secret_hash bytea NOT NULL,
    state text DEFAULT 'active'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    revoked_at timestamp with time zone,
    CONSTRAINT host_app_credentials_revocation_state CHECK ((((state = 'active'::text) AND (revoked_at IS NULL)) OR ((state = 'revoked'::text) AND (revoked_at IS NOT NULL)))),
    CONSTRAINT host_app_credentials_secret_hash_length CHECK ((octet_length(secret_hash) = 32)),
    CONSTRAINT host_app_credentials_state_valid CHECK ((state = ANY (ARRAY['active'::text, 'revoked'::text])))
);

CREATE TABLE public.host_apps (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    deployment_id uuid NOT NULL,
    external_key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    allowed_origins text[] DEFAULT '{}'::text[] NOT NULL,
    CONSTRAINT host_apps_external_key_not_empty CHECK (((length(btrim(external_key)) >= 1) AND (length(btrim(external_key)) <= 255)))
);

CREATE TABLE public.host_organizations (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    deployment_id uuid NOT NULL,
    host_app_id uuid NOT NULL,
    external_key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT host_organizations_external_key_not_empty CHECK (((length(btrim(external_key)) >= 1) AND (length(btrim(external_key)) <= 255)))
);

CREATE TABLE public.identity_adapters (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    deployment_id uuid NOT NULL,
    external_key text NOT NULL,
    kind text NOT NULL,
    configuration jsonb NOT NULL,
    state text DEFAULT 'enabled'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    disabled_at timestamp with time zone,
    CONSTRAINT identity_adapters_configuration_object CHECK ((jsonb_typeof(configuration) = 'object'::text)),
    CONSTRAINT identity_adapters_disabled_state CHECK ((((state = 'enabled'::text) AND (disabled_at IS NULL)) OR ((state = 'disabled'::text) AND (disabled_at IS NOT NULL)))),
    CONSTRAINT identity_adapters_external_key_not_empty CHECK (((length(btrim(external_key)) >= 1) AND (length(btrim(external_key)) <= 255))),
    CONSTRAINT identity_adapters_kind_valid CHECK ((kind = ANY (ARRAY['federated_ed25519'::text, 'passwordless_recovery'::text]))),
    CONSTRAINT identity_adapters_state_valid CHECK ((state = ANY (ARRAY['enabled'::text, 'disabled'::text])))
);

CREATE TABLE public.identity_authentication_sessions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    login_identity_id uuid NOT NULL,
    token_hash bytea NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    consumed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT identity_authentication_sessions_token_hash_length CHECK ((octet_length(token_hash) = 32))
);

CREATE TABLE public.identity_link_events (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    link_id uuid NOT NULL,
    event_kind text NOT NULL,
    occurred_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT identity_link_events_kind_valid CHECK ((event_kind = ANY (ARRAY['linked'::text, 'unlinked'::text])))
);

CREATE TABLE public.identity_links (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    left_login_identity_id uuid NOT NULL,
    right_login_identity_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    removed_at timestamp with time zone,
    CONSTRAINT identity_links_distinct_identities CHECK ((left_login_identity_id <> right_login_identity_id)),
    CONSTRAINT identity_links_ordered_identities CHECK ((left_login_identity_id < right_login_identity_id))
);

CREATE TABLE public.integration_capability_declarations (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    integration_id uuid NOT NULL,
    external_key text NOT NULL,
    effect text NOT NULL,
    access_needs text[] DEFAULT '{}'::text[] NOT NULL,
    data_recipients text[] DEFAULT '{}'::text[] NOT NULL,
    regions text[] DEFAULT '{}'::text[] NOT NULL,
    failure_modes text[] DEFAULT '{}'::text[] NOT NULL,
    optional_guarantees jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT integration_capabilities_effect_valid CHECK ((effect = ANY (ARRAY['read'::text, 'write'::text, 'mixed'::text]))),
    CONSTRAINT integration_capabilities_guarantees_object CHECK ((jsonb_typeof(optional_guarantees) = 'object'::text)),
    CONSTRAINT integration_capabilities_key_not_empty CHECK (((length(btrim(external_key)) >= 1) AND (length(btrim(external_key)) <= 255)))
);

CREATE TABLE public.integration_codes (
    code_hash text NOT NULL,
    grant_id uuid NOT NULL,
    redirect_uri text NOT NULL,
    code_challenge text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    consumed_at timestamp with time zone
);

CREATE TABLE public.integration_declaration_versions (
    integration_id uuid NOT NULL,
    version integer NOT NULL,
    declaration jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT integration_declaration_versions_declaration_check CHECK ((jsonb_typeof(declaration) = 'object'::text)),
    CONSTRAINT integration_declaration_versions_version_check CHECK ((version > 0))
);

CREATE TABLE public.integration_grants (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    client_id text NOT NULL,
    collection_ids uuid[] NOT NULL,
    chart_ids uuid[] NOT NULL,
    span_from timestamp with time zone,
    span_to timestamp with time zone,
    allow_create_plans boolean DEFAULT false NOT NULL,
    expires_at timestamp with time zone DEFAULT (now() + '90 days'::interval) NOT NULL,
    revoked_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT integration_grants_check CHECK ((((span_from IS NULL) AND (span_to IS NULL)) OR ((span_from IS NOT NULL) AND (span_to IS NOT NULL) AND (span_to > span_from) AND (span_to <= (span_from + '365 days'::interval)))))
);

CREATE TABLE public.integration_plan_requests (
    user_id uuid NOT NULL,
    client_id text NOT NULL,
    request_id uuid NOT NULL,
    payload_hash text NOT NULL,
    span_id uuid NOT NULL
);

CREATE TABLE public.integration_tokens (
    access_hash text NOT NULL,
    refresh_hash text NOT NULL,
    grant_id uuid NOT NULL,
    access_expires_at timestamp with time zone NOT NULL,
    refresh_expires_at timestamp with time zone NOT NULL,
    rotated_at timestamp with time zone
);

CREATE TABLE public.jobs (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid,
    kind text NOT NULL,
    payload_reference_id uuid,
    span_id uuid,
    schedule_id uuid,
    source_event_id uuid,
    occurrence_at timestamp with time zone,
    dedupe_key text,
    input_reference text,
    checkpoint jsonb DEFAULT '{}'::jsonb NOT NULL,
    state text DEFAULT 'pending'::text NOT NULL,
    wait_reason text,
    available_at timestamp with time zone DEFAULT now() NOT NULL,
    deadline_at timestamp with time zone,
    max_attempts integer DEFAULT 3 NOT NULL,
    attempt_count integer DEFAULT 0 NOT NULL,
    lease_generation bigint DEFAULT 0 NOT NULL,
    lease_owner text,
    lease_expires_at timestamp with time zone,
    execution_policy jsonb DEFAULT '{}'::jsonb NOT NULL,
    last_error_code text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    user_context_id uuid,
    priority smallint DEFAULT 0 NOT NULL,
    action_idempotency_keys jsonb DEFAULT '[]'::jsonb NOT NULL,
    CONSTRAINT jobs_attempt_count_check CHECK ((attempt_count >= 0)),
    CONSTRAINT jobs_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT jobs_kind_check CHECK ((kind = ANY (ARRAY['process_event'::text, 'process_event_batch'::text, 'run_schedule'::text, 'dispatch_action'::text, 'summarize_conversation'::text, 'evaluate_span'::text, 'execute_span'::text, 'run_space'::text, 'process_attachment'::text]))),
    CONSTRAINT jobs_max_attempts_check CHECK ((max_attempts > 0)),
    CONSTRAINT jobs_owned_refs_have_user CHECK ((((span_id IS NULL) AND (schedule_id IS NULL) AND (source_event_id IS NULL)) OR (user_id IS NOT NULL))),
    CONSTRAINT jobs_state_check CHECK ((state = ANY (ARRAY['pending'::text, 'running'::text, 'completed'::text, 'failed'::text, 'cancelled'::text])))
);

CREATE TABLE public.login_identities (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_context_id uuid NOT NULL,
    adapter_id uuid NOT NULL,
    subject_hash bytea NOT NULL,
    verified_at timestamp with time zone DEFAULT now() NOT NULL,
    last_authenticated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT login_identities_subject_hash_length CHECK ((octet_length(subject_hash) = 32))
);

CREATE TABLE public.messages (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    conversation_id uuid NOT NULL,
    sequence_number bigint NOT NULL,
    role text NOT NULL,
    text text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT messages_role_check CHECK ((role = ANY (ARRAY['user'::text, 'assistant'::text, 'system'::text]))),
    CONSTRAINT messages_text_check CHECK ((length(btrim(text)) > 0))
);

CREATE TABLE public.operational_quota_reservations (
    attempt_id uuid NOT NULL,
    quota_id uuid NOT NULL,
    approval_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.operational_quotas (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_context_id uuid NOT NULL,
    provider_external_key text NOT NULL,
    model_identifier text NOT NULL,
    account_hash bytea NOT NULL,
    connection_id uuid NOT NULL,
    max_attempts integer NOT NULL,
    reserved_attempts integer DEFAULT 0 NOT NULL,
    version integer DEFAULT 1 NOT NULL,
    CONSTRAINT operational_quotas_max_attempts_check CHECK ((max_attempts > 0)),
    CONSTRAINT operational_quotas_reserved_attempts_check CHECK ((reserved_attempts >= 0))
);

CREATE TABLE public.passwordless_recovery_challenges (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    adapter_id uuid NOT NULL,
    user_context_id uuid NOT NULL,
    recovery_handle_hash bytea NOT NULL,
    code_hash bytea NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    consumed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT passwordless_recovery_code_hash_length CHECK ((octet_length(code_hash) = 32)),
    CONSTRAINT passwordless_recovery_handle_hash_length CHECK ((octet_length(recovery_handle_hash) = 32))
);

CREATE TABLE public.pending_phone_links (
    user_id uuid NOT NULL,
    normalized_phone text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT pending_phone_links_normalized_phone_check CHECK ((length(btrim(normalized_phone)) > 0))
);

CREATE TABLE public.phone_verifications (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    normalized_phone text NOT NULL,
    code_hash text NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    consumed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.platform_deployments (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    external_key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT platform_deployments_external_key_not_empty CHECK (((length(btrim(external_key)) >= 1) AND (length(btrim(external_key)) <= 255)))
);

CREATE TABLE public.reminder_deliveries (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    reminder_id uuid NOT NULL,
    scheduled_for timestamp with time zone NOT NULL,
    attempted_at timestamp with time zone DEFAULT now() NOT NULL,
    status text NOT NULL,
    channel text NOT NULL,
    destination text NOT NULL,
    provider_receipt_id text,
    failure_reason text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT reminder_deliveries_status_check CHECK ((status = ANY (ARRAY['delivered_to_channel'::text, 'failed'::text, 'unknown'::text, 'missed'::text])))
);

CREATE TABLE public.reminders (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_context_id uuid NOT NULL,
    title text NOT NULL,
    message text NOT NULL,
    channel text NOT NULL,
    destination text NOT NULL,
    timezone text DEFAULT 'UTC'::text NOT NULL,
    schedule_kind text NOT NULL,
    run_at timestamp with time zone,
    interval_seconds bigint,
    recurrence_expression text,
    next_trigger_at timestamp with time zone,
    status text DEFAULT 'scheduled'::text NOT NULL,
    retry_count integer DEFAULT 0 NOT NULL,
    max_retries integer DEFAULT 3 NOT NULL,
    last_attempt_at timestamp with time zone,
    delivered_at timestamp with time zone,
    failure_reason text,
    metadata jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    span_id uuid,
    CONSTRAINT reminders_channel_check CHECK ((length(btrim(channel)) > 0)),
    CONSTRAINT reminders_destination_check CHECK ((length(btrim(destination)) > 0)),
    CONSTRAINT reminders_message_check CHECK ((length(btrim(message)) > 0)),
    CONSTRAINT reminders_schedule_kind_check CHECK ((schedule_kind = ANY (ARRAY['one_time'::text, 'interval'::text, 'calendar_recurrence'::text]))),
    CONSTRAINT reminders_schedule_shape CHECK ((((schedule_kind = 'one_time'::text) AND (run_at IS NOT NULL) AND (interval_seconds IS NULL) AND (recurrence_expression IS NULL)) OR ((schedule_kind = 'interval'::text) AND (interval_seconds IS NOT NULL) AND (interval_seconds > 0) AND (recurrence_expression IS NULL)) OR ((schedule_kind = 'calendar_recurrence'::text) AND (recurrence_expression IS NOT NULL) AND (interval_seconds IS NULL)))),
    CONSTRAINT reminders_status_check CHECK ((status = ANY (ARRAY['scheduled'::text, 'delivered_to_channel'::text, 'failed'::text, 'unknown'::text, 'missed'::text, 'cancelled'::text]))),
    CONSTRAINT reminders_title_check CHECK ((length(btrim(title)) > 0))
);

CREATE TABLE public.schedule_occurrence_dispatches (
    schedule_id uuid NOT NULL,
    occurrence_at timestamp with time zone NOT NULL,
    state text NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT schedule_occurrence_dispatches_state_check CHECK ((state = ANY (ARRAY['claimed'::text, 'dispatched'::text, 'unknown'::text])))
);

CREATE TABLE public.schedules (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    span_id uuid,
    instruction text NOT NULL,
    kind text NOT NULL,
    recurrence_expression text,
    timezone text DEFAULT 'UTC'::text NOT NULL,
    next_run_at timestamp with time zone,
    state text DEFAULT 'active'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    user_context_id uuid,
    CONSTRAINT schedules_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT schedules_instruction_check CHECK ((length(btrim(instruction)) > 0)),
    CONSTRAINT schedules_kind_check CHECK ((kind = ANY (ARRAY['once'::text, 'recurring'::text]))),
    CONSTRAINT schedules_recurrence_shape CHECK ((((kind = 'once'::text) AND (recurrence_expression IS NULL)) OR ((kind = 'recurring'::text) AND (recurrence_expression IS NOT NULL)))),
    CONSTRAINT schedules_state_check CHECK ((state = ANY (ARRAY['active'::text, 'paused'::text, 'completed'::text])))
);

CREATE VIEW public.scheduled_tasks WITH (security_invoker='true') AS
 SELECT id,
    user_id,
    instruction,
    kind AS schedule_kind,
    recurrence_expression,
    timezone,
    next_run_at,
    state,
    created_at,
    updated_at
   FROM public.schedules;

CREATE TABLE public.skill_agent_enablements (
    user_context_id uuid NOT NULL,
    skill_id uuid NOT NULL,
    agent_definition_id uuid NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.skill_installations (
    user_context_id uuid NOT NULL,
    skill_id uuid NOT NULL,
    installed_version integer NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    installed_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    independently_installed boolean DEFAULT true NOT NULL
);

CREATE TABLE public.skill_package_versions (
    skill_id uuid NOT NULL,
    version integer NOT NULL,
    instructions text NOT NULL,
    requested_capabilities text[] DEFAULT '{}'::text[] NOT NULL,
    resources jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    title text NOT NULL,
    summary text NOT NULL,
    digest text,
    search_external_key text NOT NULL,
    search_document tsvector GENERATED ALWAYS AS (to_tsvector('simple'::regconfig, ((((replace(search_external_key, '-'::text, ' '::text) || ' '::text) || title) || ' '::text) || summary))) STORED,
    CONSTRAINT skill_package_versions_version_check CHECK ((version > 0))
);

CREATE TABLE public.skill_packages (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    deployment_id uuid NOT NULL,
    owner_user_context_id uuid,
    external_key text NOT NULL,
    title text NOT NULL,
    summary text NOT NULL,
    latest_version integer DEFAULT 1 NOT NULL,
    state text DEFAULT 'active'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT skill_packages_latest_version_check CHECK ((latest_version > 0)),
    CONSTRAINT skill_packages_state_check CHECK ((state = ANY (ARRAY['active'::text, 'removed'::text])))
);

CREATE TABLE public.space_edges (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    space_id uuid NOT NULL,
    from_node uuid NOT NULL,
    to_node uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.space_messages (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    space_id uuid NOT NULL,
    role text NOT NULL,
    text text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT space_messages_role_check CHECK ((role = ANY (ARRAY['user'::text, 'assistant'::text, 'system'::text])))
);

CREATE TABLE public.space_nodes (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    space_id uuid NOT NULL,
    kind text DEFAULT 'node'::text NOT NULL,
    title text NOT NULL,
    body text DEFAULT ''::text NOT NULL,
    data jsonb DEFAULT '{}'::jsonb NOT NULL,
    state text NOT NULL,
    "position" jsonb DEFAULT '{"x": 0.0, "y": 0.0}'::jsonb NOT NULL,
    derived_from uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    provenance jsonb DEFAULT '{}'::jsonb NOT NULL,
    version integer DEFAULT 1 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT space_nodes_state_check CHECK ((state = ANY (ARRAY['running'::text, 'done'::text, 'stale'::text, 'rejected'::text])))
);

CREATE TABLE public.space_tasks (
    node_id uuid NOT NULL,
    space_id uuid NOT NULL,
    role text NOT NULL,
    brief text NOT NULL,
    status text DEFAULT 'queued'::text NOT NULL,
    attempt integer DEFAULT 0 NOT NULL,
    dedupe_key text NOT NULL,
    generation integer DEFAULT 1 NOT NULL,
    lease_token uuid,
    lease_expires_at timestamp with time zone,
    input_versions jsonb DEFAULT '{}'::jsonb NOT NULL,
    output jsonb DEFAULT '{}'::jsonb NOT NULL,
    error text,
    expanded boolean DEFAULT false NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT space_tasks_role_check CHECK ((role = ANY (ARRAY['web_search'::text, 'user_data'::text, 'synthesis'::text, 'plan'::text]))),
    CONSTRAINT space_tasks_status_check CHECK ((status = ANY (ARRAY['queued'::text, 'running'::text, 'done'::text, 'failed'::text, 'blocked'::text, 'cancelled'::text])))
);

CREATE TABLE public.space_workflow_requests (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    space_id uuid NOT NULL,
    message text NOT NULL,
    node_id uuid,
    generation integer DEFAULT 1 NOT NULL,
    processed boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.spaces (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    title text NOT NULL,
    intent text NOT NULL,
    state text NOT NULL,
    agent_spec jsonb DEFAULT '{}'::jsonb NOT NULL,
    committed_collection_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    run_state text DEFAULT 'idle'::text NOT NULL,
    run_error text,
    workflow_generation integer DEFAULT 1 NOT NULL,
    CONSTRAINT spaces_run_state_check CHECK ((run_state = ANY (ARRAY['idle'::text, 'running'::text, 'failed'::text]))),
    CONSTRAINT spaces_state_check CHECK ((state = ANY (ARRAY['ideating'::text, 'planned'::text, 'committed'::text, 'dropped'::text]))),
    CONSTRAINT spaces_title_check CHECK ((length(btrim(title)) > 0))
);

CREATE TABLE public.span_revisions (
    user_id uuid NOT NULL,
    revision bigint DEFAULT 0 NOT NULL
);

CREATE TABLE public.spans (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    user_context_id uuid,
    parent_id uuid,
    title text NOT NULL,
    notes text DEFAULT ''::text NOT NULL,
    category text DEFAULT 'general'::text NOT NULL,
    source text DEFAULT 'user'::text NOT NULL,
    source_ref text,
    status text DEFAULT 'planned'::text NOT NULL,
    start_at timestamp with time zone,
    end_at timestamp with time zone,
    due_at timestamp with time zone,
    priority integer DEFAULT 0 NOT NULL,
    execution_type text,
    execution_result jsonb DEFAULT '{}'::jsonb NOT NULL,
    data jsonb DEFAULT '{}'::jsonb NOT NULL,
    version integer DEFAULT 1 NOT NULL,
    cancellation_requested_at timestamp with time zone,
    completed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    schema_id uuid,
    source_event_id uuid,
    CONSTRAINT spans_category_check CHECK ((length(btrim(category)) > 0)),
    CONSTRAINT spans_context_presence CHECK (((user_id IS NULL) = (user_context_id IS NULL))),
    CONSTRAINT spans_data_check CHECK ((jsonb_typeof(data) = 'object'::text)),
    CONSTRAINT spans_execution_type_check CHECK ((execution_type = ANY (ARRAY['autonomous'::text, 'interactive'::text, 'manual_human'::text]))),
    CONSTRAINT spans_not_own_parent CHECK (((parent_id IS NULL) OR (parent_id <> id))),
    CONSTRAINT spans_source_check CHECK ((length(btrim(source)) > 0)),
    CONSTRAINT spans_status_check CHECK ((status = ANY (ARRAY['planned'::text, 'active'::text, 'waiting_user'::text, 'done'::text, 'failed'::text, 'cancelled'::text]))),
    CONSTRAINT spans_time_order CHECK (((start_at IS NULL) OR (end_at IS NULL) OR (end_at >= start_at))),
    CONSTRAINT spans_title_check CHECK ((length(btrim(title)) > 0))
);

CREATE TABLE public.spending_policies (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_context_id uuid NOT NULL,
    capability_external_key text NOT NULL,
    provider_external_key text NOT NULL,
    currency text NOT NULL,
    max_amount_minor bigint NOT NULL,
    version integer DEFAULT 1 NOT NULL,
    CONSTRAINT spending_policies_max_amount_minor_check CHECK ((max_amount_minor >= 0))
);

CREATE TABLE public.status_events (
    cursor bigint NOT NULL,
    user_context_id uuid NOT NULL,
    aggregate_type text NOT NULL,
    aggregate_id uuid NOT NULL,
    event_type text NOT NULL,
    state text NOT NULL,
    deduplication_key text NOT NULL,
    occurred_at timestamp with time zone NOT NULL,
    committed_at timestamp with time zone DEFAULT now() NOT NULL,
    payload jsonb DEFAULT '{}'::jsonb NOT NULL,
    CONSTRAINT status_events_payload_object CHECK ((jsonb_typeof(payload) = 'object'::text))
);

ALTER TABLE public.status_events ALTER COLUMN cursor ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME public.status_events_cursor_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

CREATE TABLE public.status_webhook_deliveries (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    subscription_id uuid NOT NULL,
    event_cursor bigint NOT NULL,
    state text DEFAULT 'queued'::text NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    available_at timestamp with time zone DEFAULT now() NOT NULL,
    lease_until timestamp with time zone,
    lease_token uuid,
    lease_owner text,
    delivered_at timestamp with time zone,
    last_http_status integer,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT status_webhook_deliveries_attempts_check CHECK ((attempts >= 0)),
    CONSTRAINT status_webhook_deliveries_state_check CHECK ((state = ANY (ARRAY['queued'::text, 'sending'::text, 'sent'::text, 'failed'::text])))
);

CREATE TABLE public.status_webhook_secrets (
    subscription_id uuid NOT NULL,
    ciphertext bytea NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.status_webhook_subscriptions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_context_id uuid NOT NULL,
    endpoint text NOT NULL,
    state text DEFAULT 'enabled'::text NOT NULL,
    secret_version integer DEFAULT 1 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT status_webhook_subscriptions_state_check CHECK ((state = ANY (ARRAY['enabled'::text, 'unhealthy'::text, 'disabled'::text])))
);

CREATE TABLE public.user_contexts (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    deployment_id uuid NOT NULL,
    host_app_id uuid NOT NULL,
    organization_id uuid,
    host_user_id text NOT NULL,
    user_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT user_contexts_host_user_id_not_empty CHECK (((length(btrim(host_user_id)) > 0) AND (octet_length(host_user_id) <= 512)))
);

CREATE TABLE public.user_notifications (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    channel text NOT NULL,
    idempotency_key text NOT NULL,
    subject text NOT NULL,
    status text DEFAULT 'pending'::text NOT NULL,
    error_code text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    sent_at timestamp with time zone,
    CONSTRAINT user_notifications_channel_check CHECK ((channel = 'email'::text)),
    CONSTRAINT user_notifications_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'sent'::text, 'failed'::text])))
);

CREATE TABLE public.user_preferences (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_context_id uuid NOT NULL,
    category text NOT NULL,
    preference_key text NOT NULL,
    value jsonb NOT NULL,
    is_sensitive boolean DEFAULT false NOT NULL,
    confirmed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT user_preferences_sensitive_confirmed CHECK (((NOT is_sensitive) OR (confirmed_at IS NOT NULL)))
);

CREATE TABLE public.users (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    status text DEFAULT 'active'::text NOT NULL,
    display_name text,
    preferences jsonb DEFAULT '{}'::jsonb NOT NULL,
    profile_facts jsonb DEFAULT '{}'::jsonb NOT NULL,
    persona jsonb DEFAULT '{"tone": "direct", "verbosity": "concise", "proactivity": "medium", "technical_depth": "standard"}'::jsonb NOT NULL,
    profile_version bigint DEFAULT 1 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    verified_email text,
    verified_email_at timestamp with time zone,
    CONSTRAINT users_persona_is_object CHECK ((jsonb_typeof(persona) = 'object'::text)),
    CONSTRAINT users_preferences_is_object CHECK ((jsonb_typeof(preferences) = 'object'::text)),
    CONSTRAINT users_profile_facts_is_object CHECK ((jsonb_typeof(profile_facts) = 'object'::text)),
    CONSTRAINT users_status_check CHECK ((status = ANY (ARRAY['provisional'::text, 'active'::text, 'disabled'::text])))
);

CREATE TABLE public.verified_integration_events (
    integration_external_key text NOT NULL,
    external_account_hash text NOT NULL,
    provider_event_id text NOT NULL,
    execution_id uuid NOT NULL,
    applied_at timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.vox_connection_setups (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    connector_id text NOT NULL,
    state_token text NOT NULL,
    status text DEFAULT 'pending'::text NOT NULL,
    connection_id uuid,
    error text,
    expires_at timestamp with time zone DEFAULT (now() + '00:15:00'::interval) NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    verifier_ciphertext bytea,
    redirect_uri text,
    consented_at timestamp with time zone
);

CREATE TABLE public.vox_connections (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    connector_id text NOT NULL,
    account_id text,
    account_display_id text,
    access_ciphertext bytea,
    refresh_ciphertext bytea,
    access_expires_at timestamp with time zone,
    authorization_state text DEFAULT 'authorized'::text NOT NULL,
    sync_timeline boolean DEFAULT true NOT NULL,
    assistant_read boolean DEFAULT true NOT NULL,
    last_synced_at timestamp with time zone,
    next_sync_at timestamp with time zone DEFAULT now() NOT NULL,
    failure_code text,
    failure_count integer DEFAULT 0 NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    generation uuid DEFAULT gen_random_uuid() NOT NULL,
    lease_token uuid,
    lease_until timestamp with time zone,
    consented_at timestamp with time zone,
    credential_generation uuid DEFAULT gen_random_uuid() NOT NULL
);

ALTER TABLE ONLY public.audit_events ALTER COLUMN cursor_id SET DEFAULT nextval('public.audit_events_cursor_id_seq'::regclass);

ALTER TABLE ONLY public.action_approvals
    ADD CONSTRAINT action_approvals_id_context_key UNIQUE (id, user_context_id);

ALTER TABLE ONLY public.action_approvals
    ADD CONSTRAINT action_approvals_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.action_approvals
    ADD CONSTRAINT action_approvals_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.action_approvals
    ADD CONSTRAINT action_approvals_proposal_id_key UNIQUE (proposal_id);

ALTER TABLE ONLY public.action_proposals
    ADD CONSTRAINT action_proposals_id_context_key UNIQUE (id, user_context_id);

ALTER TABLE ONLY public.action_proposals
    ADD CONSTRAINT action_proposals_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.action_proposals
    ADD CONSTRAINT action_proposals_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.agent_definitions
    ADD CONSTRAINT agent_definitions_deployment_id_unique UNIQUE (deployment_id, id);

ALTER TABLE ONLY public.agent_definitions
    ADD CONSTRAINT agent_definitions_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.agent_delegation_permissions
    ADD CONSTRAINT agent_delegation_permissions_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.agent_instruction_versions
    ADD CONSTRAINT agent_instruction_versions_pkey PRIMARY KEY (agent_id, version);

ALTER TABLE ONLY public.agent_memories
    ADD CONSTRAINT agent_memories_pkey PRIMARY KEY (user_context_id, agent_id);

ALTER TABLE ONLY public.agent_model_configurations
    ADD CONSTRAINT agent_model_configurations_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.agent_model_configurations
    ADD CONSTRAINT agent_model_configurations_version_unique UNIQUE (agent_definition_id, version);

ALTER TABLE ONLY public.agent_definitions
    ADD CONSTRAINT agent_owner_id UNIQUE (owner_user_context_id, id);

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_context_job_unique UNIQUE (user_context_id, job_id);

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_task_runs_pkey PRIMARY KEY (job_id);

ALTER TABLE ONLY public.audit_events
    ADD CONSTRAINT audit_events_id_key UNIQUE (id);

ALTER TABLE ONLY public.audit_events
    ADD CONSTRAINT audit_events_pkey PRIMARY KEY (cursor_id);

ALTER TABLE ONLY public.auth_identities
    ADD CONSTRAINT auth_identities_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.auth_identities
    ADD CONSTRAINT auth_identities_issuer_subject_key UNIQUE (issuer, subject);

ALTER TABLE ONLY public.auth_identities
    ADD CONSTRAINT auth_identities_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.auth_sessions
    ADD CONSTRAINT auth_sessions_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.auth_sessions
    ADD CONSTRAINT auth_sessions_token_hash_key UNIQUE (token_hash);

ALTER TABLE ONLY public.channel_identities
    ADD CONSTRAINT channel_identities_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.channel_identities
    ADD CONSTRAINT channel_identities_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.collection_spans
    ADD CONSTRAINT collection_spans_pkey PRIMARY KEY (collection_id, span_id);

ALTER TABLE ONLY public.collections
    ADD CONSTRAINT collections_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.collections
    ADD CONSTRAINT collections_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.conversations
    ADD CONSTRAINT conversations_context_channel_external_key UNIQUE (user_context_id, channel, external_conversation_id);

ALTER TABLE ONLY public.conversations
    ADD CONSTRAINT conversations_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.data_schemas
    ADD CONSTRAINT data_schemas_id_scope_key UNIQUE (id, owner_scope);

ALTER TABLE ONLY public.data_schemas
    ADD CONSTRAINT data_schemas_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.data_schemas
    ADD CONSTRAINT data_schemas_user_namespace_name_version_key UNIQUE (user_id, namespace, name, version);

ALTER TABLE ONLY public.data_source_consents
    ADD CONSTRAINT data_source_consents_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.data_source_consents
    ADD CONSTRAINT data_source_consents_user_source_unique UNIQUE (user_id, data_source);

ALTER TABLE ONLY public.agent_delegation_permissions
    ADD CONSTRAINT delegation_context_id_unique UNIQUE (user_context_id, id);

ALTER TABLE ONLY public.deployment_agent_selections
    ADD CONSTRAINT deployment_agent_selections_pkey PRIMARY KEY (deployment_id, agent_definition_id);

ALTER TABLE ONLY public.devices
    ADD CONSTRAINT devices_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.devices
    ADD CONSTRAINT devices_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.devices
    ADD CONSTRAINT devices_user_identifier_key UNIQUE (user_id, device_identifier);

ALTER TABLE ONLY public.event_agent_decisions
    ADD CONSTRAINT event_agent_decisions_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.execution_attempts
    ADD CONSTRAINT execution_attempts_execution_attempt_key UNIQUE (execution_id, attempt_number);

ALTER TABLE ONLY public.execution_attempts
    ADD CONSTRAINT execution_attempts_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_approval_id_key UNIQUE (approval_id);

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_context_idempotency_key UNIQUE (user_context_id, idempotency_key);

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_proposal_id_key UNIQUE (proposal_id);

ALTER TABLE ONLY public.federated_identity_nonces
    ADD CONSTRAINT federated_identity_nonces_pkey PRIMARY KEY (adapter_id, nonce);

ALTER TABLE ONLY public.host_app_assertion_nonces
    ADD CONSTRAINT host_app_assertion_nonces_pkey PRIMARY KEY (credential_id, nonce);

ALTER TABLE ONLY public.host_app_credentials
    ADD CONSTRAINT host_app_credentials_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.host_apps
    ADD CONSTRAINT host_apps_deployment_external_key_key UNIQUE (deployment_id, external_key);

ALTER TABLE ONLY public.host_apps
    ADD CONSTRAINT host_apps_deployment_id_id_key UNIQUE (deployment_id, id);

ALTER TABLE ONLY public.host_apps
    ADD CONSTRAINT host_apps_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.host_organizations
    ADD CONSTRAINT host_organizations_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.host_organizations
    ADD CONSTRAINT host_organizations_scope_external_key_key UNIQUE (deployment_id, host_app_id, external_key);

ALTER TABLE ONLY public.host_organizations
    ADD CONSTRAINT host_organizations_scope_id_key UNIQUE (deployment_id, host_app_id, id);

ALTER TABLE ONLY public.identity_adapters
    ADD CONSTRAINT identity_adapters_external_key_unique UNIQUE (deployment_id, external_key);

ALTER TABLE ONLY public.identity_adapters
    ADD CONSTRAINT identity_adapters_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.identity_authentication_sessions
    ADD CONSTRAINT identity_authentication_sessions_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.identity_authentication_sessions
    ADD CONSTRAINT identity_authentication_sessions_token_hash_key UNIQUE (token_hash);

ALTER TABLE ONLY public.identity_link_events
    ADD CONSTRAINT identity_link_events_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.identity_links
    ADD CONSTRAINT identity_links_pair_unique UNIQUE (left_login_identity_id, right_login_identity_id);

ALTER TABLE ONLY public.identity_links
    ADD CONSTRAINT identity_links_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.inbound_events
    ADD CONSTRAINT inbound_events_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.inbound_events
    ADD CONSTRAINT inbound_events_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.inbound_events
    ADD CONSTRAINT inbound_events_source_external_key UNIQUE (source_kind, source_id, external_event_id);

ALTER TABLE ONLY public.integration_capability_declarations
    ADD CONSTRAINT integration_capabilities_key_unique UNIQUE (integration_id, external_key);

ALTER TABLE ONLY public.integration_capability_declarations
    ADD CONSTRAINT integration_capability_declarations_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.integration_codes
    ADD CONSTRAINT integration_codes_pkey PRIMARY KEY (code_hash);

ALTER TABLE ONLY public.integration_declaration_versions
    ADD CONSTRAINT integration_declaration_versions_pkey PRIMARY KEY (integration_id, version);

ALTER TABLE ONLY public.integration_grants
    ADD CONSTRAINT integration_grants_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.integration_plan_requests
    ADD CONSTRAINT integration_plan_requests_pkey PRIMARY KEY (user_id, client_id, request_id);

ALTER TABLE ONLY public.integration_tokens
    ADD CONSTRAINT integration_tokens_pkey PRIMARY KEY (access_hash);

ALTER TABLE ONLY public.integration_tokens
    ADD CONSTRAINT integration_tokens_refresh_hash_key UNIQUE (refresh_hash);

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_context_id_unique UNIQUE (user_context_id, id);

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.login_identities
    ADD CONSTRAINT login_identities_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.login_identities
    ADD CONSTRAINT login_identities_unique_subject_in_context UNIQUE (user_context_id, adapter_id, subject_hash);

ALTER TABLE ONLY public.messages
    ADD CONSTRAINT messages_conversation_sequence_key UNIQUE (conversation_id, sequence_number);

ALTER TABLE ONLY public.messages
    ADD CONSTRAINT messages_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.agent_model_configurations
    ADD CONSTRAINT model_actor_version_unique UNIQUE (agent_definition_id, id, version);

ALTER TABLE ONLY public.operational_quota_reservations
    ADD CONSTRAINT operational_quota_reservations_pkey PRIMARY KEY (attempt_id);

ALTER TABLE ONLY public.operational_quotas
    ADD CONSTRAINT operational_quotas_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.operational_quotas
    ADD CONSTRAINT operational_quotas_user_context_id_provider_external_key_mo_key UNIQUE (user_context_id, provider_external_key, model_identifier, account_hash, connection_id);

ALTER TABLE ONLY public.passwordless_recovery_challenges
    ADD CONSTRAINT passwordless_recovery_challenges_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.pending_phone_links
    ADD CONSTRAINT pending_phone_links_pkey PRIMARY KEY (user_id);

ALTER TABLE ONLY public.phone_verifications
    ADD CONSTRAINT phone_verifications_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.platform_deployments
    ADD CONSTRAINT platform_deployments_external_key_key UNIQUE (external_key);

ALTER TABLE ONLY public.platform_deployments
    ADD CONSTRAINT platform_deployments_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.reminder_deliveries
    ADD CONSTRAINT reminder_deliveries_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.reminders
    ADD CONSTRAINT reminders_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.schedule_occurrence_dispatches
    ADD CONSTRAINT schedule_occurrence_dispatches_pkey PRIMARY KEY (schedule_id, occurrence_at);

ALTER TABLE ONLY public.schedules
    ADD CONSTRAINT schedules_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.schedules
    ADD CONSTRAINT schedules_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.skill_agent_enablements
    ADD CONSTRAINT skill_agent_enablements_pkey PRIMARY KEY (user_context_id, skill_id, agent_definition_id);

ALTER TABLE ONLY public.skill_installations
    ADD CONSTRAINT skill_installations_pkey PRIMARY KEY (user_context_id, skill_id);

ALTER TABLE ONLY public.skill_package_versions
    ADD CONSTRAINT skill_package_versions_pkey PRIMARY KEY (skill_id, version);

ALTER TABLE ONLY public.skill_packages
    ADD CONSTRAINT skill_packages_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.space_edges
    ADD CONSTRAINT space_edges_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.space_edges
    ADD CONSTRAINT space_edges_unique_edge UNIQUE (space_id, from_node, to_node);

ALTER TABLE ONLY public.space_messages
    ADD CONSTRAINT space_messages_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.space_nodes
    ADD CONSTRAINT space_nodes_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.space_tasks
    ADD CONSTRAINT space_tasks_pkey PRIMARY KEY (node_id);

ALTER TABLE ONLY public.space_tasks
    ADD CONSTRAINT space_tasks_space_id_generation_dedupe_key_key UNIQUE (space_id, generation, dedupe_key);

ALTER TABLE ONLY public.space_workflow_requests
    ADD CONSTRAINT space_workflow_requests_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.spaces
    ADD CONSTRAINT spaces_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.span_revisions
    ADD CONSTRAINT span_revisions_pkey PRIMARY KEY (user_id);

ALTER TABLE ONLY public.spans
    ADD CONSTRAINT spans_id_user_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.spans
    ADD CONSTRAINT spans_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.spans
    ADD CONSTRAINT spans_source_ref_key UNIQUE (user_id, source, source_ref);

ALTER TABLE ONLY public.spending_policies
    ADD CONSTRAINT spending_policies_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.spending_policies
    ADD CONSTRAINT spending_policies_user_context_id_capability_external_key_p_key UNIQUE (user_context_id, capability_external_key, provider_external_key, currency);

ALTER TABLE ONLY public.status_events
    ADD CONSTRAINT status_events_dedup UNIQUE (user_context_id, deduplication_key);

ALTER TABLE ONLY public.status_events
    ADD CONSTRAINT status_events_pkey PRIMARY KEY (cursor);

ALTER TABLE ONLY public.status_webhook_deliveries
    ADD CONSTRAINT status_webhook_deliveries_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.status_webhook_deliveries
    ADD CONSTRAINT status_webhook_deliveries_subscription_id_event_cursor_key UNIQUE (subscription_id, event_cursor);

ALTER TABLE ONLY public.status_webhook_secrets
    ADD CONSTRAINT status_webhook_secrets_pkey PRIMARY KEY (subscription_id);

ALTER TABLE ONLY public.status_webhook_subscriptions
    ADD CONSTRAINT status_webhook_subscriptions_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.user_contexts
    ADD CONSTRAINT user_contexts_deployment_id_unique UNIQUE (deployment_id, id);

ALTER TABLE ONLY public.user_contexts
    ADD CONSTRAINT user_contexts_id_user_id_key UNIQUE (id, user_id);

ALTER TABLE ONLY public.user_contexts
    ADD CONSTRAINT user_contexts_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.user_contexts
    ADD CONSTRAINT user_contexts_user_id_key UNIQUE (user_id);

ALTER TABLE ONLY public.user_notifications
    ADD CONSTRAINT user_notifications_idempotency_key UNIQUE (user_id, idempotency_key);

ALTER TABLE ONLY public.user_notifications
    ADD CONSTRAINT user_notifications_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.user_preferences
    ADD CONSTRAINT user_preferences_context_key_uniq UNIQUE (user_context_id, preference_key);

ALTER TABLE ONLY public.user_preferences
    ADD CONSTRAINT user_preferences_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.verified_integration_events
    ADD CONSTRAINT verified_integration_events_pkey PRIMARY KEY (integration_external_key, external_account_hash, provider_event_id);

ALTER TABLE ONLY public.vox_connection_setups
    ADD CONSTRAINT vox_connection_setups_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.vox_connection_setups
    ADD CONSTRAINT vox_connection_setups_state_token_key UNIQUE (state_token);

ALTER TABLE ONLY public.vox_connections
    ADD CONSTRAINT vox_connections_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.vox_connections
    ADD CONSTRAINT vox_connections_user_connector_unique UNIQUE (user_id, connector_id);

CREATE INDEX action_approvals_context_idx ON public.action_approvals USING btree (user_context_id);

CREATE INDEX action_proposals_context_idx ON public.action_proposals USING btree (user_context_id);

CREATE INDEX agent_definitions_deployment_enabled_idx ON public.agent_definitions USING btree (deployment_id, external_key) WHERE (state = 'enabled'::text);

CREATE UNIQUE INDEX agent_owned_default ON public.agent_definitions USING btree (owner_user_context_id) WHERE is_default;

CREATE UNIQUE INDEX agent_owned_key ON public.agent_definitions USING btree (owner_user_context_id, external_key) WHERE (owner_user_context_id IS NOT NULL);

CREATE UNIQUE INDEX agent_template_key ON public.agent_definitions USING btree (deployment_id, external_key) WHERE (owner_user_context_id IS NULL);

CREATE INDEX assigned_runs_parent_idx ON public.assigned_task_runs USING btree (parent_run_id) WHERE (parent_run_id IS NOT NULL);

CREATE INDEX audit_events_context_idx ON public.audit_events USING btree (user_context_id);

CREATE INDEX audit_events_user_time_idx ON public.audit_events USING btree (user_id, occurred_at DESC);

CREATE INDEX auth_identities_context_idx ON public.auth_identities USING btree (user_context_id);

CREATE INDEX auth_sessions_context_idx ON public.auth_sessions USING btree (user_context_id);

CREATE UNIQUE INDEX channel_identities_active_idx ON public.channel_identities USING btree (channel, provider_scope, normalized_external_id) WHERE (revoked_at IS NULL);

CREATE INDEX channel_identities_context_idx ON public.channel_identities USING btree (user_context_id);

CREATE INDEX collection_spans_span_idx ON public.collection_spans USING btree (span_id);

CREATE INDEX collections_context_idx ON public.collections USING btree (user_context_id);

CREATE INDEX collections_user_kind_idx ON public.collections USING btree (user_id, kind, status);

CREATE INDEX conversations_agent_memory ON public.conversations USING btree (user_context_id, agent_external_key, updated_at DESC) WHERE (summary_version > 0);

CREATE INDEX conversations_context_idx ON public.conversations USING btree (user_context_id);

CREATE INDEX data_schemas_context_idx ON public.data_schemas USING btree (user_context_id);

CREATE INDEX data_schemas_lookup_idx ON public.data_schemas USING btree (user_id, namespace, name);

CREATE INDEX delegation_context_idx ON public.agent_delegation_permissions USING btree (user_context_id, created_at DESC);

CREATE INDEX devices_context_idx ON public.devices USING btree (user_context_id);

CREATE INDEX devices_user_active_idx ON public.devices USING btree (user_id, is_active);

CREATE INDEX event_agent_decisions_user_idx ON public.event_agent_decisions USING btree (user_id, created_at DESC);

CREATE INDEX executions_context_idx ON public.executions USING btree (user_context_id);

CREATE INDEX federated_identity_nonces_expiry_idx ON public.federated_identity_nonces USING btree (expires_at);

CREATE INDEX host_app_assertion_nonces_expiry_idx ON public.host_app_assertion_nonces USING btree (expires_at);

CREATE INDEX host_app_credentials_active_idx ON public.host_app_credentials USING btree (id) WHERE (state = 'active'::text);

CREATE INDEX identity_authentication_sessions_active_idx ON public.identity_authentication_sessions USING btree (token_hash, expires_at) WHERE (consumed_at IS NULL);

CREATE INDEX inbound_events_batch_idx ON public.inbound_events USING btree (batch_id) WHERE (batch_id IS NOT NULL);

CREATE INDEX inbound_events_context_idx ON public.inbound_events USING btree (user_context_id);

CREATE INDEX inbound_events_requeue_idx ON public.inbound_events USING btree (failed_at) WHERE ((processing_error IS NOT NULL) AND retryable);

CREATE INDEX inbound_events_user_processed_idx ON public.inbound_events USING btree (user_id, processed_at) WHERE (processed_at IS NULL);

CREATE INDEX jobs_context_idx ON public.jobs USING btree (user_context_id);

CREATE INDEX jobs_lease_recovery_idx ON public.jobs USING btree (lease_expires_at) WHERE (state = 'running'::text);

CREATE INDEX jobs_queued_idx ON public.jobs USING btree (priority DESC, available_at, id) WHERE (state = 'pending'::text);

CREATE INDEX jobs_user_kind_idx ON public.jobs USING btree (user_id, kind, state);

CREATE INDEX messages_conv_seq_idx ON public.messages USING btree (conversation_id, sequence_number);

CREATE INDEX passwordless_recovery_challenges_active_idx ON public.passwordless_recovery_challenges USING btree (adapter_id, user_context_id, expires_at) WHERE (consumed_at IS NULL);

CREATE INDEX pending_phone_links_phone_idx ON public.pending_phone_links USING btree (normalized_phone);

CREATE INDEX phone_verifications_phone_idx ON public.phone_verifications USING btree (normalized_phone, created_at DESC);

CREATE INDEX phone_verifications_user_idx ON public.phone_verifications USING btree (user_id, created_at DESC);

CREATE INDEX reminder_deliveries_reminder_idx ON public.reminder_deliveries USING btree (reminder_id, scheduled_for);

CREATE INDEX reminders_active_idx ON public.reminders USING btree (next_trigger_at) WHERE (status = 'scheduled'::text);

CREATE INDEX reminders_span_idx ON public.reminders USING btree (span_id) WHERE (span_id IS NOT NULL);

CREATE INDEX reminders_user_context_idx ON public.reminders USING btree (user_context_id);

CREATE INDEX schedules_active_idx ON public.schedules USING btree (next_run_at) WHERE (state = 'active'::text);

CREATE INDEX schedules_context_idx ON public.schedules USING btree (user_context_id);

CREATE INDEX skill_package_versions_search ON public.skill_package_versions USING gin (search_document);

CREATE UNIQUE INDEX skill_packages_curated_key ON public.skill_packages USING btree (deployment_id, external_key) WHERE (owner_user_context_id IS NULL);

CREATE UNIQUE INDEX skill_packages_private_key ON public.skill_packages USING btree (owner_user_context_id, external_key) WHERE (owner_user_context_id IS NOT NULL);

CREATE INDEX space_edges_space_idx ON public.space_edges USING btree (space_id);

CREATE INDEX space_messages_space_created_idx ON public.space_messages USING btree (space_id, created_at);

CREATE INDEX space_nodes_space_idx ON public.space_nodes USING btree (space_id, created_at);

CREATE INDEX spaces_user_created_idx ON public.spaces USING btree (user_id, created_at DESC);

CREATE INDEX spans_context_idx ON public.spans USING btree (user_context_id);

CREATE INDEX spans_data_gin ON public.spans USING gin (data jsonb_path_ops);

CREATE INDEX spans_parent_idx ON public.spans USING btree (parent_id) WHERE (parent_id IS NOT NULL);

CREATE INDEX spans_sms_category_idx ON public.spans USING btree (user_id, category) WHERE (source = 'sms'::text);

CREATE INDEX spans_sms_fingerprint_idx ON public.spans USING btree (user_id, ((data ->> 'fingerprint'::text))) WHERE (source = 'sms'::text);

CREATE INDEX spans_user_range_gist ON public.spans USING gist (user_id, public.span_range(start_at, end_at)) WHERE (start_at IS NOT NULL);

CREATE INDEX spans_user_schema_idx ON public.spans USING btree (user_id, schema_id, start_at DESC);

CREATE INDEX spans_user_start_idx ON public.spans USING btree (user_id, start_at);

CREATE INDEX spans_user_status_idx ON public.spans USING btree (user_id, status, due_at);

CREATE INDEX status_events_context_cursor_idx ON public.status_events USING btree (user_context_id, cursor);

CREATE INDEX status_webhook_deliveries_ready_idx ON public.status_webhook_deliveries USING btree (available_at, id) WHERE (state = ANY (ARRAY['queued'::text, 'sending'::text]));

CREATE INDEX status_webhook_subscriptions_context_idx ON public.status_webhook_subscriptions USING btree (user_context_id, id);

CREATE UNIQUE INDEX user_contexts_organized_subject_key ON public.user_contexts USING btree (deployment_id, host_app_id, organization_id, host_user_id) WHERE (organization_id IS NOT NULL);

CREATE INDEX user_contexts_scope_idx ON public.user_contexts USING btree (deployment_id, host_app_id, organization_id);

CREATE UNIQUE INDEX user_contexts_unorganized_subject_key ON public.user_contexts USING btree (deployment_id, host_app_id, host_user_id) WHERE (organization_id IS NULL);

CREATE INDEX user_notifications_user_recent_idx ON public.user_notifications USING btree (user_id, created_at DESC);

CREATE INDEX user_preferences_context_category_idx ON public.user_preferences USING btree (user_context_id, category);

CREATE INDEX vox_connection_setups_token_idx ON public.vox_connection_setups USING btree (state_token);

CREATE INDEX vox_connections_sync_due_idx ON public.vox_connections USING btree (next_sync_at) WHERE ((authorization_state = 'authorized'::text) AND sync_timeline);

CREATE TRIGGER action_approvals_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.action_approvals FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER action_proposals_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.action_proposals FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER assigned_run_actor_immutable BEFORE UPDATE ON public.assigned_task_runs FOR EACH ROW EXECUTE FUNCTION public.immutable_assigned_run_actor();

CREATE TRIGGER audit_events_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.audit_events FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER auth_identities_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.auth_identities FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER auth_sessions_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.auth_sessions FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.action_approvals FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.action_proposals FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.audit_events FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.auth_identities FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.auth_sessions FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.channel_identities FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.collections FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.conversations FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.data_schemas FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.devices FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.executions FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.inbound_events FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.jobs FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.schedules FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER bind_owned_context BEFORE INSERT OR UPDATE ON public.spans FOR EACH ROW EXECUTE FUNCTION public.bind_owned_context();

CREATE TRIGGER channel_identities_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.channel_identities FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER collections_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.collections FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER conversations_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.conversations FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER data_schemas_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.data_schemas FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER delegation_scope_immutable BEFORE UPDATE ON public.agent_delegation_permissions FOR EACH ROW EXECUTE FUNCTION public.immutable_delegation_scope();

CREATE TRIGGER devices_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.devices FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER executions_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.executions FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER inbound_events_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.inbound_events FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER jobs_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.jobs FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER jobs_status_event AFTER INSERT OR UPDATE OF state, wait_reason ON public.jobs FOR EACH ROW EXECUTE FUNCTION public.record_run_status_event();

CREATE TRIGGER reminders_sync_span BEFORE INSERT OR UPDATE ON public.reminders FOR EACH ROW EXECUTE FUNCTION public.sync_reminder_span();

CREATE TRIGGER schedules_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.schedules FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER skill_version_search_key BEFORE INSERT OR UPDATE OF skill_id ON public.skill_package_versions FOR EACH ROW EXECUTE FUNCTION public.index_skill_external_key();

CREATE TRIGGER spans_context_compatibility BEFORE INSERT OR UPDATE OF user_id, user_context_id ON public.spans FOR EACH ROW EXECUTE FUNCTION public.resource_context_compatibility();

CREATE TRIGGER spans_revision_delete AFTER DELETE ON public.spans REFERENCING OLD TABLE AS changed FOR EACH STATEMENT EXECUTE FUNCTION public.bump_span_revisions();

CREATE TRIGGER spans_revision_insert AFTER INSERT ON public.spans REFERENCING NEW TABLE AS changed FOR EACH STATEMENT EXECUTE FUNCTION public.bump_span_revisions();

CREATE TRIGGER spans_revision_update AFTER UPDATE ON public.spans REFERENCING NEW TABLE AS changed FOR EACH STATEMENT EXECUTE FUNCTION public.bump_span_revisions();

CREATE TRIGGER spans_status_event AFTER INSERT OR UPDATE OF status ON public.spans FOR EACH ROW EXECUTE FUNCTION public.record_span_status_event();

CREATE TRIGGER status_event_webhook_outbox AFTER INSERT ON public.status_events FOR EACH ROW EXECUTE FUNCTION public.enqueue_status_webhooks();

ALTER TABLE ONLY public.action_approvals
    ADD CONSTRAINT action_approvals_consumed_fk FOREIGN KEY (consumed_execution_id) REFERENCES public.executions(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.action_approvals
    ADD CONSTRAINT action_approvals_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.action_approvals
    ADD CONSTRAINT action_approvals_proposal_context_fk FOREIGN KEY (proposal_id, user_context_id) REFERENCES public.action_proposals(id, user_context_id) ON DELETE CASCADE;

ALTER TABLE ONLY public.action_approvals
    ADD CONSTRAINT action_approvals_proposal_owner_fk FOREIGN KEY (proposal_id, user_id) REFERENCES public.action_proposals(id, user_id) ON DELETE CASCADE DEFERRABLE;

ALTER TABLE ONLY public.action_approvals
    ADD CONSTRAINT action_approvals_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.action_proposals
    ADD CONSTRAINT action_proposals_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.action_proposals
    ADD CONSTRAINT action_proposals_job_id_fkey FOREIGN KEY (job_id) REFERENCES public.jobs(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.action_proposals
    ADD CONSTRAINT action_proposals_job_owner_fk FOREIGN KEY (job_id, user_id) REFERENCES public.jobs(id, user_id) ON DELETE SET NULL (job_id) DEFERRABLE;

ALTER TABLE ONLY public.action_proposals
    ADD CONSTRAINT action_proposals_span_owner_fk FOREIGN KEY (span_id, user_id) REFERENCES public.spans(id, user_id) ON DELETE SET NULL (span_id) DEFERRABLE;

ALTER TABLE ONLY public.action_proposals
    ADD CONSTRAINT action_proposals_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.agent_definitions
    ADD CONSTRAINT agent_definitions_deployment_id_fkey FOREIGN KEY (deployment_id) REFERENCES public.platform_deployments(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.agent_definitions
    ADD CONSTRAINT agent_definitions_template_id_fkey FOREIGN KEY (template_id) REFERENCES public.agent_definitions(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.agent_delegation_permissions
    ADD CONSTRAINT agent_delegation_permissions_parent_run_id_fkey FOREIGN KEY (parent_run_id) REFERENCES public.assigned_task_runs(job_id);

ALTER TABLE ONLY public.agent_delegation_permissions
    ADD CONSTRAINT agent_delegation_permissions_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id);

ALTER TABLE ONLY public.agent_delegation_permissions
    ADD CONSTRAINT agent_delegation_permissions_user_context_id_requester_age_fkey FOREIGN KEY (user_context_id, requester_agent_id) REFERENCES public.agent_definitions(owner_user_context_id, id);

ALTER TABLE ONLY public.agent_delegation_permissions
    ADD CONSTRAINT agent_delegation_permissions_user_context_id_specialist_ag_fkey FOREIGN KEY (user_context_id, specialist_agent_id) REFERENCES public.agent_definitions(owner_user_context_id, id);

ALTER TABLE ONLY public.agent_instruction_versions
    ADD CONSTRAINT agent_instruction_versions_agent_id_fkey FOREIGN KEY (agent_id) REFERENCES public.agent_definitions(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.agent_memories
    ADD CONSTRAINT agent_memories_user_context_id_agent_id_fkey FOREIGN KEY (user_context_id, agent_id) REFERENCES public.agent_definitions(owner_user_context_id, id) ON DELETE CASCADE;

ALTER TABLE ONLY public.agent_model_configurations
    ADD CONSTRAINT agent_model_configurations_agent_definition_id_fkey FOREIGN KEY (agent_definition_id) REFERENCES public.agent_definitions(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.agent_definitions
    ADD CONSTRAINT agent_owner_scope FOREIGN KEY (deployment_id, owner_user_context_id) REFERENCES public.user_contexts(deployment_id, id);

ALTER TABLE ONLY public.agent_definitions
    ADD CONSTRAINT agent_template_scope FOREIGN KEY (deployment_id, template_id) REFERENCES public.agent_definitions(deployment_id, id);

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_delegation_permission_fk FOREIGN KEY (delegation_permission_id) REFERENCES public.agent_delegation_permissions(id);

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_parent_context_fk FOREIGN KEY (user_context_id, parent_run_id) REFERENCES public.assigned_task_runs(user_context_id, job_id);

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_permission_context_fk FOREIGN KEY (user_context_id, delegation_permission_id) REFERENCES public.agent_delegation_permissions(user_context_id, id);

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_task_runs_agent_id_instruction_version_fkey FOREIGN KEY (agent_id, instruction_version) REFERENCES public.agent_instruction_versions(agent_id, version) ON DELETE RESTRICT;

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_task_runs_agent_id_model_configuration_id_model_v_fkey FOREIGN KEY (agent_id, model_configuration_id, model_version) REFERENCES public.agent_model_configurations(agent_definition_id, id, version) ON DELETE RESTRICT;

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_task_runs_parent_run_id_fkey FOREIGN KEY (parent_run_id) REFERENCES public.assigned_task_runs(job_id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_task_runs_pending_proposal_id_fkey FOREIGN KEY (pending_proposal_id) REFERENCES public.action_proposals(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_task_runs_user_context_id_agent_id_fkey FOREIGN KEY (user_context_id, agent_id) REFERENCES public.agent_definitions(owner_user_context_id, id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.assigned_task_runs
    ADD CONSTRAINT assigned_task_runs_user_context_id_job_id_fkey FOREIGN KEY (user_context_id, job_id) REFERENCES public.jobs(user_context_id, id) ON DELETE CASCADE;

ALTER TABLE ONLY public.audit_events
    ADD CONSTRAINT audit_events_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.audit_events
    ADD CONSTRAINT audit_events_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.auth_identities
    ADD CONSTRAINT auth_identities_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.auth_identities
    ADD CONSTRAINT auth_identities_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.auth_sessions
    ADD CONSTRAINT auth_sessions_auth_identity_id_fkey FOREIGN KEY (auth_identity_id) REFERENCES public.auth_identities(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.auth_sessions
    ADD CONSTRAINT auth_sessions_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.auth_sessions
    ADD CONSTRAINT auth_sessions_device_fk FOREIGN KEY (device_id) REFERENCES public.devices(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.auth_sessions
    ADD CONSTRAINT auth_sessions_device_owner_fk FOREIGN KEY (device_id, user_id) REFERENCES public.devices(id, user_id) ON DELETE SET NULL (device_id) DEFERRABLE;

ALTER TABLE ONLY public.auth_sessions
    ADD CONSTRAINT auth_sessions_identity_owner_fk FOREIGN KEY (auth_identity_id, user_id) REFERENCES public.auth_identities(id, user_id) ON DELETE SET NULL (auth_identity_id) DEFERRABLE;

ALTER TABLE ONLY public.auth_sessions
    ADD CONSTRAINT auth_sessions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.channel_identities
    ADD CONSTRAINT channel_identities_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.channel_identities
    ADD CONSTRAINT channel_identities_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.collection_spans
    ADD CONSTRAINT collection_spans_collection_owner_fk FOREIGN KEY (collection_id, user_id) REFERENCES public.collections(id, user_id) ON DELETE CASCADE DEFERRABLE;

ALTER TABLE ONLY public.collection_spans
    ADD CONSTRAINT collection_spans_span_owner_fk FOREIGN KEY (span_id, user_id) REFERENCES public.spans(id, user_id) ON DELETE CASCADE DEFERRABLE;

ALTER TABLE ONLY public.collections
    ADD CONSTRAINT collections_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.collections
    ADD CONSTRAINT collections_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.conversations
    ADD CONSTRAINT conversations_channel_identity_id_fkey FOREIGN KEY (channel_identity_id) REFERENCES public.channel_identities(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.conversations
    ADD CONSTRAINT conversations_channel_identity_owner_fk FOREIGN KEY (channel_identity_id, user_id) REFERENCES public.channel_identities(id, user_id) ON DELETE SET NULL (channel_identity_id) DEFERRABLE;

ALTER TABLE ONLY public.conversations
    ADD CONSTRAINT conversations_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.conversations
    ADD CONSTRAINT conversations_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.data_schemas
    ADD CONSTRAINT data_schemas_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.data_schemas
    ADD CONSTRAINT data_schemas_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.data_source_consents
    ADD CONSTRAINT data_source_consents_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.agent_delegation_permissions
    ADD CONSTRAINT delegation_parent_context_fk FOREIGN KEY (user_context_id, parent_run_id) REFERENCES public.assigned_task_runs(user_context_id, job_id);

ALTER TABLE ONLY public.deployment_agent_selections
    ADD CONSTRAINT deployment_agent_selections_agent_definition_id_fkey FOREIGN KEY (agent_definition_id) REFERENCES public.agent_definitions(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.deployment_agent_selections
    ADD CONSTRAINT deployment_agent_selections_definition_scope_fkey FOREIGN KEY (deployment_id, agent_definition_id) REFERENCES public.agent_definitions(deployment_id, id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.deployment_agent_selections
    ADD CONSTRAINT deployment_agent_selections_deployment_id_fkey FOREIGN KEY (deployment_id) REFERENCES public.platform_deployments(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.deployment_agent_selections
    ADD CONSTRAINT deployment_agent_selections_model_configuration_id_fkey FOREIGN KEY (model_configuration_id) REFERENCES public.agent_model_configurations(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.devices
    ADD CONSTRAINT devices_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.devices
    ADD CONSTRAINT devices_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.event_agent_decisions
    ADD CONSTRAINT event_agent_decisions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.execution_attempts
    ADD CONSTRAINT execution_attempts_execution_id_fkey FOREIGN KEY (execution_id) REFERENCES public.executions(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_approval_context_fk FOREIGN KEY (approval_id, user_context_id) REFERENCES public.action_approvals(id, user_context_id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_approval_owner_fk FOREIGN KEY (approval_id, user_id) REFERENCES public.action_approvals(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_proposal_context_fk FOREIGN KEY (proposal_id, user_context_id) REFERENCES public.action_proposals(id, user_context_id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_proposal_owner_fk FOREIGN KEY (proposal_id, user_id) REFERENCES public.action_proposals(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.executions
    ADD CONSTRAINT executions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.federated_identity_nonces
    ADD CONSTRAINT federated_identity_nonces_adapter_id_fkey FOREIGN KEY (adapter_id) REFERENCES public.identity_adapters(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.host_app_assertion_nonces
    ADD CONSTRAINT host_app_assertion_nonces_credential_id_fkey FOREIGN KEY (credential_id) REFERENCES public.host_app_credentials(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.host_app_credentials
    ADD CONSTRAINT host_app_credentials_host_app_fkey FOREIGN KEY (deployment_id, host_app_id) REFERENCES public.host_apps(deployment_id, id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.host_apps
    ADD CONSTRAINT host_apps_deployment_id_fkey FOREIGN KEY (deployment_id) REFERENCES public.platform_deployments(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.host_organizations
    ADD CONSTRAINT host_organizations_host_app_fkey FOREIGN KEY (deployment_id, host_app_id) REFERENCES public.host_apps(deployment_id, id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.identity_adapters
    ADD CONSTRAINT identity_adapters_deployment_id_fkey FOREIGN KEY (deployment_id) REFERENCES public.platform_deployments(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.identity_authentication_sessions
    ADD CONSTRAINT identity_authentication_sessions_login_identity_id_fkey FOREIGN KEY (login_identity_id) REFERENCES public.login_identities(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.identity_link_events
    ADD CONSTRAINT identity_link_events_link_id_fkey FOREIGN KEY (link_id) REFERENCES public.identity_links(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.identity_links
    ADD CONSTRAINT identity_links_left_login_identity_id_fkey FOREIGN KEY (left_login_identity_id) REFERENCES public.login_identities(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.identity_links
    ADD CONSTRAINT identity_links_right_login_identity_id_fkey FOREIGN KEY (right_login_identity_id) REFERENCES public.login_identities(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.inbound_events
    ADD CONSTRAINT inbound_events_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.inbound_events
    ADD CONSTRAINT inbound_events_execution_id_fkey FOREIGN KEY (execution_id) REFERENCES public.executions(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.inbound_events
    ADD CONSTRAINT inbound_events_execution_owner_fk FOREIGN KEY (execution_id, user_id) REFERENCES public.executions(id, user_id) ON DELETE SET NULL (execution_id) DEFERRABLE;

ALTER TABLE ONLY public.inbound_events
    ADD CONSTRAINT inbound_events_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.integration_codes
    ADD CONSTRAINT integration_codes_grant_id_fkey FOREIGN KEY (grant_id) REFERENCES public.integration_grants(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.integration_grants
    ADD CONSTRAINT integration_grants_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.integration_plan_requests
    ADD CONSTRAINT integration_plan_requests_span_id_fkey FOREIGN KEY (span_id) REFERENCES public.spans(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.integration_plan_requests
    ADD CONSTRAINT integration_plan_requests_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.integration_tokens
    ADD CONSTRAINT integration_tokens_grant_id_fkey FOREIGN KEY (grant_id) REFERENCES public.integration_grants(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_schedule_id_fkey FOREIGN KEY (schedule_id) REFERENCES public.schedules(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_schedule_owner_fk FOREIGN KEY (schedule_id, user_id) REFERENCES public.schedules(id, user_id) ON DELETE SET NULL (schedule_id) DEFERRABLE;

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_source_event_fk FOREIGN KEY (source_event_id) REFERENCES public.inbound_events(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_source_event_owner_fk FOREIGN KEY (source_event_id, user_id) REFERENCES public.inbound_events(id, user_id) ON DELETE SET NULL (source_event_id) DEFERRABLE;

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_span_owner_fk FOREIGN KEY (span_id, user_id) REFERENCES public.spans(id, user_id) ON DELETE SET NULL (span_id) DEFERRABLE;

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.login_identities
    ADD CONSTRAINT login_identities_adapter_id_fkey FOREIGN KEY (adapter_id) REFERENCES public.identity_adapters(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.login_identities
    ADD CONSTRAINT login_identities_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.messages
    ADD CONSTRAINT messages_conversation_id_fkey FOREIGN KEY (conversation_id) REFERENCES public.conversations(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.operational_quota_reservations
    ADD CONSTRAINT operational_quota_reservations_approval_id_fkey FOREIGN KEY (approval_id) REFERENCES public.action_approvals(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.operational_quota_reservations
    ADD CONSTRAINT operational_quota_reservations_quota_id_fkey FOREIGN KEY (quota_id) REFERENCES public.operational_quotas(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.operational_quotas
    ADD CONSTRAINT operational_quotas_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.passwordless_recovery_challenges
    ADD CONSTRAINT passwordless_recovery_challenges_adapter_id_fkey FOREIGN KEY (adapter_id) REFERENCES public.identity_adapters(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.passwordless_recovery_challenges
    ADD CONSTRAINT passwordless_recovery_challenges_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.pending_phone_links
    ADD CONSTRAINT pending_phone_links_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.phone_verifications
    ADD CONSTRAINT phone_verifications_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.reminder_deliveries
    ADD CONSTRAINT reminder_deliveries_reminder_id_fkey FOREIGN KEY (reminder_id) REFERENCES public.reminders(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.reminders
    ADD CONSTRAINT reminders_span_id_fkey FOREIGN KEY (span_id) REFERENCES public.spans(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.reminders
    ADD CONSTRAINT reminders_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.schedule_occurrence_dispatches
    ADD CONSTRAINT schedule_occurrence_dispatches_schedule_id_fkey FOREIGN KEY (schedule_id) REFERENCES public.schedules(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.schedules
    ADD CONSTRAINT schedules_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.schedules
    ADD CONSTRAINT schedules_span_owner_fk FOREIGN KEY (span_id, user_id) REFERENCES public.spans(id, user_id) ON DELETE SET NULL (span_id) DEFERRABLE;

ALTER TABLE ONLY public.schedules
    ADD CONSTRAINT schedules_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.skill_agent_enablements
    ADD CONSTRAINT skill_agent_enablements_agent_definition_id_fkey FOREIGN KEY (agent_definition_id) REFERENCES public.agent_definitions(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.skill_agent_enablements
    ADD CONSTRAINT skill_agent_enablements_skill_id_fkey FOREIGN KEY (skill_id) REFERENCES public.skill_packages(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.skill_agent_enablements
    ADD CONSTRAINT skill_agent_enablements_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.skill_agent_enablements
    ADD CONSTRAINT skill_agent_enablements_user_context_id_skill_id_fkey FOREIGN KEY (user_context_id, skill_id) REFERENCES public.skill_installations(user_context_id, skill_id) ON DELETE CASCADE;

ALTER TABLE ONLY public.skill_installations
    ADD CONSTRAINT skill_installations_skill_id_fkey FOREIGN KEY (skill_id) REFERENCES public.skill_packages(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.skill_installations
    ADD CONSTRAINT skill_installations_skill_id_installed_version_fkey FOREIGN KEY (skill_id, installed_version) REFERENCES public.skill_package_versions(skill_id, version);

ALTER TABLE ONLY public.skill_installations
    ADD CONSTRAINT skill_installations_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.skill_agent_enablements
    ADD CONSTRAINT skill_owned_agent FOREIGN KEY (user_context_id, agent_definition_id) REFERENCES public.agent_definitions(owner_user_context_id, id);

ALTER TABLE ONLY public.skill_package_versions
    ADD CONSTRAINT skill_package_versions_skill_id_fkey FOREIGN KEY (skill_id) REFERENCES public.skill_packages(id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.skill_packages
    ADD CONSTRAINT skill_packages_deployment_id_fkey FOREIGN KEY (deployment_id) REFERENCES public.platform_deployments(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.skill_packages
    ADD CONSTRAINT skill_packages_owner_user_context_id_fkey FOREIGN KEY (owner_user_context_id) REFERENCES public.user_contexts(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.space_edges
    ADD CONSTRAINT space_edges_from_node_fkey FOREIGN KEY (from_node) REFERENCES public.space_nodes(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.space_edges
    ADD CONSTRAINT space_edges_space_id_fkey FOREIGN KEY (space_id) REFERENCES public.spaces(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.space_edges
    ADD CONSTRAINT space_edges_to_node_fkey FOREIGN KEY (to_node) REFERENCES public.space_nodes(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.space_messages
    ADD CONSTRAINT space_messages_space_id_fkey FOREIGN KEY (space_id) REFERENCES public.spaces(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.space_nodes
    ADD CONSTRAINT space_nodes_space_id_fkey FOREIGN KEY (space_id) REFERENCES public.spaces(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.space_tasks
    ADD CONSTRAINT space_tasks_node_id_fkey FOREIGN KEY (node_id) REFERENCES public.space_nodes(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.space_tasks
    ADD CONSTRAINT space_tasks_space_id_fkey FOREIGN KEY (space_id) REFERENCES public.spaces(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.space_workflow_requests
    ADD CONSTRAINT space_workflow_requests_node_id_fkey FOREIGN KEY (node_id) REFERENCES public.space_nodes(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.space_workflow_requests
    ADD CONSTRAINT space_workflow_requests_space_id_fkey FOREIGN KEY (space_id) REFERENCES public.spaces(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.spaces
    ADD CONSTRAINT spaces_committed_collection_id_fkey FOREIGN KEY (committed_collection_id) REFERENCES public.collections(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.spaces
    ADD CONSTRAINT spaces_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.spans
    ADD CONSTRAINT spans_context_owner_fk FOREIGN KEY (user_context_id, user_id) REFERENCES public.user_contexts(id, user_id) ON DELETE RESTRICT DEFERRABLE;

ALTER TABLE ONLY public.spans
    ADD CONSTRAINT spans_parent_owner_fk FOREIGN KEY (parent_id, user_id) REFERENCES public.spans(id, user_id) ON DELETE SET NULL (parent_id) DEFERRABLE;

ALTER TABLE ONLY public.spans
    ADD CONSTRAINT spans_schema_id_fkey FOREIGN KEY (schema_id) REFERENCES public.data_schemas(id);

ALTER TABLE ONLY public.spans
    ADD CONSTRAINT spans_source_event_id_fkey FOREIGN KEY (source_event_id) REFERENCES public.inbound_events(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.spans
    ADD CONSTRAINT spans_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.spending_policies
    ADD CONSTRAINT spending_policies_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.status_events
    ADD CONSTRAINT status_events_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.status_webhook_deliveries
    ADD CONSTRAINT status_webhook_deliveries_event_cursor_fkey FOREIGN KEY (event_cursor) REFERENCES public.status_events(cursor) ON DELETE CASCADE;

ALTER TABLE ONLY public.status_webhook_deliveries
    ADD CONSTRAINT status_webhook_deliveries_subscription_id_fkey FOREIGN KEY (subscription_id) REFERENCES public.status_webhook_subscriptions(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.status_webhook_secrets
    ADD CONSTRAINT status_webhook_secrets_subscription_id_fkey FOREIGN KEY (subscription_id) REFERENCES public.status_webhook_subscriptions(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.status_webhook_subscriptions
    ADD CONSTRAINT status_webhook_subscriptions_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.user_contexts
    ADD CONSTRAINT user_contexts_host_app_fkey FOREIGN KEY (deployment_id, host_app_id) REFERENCES public.host_apps(deployment_id, id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.user_contexts
    ADD CONSTRAINT user_contexts_organization_fkey FOREIGN KEY (deployment_id, host_app_id, organization_id) REFERENCES public.host_organizations(deployment_id, host_app_id, id) ON DELETE RESTRICT;

ALTER TABLE ONLY public.user_contexts
    ADD CONSTRAINT user_contexts_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.user_notifications
    ADD CONSTRAINT user_notifications_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.user_preferences
    ADD CONSTRAINT user_preferences_user_context_id_fkey FOREIGN KEY (user_context_id) REFERENCES public.user_contexts(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.verified_integration_events
    ADD CONSTRAINT verified_integration_events_execution_id_fkey FOREIGN KEY (execution_id) REFERENCES public.executions(id);

ALTER TABLE ONLY public.vox_connection_setups
    ADD CONSTRAINT vox_connection_setups_connection_id_fkey FOREIGN KEY (connection_id) REFERENCES public.vox_connections(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.vox_connection_setups
    ADD CONSTRAINT vox_connection_setups_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.vox_connections
    ADD CONSTRAINT vox_connections_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE public.action_approvals ENABLE ROW LEVEL SECURITY;

CREATE POLICY action_approvals_user_all ON public.action_approvals TO authenticated USING ((EXISTS ( SELECT 1
   FROM public.action_proposals p
  WHERE ((p.id = action_approvals.proposal_id) AND (p.user_id = auth.uid()))))) WITH CHECK ((EXISTS ( SELECT 1
   FROM public.action_proposals p
  WHERE ((p.id = action_approvals.proposal_id) AND (p.user_id = auth.uid())))));

ALTER TABLE public.action_proposals ENABLE ROW LEVEL SECURITY;

CREATE POLICY action_proposals_user_all ON public.action_proposals TO authenticated USING ((user_id = auth.uid())) WITH CHECK ((user_id = auth.uid()));

ALTER TABLE public.agent_delegation_permissions ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.audit_events ENABLE ROW LEVEL SECURITY;

CREATE POLICY audit_events_user_select ON public.audit_events FOR SELECT TO authenticated USING ((user_id = auth.uid()));

ALTER TABLE public.auth_identities ENABLE ROW LEVEL SECURITY;

CREATE POLICY auth_identities_user_select ON public.auth_identities FOR SELECT TO authenticated USING ((user_id = auth.uid()));

ALTER TABLE public.auth_sessions ENABLE ROW LEVEL SECURITY;

CREATE POLICY auth_sessions_user_select ON public.auth_sessions FOR SELECT TO authenticated USING (((user_id = auth.uid()) AND (revoked_at IS NULL) AND (expires_at > now())));

ALTER TABLE public.channel_identities ENABLE ROW LEVEL SECURITY;

CREATE POLICY channel_identities_user_select ON public.channel_identities FOR SELECT TO authenticated USING ((user_id = auth.uid()));

ALTER TABLE public.collection_spans ENABLE ROW LEVEL SECURITY;

CREATE POLICY collection_spans_user_all ON public.collection_spans TO authenticated USING ((user_id = auth.uid())) WITH CHECK ((user_id = auth.uid()));

ALTER TABLE public.collections ENABLE ROW LEVEL SECURITY;

CREATE POLICY collections_user_all ON public.collections TO authenticated USING ((user_id = auth.uid())) WITH CHECK ((user_id = auth.uid()));

ALTER TABLE public.conversations ENABLE ROW LEVEL SECURITY;

CREATE POLICY conversations_user_all ON public.conversations TO authenticated USING ((user_id = auth.uid())) WITH CHECK ((user_id = auth.uid()));

ALTER TABLE public.data_schemas ENABLE ROW LEVEL SECURITY;

CREATE POLICY data_schemas_user_delete ON public.data_schemas FOR DELETE TO authenticated USING ((user_id = auth.uid()));

CREATE POLICY data_schemas_user_insert ON public.data_schemas FOR INSERT TO authenticated WITH CHECK ((user_id = auth.uid()));

CREATE POLICY data_schemas_user_select ON public.data_schemas FOR SELECT TO authenticated USING (((user_id IS NULL) OR (user_id = auth.uid())));

CREATE POLICY data_schemas_user_update ON public.data_schemas FOR UPDATE TO authenticated USING ((user_id = auth.uid())) WITH CHECK ((user_id = auth.uid()));

ALTER TABLE public.devices ENABLE ROW LEVEL SECURITY;

CREATE POLICY devices_user_all ON public.devices TO authenticated USING ((user_id = auth.uid())) WITH CHECK ((user_id = auth.uid()));

ALTER TABLE public.execution_attempts ENABLE ROW LEVEL SECURITY;

CREATE POLICY execution_attempts_user_select ON public.execution_attempts FOR SELECT TO authenticated USING ((EXISTS ( SELECT 1
   FROM (public.executions e
     JOIN public.action_proposals p ON ((p.id = e.proposal_id)))
  WHERE ((e.id = execution_attempts.execution_id) AND (p.user_id = auth.uid())))));

ALTER TABLE public.executions ENABLE ROW LEVEL SECURITY;

CREATE POLICY executions_user_select ON public.executions FOR SELECT TO authenticated USING ((EXISTS ( SELECT 1
   FROM public.action_proposals p
  WHERE ((p.id = executions.proposal_id) AND (p.user_id = auth.uid())))));

ALTER TABLE public.inbound_events ENABLE ROW LEVEL SECURITY;

CREATE POLICY inbound_events_user_all ON public.inbound_events TO authenticated USING ((user_id = auth.uid())) WITH CHECK ((user_id = auth.uid()));

ALTER TABLE public.integration_codes ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.integration_grants ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.integration_plan_requests ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.integration_tokens ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.jobs ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.messages ENABLE ROW LEVEL SECURITY;

CREATE POLICY messages_user_delete ON public.messages FOR DELETE TO authenticated USING ((EXISTS ( SELECT 1
   FROM public.conversations c
  WHERE ((c.id = messages.conversation_id) AND (c.user_id = auth.uid())))));

CREATE POLICY messages_user_insert ON public.messages FOR INSERT TO authenticated WITH CHECK ((EXISTS ( SELECT 1
   FROM public.conversations c
  WHERE ((c.id = messages.conversation_id) AND (c.user_id = auth.uid())))));

CREATE POLICY messages_user_select ON public.messages FOR SELECT TO authenticated USING ((EXISTS ( SELECT 1
   FROM public.conversations c
  WHERE ((c.id = messages.conversation_id) AND (c.user_id = auth.uid())))));

CREATE POLICY messages_user_update ON public.messages FOR UPDATE TO authenticated USING ((EXISTS ( SELECT 1
   FROM public.conversations c
  WHERE ((c.id = messages.conversation_id) AND (c.user_id = auth.uid()))))) WITH CHECK ((EXISTS ( SELECT 1
   FROM public.conversations c
  WHERE ((c.id = messages.conversation_id) AND (c.user_id = auth.uid())))));

ALTER TABLE public.pending_phone_links ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.reminder_deliveries ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.reminders ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.schedules ENABLE ROW LEVEL SECURITY;

CREATE POLICY schedules_user_all ON public.schedules TO authenticated USING ((user_id = auth.uid())) WITH CHECK ((user_id = auth.uid()));

CREATE POLICY service_role_action_approvals ON public.action_approvals TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_action_proposals ON public.action_proposals TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_audit_events ON public.audit_events TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_auth_identities ON public.auth_identities TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_auth_sessions ON public.auth_sessions TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_channel_identities ON public.channel_identities TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_collection_spans ON public.collection_spans TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_collections ON public.collections TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_conversations ON public.conversations TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_data_schemas ON public.data_schemas TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_devices ON public.devices TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_execution_attempts ON public.execution_attempts TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_executions ON public.executions TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_inbound_events ON public.inbound_events TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_jobs ON public.jobs TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_messages ON public.messages TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_reminder_deliveries ON public.reminder_deliveries TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_reminders ON public.reminders TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_schedules ON public.schedules TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_spans ON public.spans TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_status_events ON public.status_events TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_status_webhook_deliveries ON public.status_webhook_deliveries TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_status_webhook_secrets ON public.status_webhook_secrets TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_status_webhook_subscriptions ON public.status_webhook_subscriptions TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_users ON public.users TO service_role USING (true) WITH CHECK (true);

CREATE POLICY service_role_verified_integration_events ON public.verified_integration_events TO service_role USING (true) WITH CHECK (true);

ALTER TABLE public.spans ENABLE ROW LEVEL SECURITY;

CREATE POLICY spans_user_all ON public.spans TO authenticated USING ((user_id = auth.uid())) WITH CHECK ((user_id = auth.uid()));

ALTER TABLE public.status_events ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.status_webhook_deliveries ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.status_webhook_secrets ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.status_webhook_subscriptions ENABLE ROW LEVEL SECURITY;

ALTER TABLE public.users ENABLE ROW LEVEL SECURITY;

CREATE POLICY users_select_own ON public.users FOR SELECT TO authenticated USING ((id = auth.uid()));

CREATE POLICY users_update_own ON public.users FOR UPDATE TO authenticated USING ((id = auth.uid())) WITH CHECK ((id = auth.uid()));

ALTER TABLE public.verified_integration_events ENABLE ROW LEVEL SECURITY;



SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

INSERT INTO public.platform_deployments VALUES ('64ed3cc0-9df9-40af-bcf7-b3cb026df051', 'vox.legacy.deployment', '2026-10-09 11:52:54.886944+00');
INSERT INTO public.platform_deployments VALUES ('11765161-5a20-4ec3-bfd6-3c00e7c494e6', 'vox.standalone.deployment', '2026-10-09 11:52:54.887451+00');

INSERT INTO public.agent_definitions VALUES ('fca67274-1b57-4292-8bf5-afcc8fbc81ee', '11765161-5a20-4ec3-bfd6-3c00e7c494e6', 'general', 'General-purpose Vox assistant for voice and messaging conversations.', '{*}', 'enabled', '2026-10-09 11:52:55.335664+00', '2026-10-09 11:52:55.335664+00', NULL, NULL, 'Assistant', false, 1);
INSERT INTO public.agent_definitions VALUES ('b4c88369-0abf-46ca-9f31-c3e2884ce0a1', '64ed3cc0-9df9-40af-bcf7-b3cb026df051', 'general', 'General-purpose Vox assistant for voice and messaging conversations.', '{*}', 'enabled', '2026-10-09 11:52:55.699191+00', '2026-10-09 11:52:55.699191+00', NULL, NULL, 'Assistant', false, 1);

INSERT INTO public.agent_model_configurations VALUES ('fd67b89e-b96d-4318-b021-fbc3d30fa33c', 'fca67274-1b57-4292-8bf5-afcc8fbc81ee', 1, 'gemini', 'gemini-3.5-flash-lite', '{}', '2026-10-09 11:52:55.336787+00');
INSERT INTO public.agent_model_configurations VALUES ('0216af1d-d083-48c2-826c-9013347691d0', 'b4c88369-0abf-46ca-9f31-c3e2884ce0a1', 1, 'gemini', 'gemini-3.5-flash-lite', '{}', '2026-10-09 11:52:55.700129+00');

INSERT INTO public.deployment_agent_selections VALUES ('11765161-5a20-4ec3-bfd6-3c00e7c494e6', 'fca67274-1b57-4292-8bf5-afcc8fbc81ee', 'fd67b89e-b96d-4318-b021-fbc3d30fa33c', '2026-10-09 11:52:55.337344+00');
INSERT INTO public.deployment_agent_selections VALUES ('64ed3cc0-9df9-40af-bcf7-b3cb026df051', 'b4c88369-0abf-46ca-9f31-c3e2884ce0a1', '0216af1d-d083-48c2-826c-9013347691d0', '2026-10-09 11:52:55.700517+00');

INSERT INTO public.host_apps VALUES ('21809325-6b3a-4feb-a01f-d9964982d0e3', '64ed3cc0-9df9-40af-bcf7-b3cb026df051', 'vox.legacy.channel-host', '2026-10-09 11:52:54.887178+00', '{}');
INSERT INTO public.host_apps VALUES ('d619e4f8-ebab-44e8-ba3a-95b1089359f5', '11765161-5a20-4ec3-bfd6-3c00e7c494e6', 'vox.standalone.web', '2026-10-09 11:52:54.887613+00', '{}');


SET search_path=public,pg_temp;
SET check_function_bodies=true;
SET row_security=on;
