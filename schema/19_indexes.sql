CREATE INDEX jobs_claimable_idx ON jobs (next_attempt_at, created_at)
    WHERE state = 'pending';
CREATE INDEX jobs_expired_lease_idx ON jobs (lease_expires_at)
    WHERE state = 'running';
CREATE UNIQUE INDEX jobs_summary_unique_idx ON jobs (payload_reference_id)
    WHERE kind = 'summarize_conversation';
CREATE INDEX scheduled_tasks_due_idx ON scheduled_tasks (next_run_at)
    WHERE state = 'active';
CREATE INDEX messages_conversation_idx ON messages (conversation_id, sequence_number);
CREATE INDEX summaries_user_created_idx ON conversation_summaries (user_id, created_at DESC);
CREATE INDEX projects_user_status_idx ON projects (user_id, status);
CREATE INDEX tasks_user_status_idx ON tasks (user_id, status);
CREATE INDEX tasks_project_idx ON tasks (project_id);
CREATE INDEX user_goals_user_domain_idx ON user_goals (user_id, domain, status);
CREATE INDEX user_records_domain_idx ON user_records (user_id, domain, occurred_at DESC);
CREATE INDEX user_records_gin_data ON user_records USING gin (data);
CREATE INDEX user_records_user_schema_idx ON user_records (user_id, schema_id, occurred_at DESC);
CREATE INDEX data_schemas_lookup_idx ON data_schemas (user_id, namespace, name);
CREATE INDEX data_schemas_gin_schema ON data_schemas USING gin (json_schema);
CREATE INDEX user_insights_user_idx ON user_insights (user_id, domain, outcome_status);
CREATE INDEX client_devices_user_active_idx ON client_devices (user_id, is_active);
CREATE INDEX actions_user_state_idx ON actions (user_id, state);
