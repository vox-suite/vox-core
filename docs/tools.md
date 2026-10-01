# Conversation tools

All conversation channels expose the same three tools. The selected agent and authenticated user context are supplied by Core, never taken from model tool arguments.

| Tool | Purpose | Authority |
| --- | --- | --- |
| `library` | Discover granted capability metadata, load pinned tools/skills, read, or propose a change | Rechecks owned-agent state, connection ownership and current access; proposals require a separate authenticated approval |
| `get_agent_memory` | Inspect this assistant's retained context | Bound to its agent and user context; reads current archive/retention state |
| `update_agent_memory` | Retain relevant assistant notes | Bound to the same owner and agent; retention settings are authoritative |

## Library operations

- `search`: query permitted metadata with bounded pagination. It does not inject every connected account's tool schema or every skill body.
- `load_tool`: load a reviewed tool schema by connection ID and tool name. A loaded schema does not confer execution authority.
- `load_skill`: load an enabled, pinned skill by ID. Guidance cannot grant access or approve a change.
- `read`: execute a permitted read with exact connection/tool arguments.
- `propose`: create an exact external-change proposal, including title and disclosure. This does not execute it. The model has no approval operation.

See [the implementation](../src/agents/tools/library.rs) for typed requests and response contracts. Human approval flows use the authenticated host boundary and bind the actual executing agent, connection, arguments and disclosure.

## Pre-launch transition

Direct conversational web/maps, terminal commands, phone scheduling, collection/schema/timeline operations and the intent-classifier tool router have been removed. These capabilities must return as reviewed integrations through the governed library. They are currently unavailable through conversation when no such integration is granted; explain the missing capability rather than inventing success.

Separate event/space planner tools and authenticated application/operator routes remain active and require their own governance review. Removing conversation registrations does not certify those paths. No retained memory, PostgreSQL records, device configuration or credentials are migrated by this refactor.

Track replacement capability delivery, scoped delegation and durable assigned work in https://github.com/vox-suite/vox-core/issues/101. Live provider acceptance and current deployment evidence are tracked in https://github.com/vox-suite/vox-deploy/issues/19.
