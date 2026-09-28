-- A connection has one authoritative record. Existing dual-written ids in
-- action proposals and executions already match external_connections ids.
ALTER TABLE action_proposals DROP CONSTRAINT IF EXISTS action_proposals_connection_id_fkey;
ALTER TABLE action_proposals DROP CONSTRAINT IF EXISTS action_proposals_connection_owner_fk;
ALTER TABLE executions DROP CONSTRAINT IF EXISTS executions_connection_id_fkey;
ALTER TABLE executions DROP CONSTRAINT IF EXISTS executions_connection_owner_fk;
ALTER TABLE operational_quotas DROP CONSTRAINT IF EXISTS operational_quotas_connection_id_fkey;
ALTER TABLE agent_capability_grants DROP CONSTRAINT IF EXISTS agent_capability_grants_connection_id_fkey;

ALTER TABLE external_connections
    ADD CONSTRAINT external_connections_id_context_key UNIQUE (id, user_context_id);

ALTER TABLE action_proposals
    ADD CONSTRAINT action_proposals_connection_context_fk
    FOREIGN KEY (connection_id, user_context_id)
    REFERENCES external_connections(id, user_context_id)
    ON DELETE SET NULL (connection_id);
ALTER TABLE executions
    ADD CONSTRAINT executions_connection_context_fk
    FOREIGN KEY (connection_id, user_context_id)
    REFERENCES external_connections(id, user_context_id)
    ON DELETE SET NULL (connection_id);
ALTER TABLE operational_quotas
    ADD CONSTRAINT operational_quotas_connection_context_fk
    FOREIGN KEY (connection_id, user_context_id)
    REFERENCES external_connections(id, user_context_id) ON DELETE CASCADE;
ALTER TABLE agent_capability_grants
    ADD CONSTRAINT agent_capability_grants_connection_context_fk
    FOREIGN KEY (connection_id, user_context_id)
    REFERENCES external_connections(id, user_context_id) ON DELETE RESTRICT;

DROP TABLE connections;
