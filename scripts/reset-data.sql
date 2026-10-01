BEGIN;

TRUNCATE
    action_approvals,
    action_proposals,
    agent_capability_grants,
    agent_memories,
    assigned_task_runs,
    audit_events,
    auth_identities,
    auth_sessions,
    channel_identities,
    chart_boards,
    charts,
    collections,
    collection_spans,
    connector_package_installations,
    connector_setups,
    connector_skill_installations,
    connector_tool_metadata,
    conversations,
    data_source_consents,
    devices,
    event_agent_decisions,
    execution_attempts,
    executions,
    external_connections,
    federated_identity_nonces,
    host_app_assertion_nonces,
    identity_authentication_sessions,
    identity_link_events,
    identity_links,
    inbound_events,
    jobs,
    login_identities,
    mcp_authorization_sessions,
    messages,
    operational_quota_reservations,
    operational_quotas,
    passwordless_recovery_challenges,
    phone_verifications,
    portable_exports,
    reminder_deliveries,
    reminders,
    remote_extension_conformance_runs,
    remote_extension_credentials,
    remote_extensions,
    remote_extension_versions,
    schedule_occurrence_dispatches,
    schedules,
    skill_agent_enablements,
    skill_installations,
    space_edges,
    space_messages,
    space_nodes,
    spaces,
    spans,
    spending_policies,
    status_events,
    status_webhook_deliveries,
    status_webhook_secrets,
    status_webhook_subscriptions,
    user_notifications,
    user_preferences,
    verified_integration_events;

DELETE FROM deployment_agent_selections
WHERE agent_definition_id IN (SELECT id FROM agent_definitions WHERE owner_user_context_id IS NOT NULL);

DELETE FROM agent_model_configurations
WHERE agent_definition_id IN (SELECT id FROM agent_definitions WHERE owner_user_context_id IS NOT NULL);

DELETE FROM agent_instruction_versions
WHERE agent_id IN (SELECT id FROM agent_definitions WHERE owner_user_context_id IS NOT NULL);

DELETE FROM agent_definitions WHERE owner_user_context_id IS NOT NULL;

DELETE FROM skill_package_versions
WHERE skill_id IN (SELECT id FROM skill_packages WHERE owner_user_context_id IS NOT NULL);

DELETE FROM skill_packages WHERE owner_user_context_id IS NOT NULL;

DELETE FROM data_schemas WHERE user_id IS NOT NULL OR user_context_id IS NOT NULL;

DELETE FROM user_contexts;

DELETE FROM users;

COMMIT;
