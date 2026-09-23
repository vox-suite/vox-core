# Vox Core API endpoint inventory

Source snapshot: `src/http/mod.rs`, HEAD `c099d94`, inspected 2026-09-22. **49 method/path operations**. This is a source-derived export, not a live-server audit or a complete OpenAPI schema. Handler implementations determine current authentication; proposed public access must not be inferred from this list.

All routes must enter the generated OpenAPI contract during the reorganization, including service/admin endpoints with their distinct security requirements. Preserve current methods (including POST reads) in compatibility routes until callers migrate.

| Method | Path | Current handler |
| --- | --- | --- |
| GET | `/v1/admin/redis` | `admin::browse` |
| DELETE | `/v1/admin/redis` | `admin::delete` |
| PUT | `/v1/admin/redis` | `admin::update` |
| GET | `/v1/admin/audit-events` | `audit::list` |
| GET | `/health/live` | `live` |
| GET | `/health/ready` | `ready` |
| POST | `/v1/conversations/respond` | `conversations::respond` |
| POST | `/v1/action-proposals` | `approvals::propose` |
| POST | `/v1/spending-policies` | `execution_policy::set_spending_policy` |
| POST | `/v1/operational-quotas` | `execution_policy::set_operational_quota` |
| POST | `/v1/action-proposals/{id}/approve` | `approvals::approve` |
| POST | `/v1/durable-tasks` | `durable_tasks::start` |
| POST | `/v1/durable-tasks/{id}` | `durable_tasks::get` |
| POST | `/v1/durable-tasks/{id}/wait` | `durable_tasks::wait` |
| POST | `/v1/durable-tasks/{id}/resume` | `durable_tasks::resume` |
| POST | `/v1/durable-tasks/{id}/cancel` | `durable_tasks::cancel` |
| POST | `/v1/conversations/speculate` | `conversations::speculate` |
| POST | `/v1/conversations/respond/stream` | `conversations::respond_stream` |
| POST | `/v1/conversations/complete` | `conversations::complete` |
| POST | `/v1/connections/authorize` | `connections::authorize` |
| POST | `/v1/capability-grants` | `capability_grants::create` |
| DELETE | `/v1/capability-grants` | `capability_grants::revoke` |
| POST | `/v1/agents/{external_key}/effective-capability-grants` | `capability_grants::effective` |
| POST | `/v1/events` | `events::ingest` |
| POST | `/v1/schedules` | `schedules::create` |
| PATCH | `/v1/schedules/{id}` | `schedules::update` |
| POST | `/v1/executions` | `execution::start` |
| POST | `/v1/executions/{id}` | `execution::get` |
| POST | `/v1/status-events` | `status::list` |
| POST | `/v1/status-webhook-subscriptions` | `status::create_subscription` |
| GET | `/v1/status-webhook-subscriptions` | `status::list_subscriptions` |
| POST | `/v1/status-webhook-subscriptions/{id}/rotate` | `status::rotate_subscription` |
| DELETE | `/v1/status-webhook-subscriptions/{id}` | `status::disable_subscription` |
| POST | `/v1/agent-definitions` | `agent_registry::register` |
| POST | `/v1/agent-selections` | `agent_registry::select` |
| POST | `/v1/agent-definitions/enabled` | `agent_registry::set_enabled` |
| POST | `/v1/integrations` | `integration_registry::register` |
| POST | `/v1/integrations/enabled` | `integration_registry::set_enabled` |
| GET | `/v1/deployments/{external_key}/capabilities` | `integration_registry::discover` |
| GET | `/v1/deployments/{external_key}/agents` | `agent_registry::list_selected` |
| POST | `/v1/host-apps` | `host_apps::register` |
| POST | `/v1/identity-adapters` | `identity_adapters::register` |
| POST | `/v1/identity/passwordless/challenges` | `identity_adapters::start_passwordless_recovery` |
| POST | `/v1/identity/authentications` | `identity_adapters::authenticate` |
| POST | `/v1/identity/links` | `identity_adapters::link` |
| DELETE | `/v1/identity/links` | `identity_adapters::unlink` |
| POST | `/v1/host-apps/{id}/credentials` | `host_apps::rotate_credential` |
| DELETE | `/v1/host-app-credentials/{id}` | `host_apps::revoke_credential` |
| POST | `/v1/host/context` | `host_apps::resolve_context` |

Not present in this router: ordinary task/project/record/schema CRUD; consumer Google sign-in/session exchange; device registration and job leases; `/v1/actions/{id}/result` used by Bridge. These are planned additions or contract gaps, not existing endpoints.
