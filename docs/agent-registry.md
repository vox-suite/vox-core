# Model-neutral agent registry

## Templates and owned assistants

Deployment definitions with no `owner_user_context_id` are operator-managed
templates. Context-owned definitions are configured assistants. They share the
registry representation, but only owned assistants may receive connection grants
or skill enablements. Names are presentation; stable keys identify agents within
their authenticated user context.

`POST /v1/agents/selected` idempotently provisions and returns the context's
Personal Assistant and active specialists. `POST /v1/agents/manage` accepts
authenticated host assertions and a `mutation` tagged as `create`, `update` or
`archive`. Creation accepts a name and instructions, using the deployment's
selected `general` template/model configuration. Update requires the current
`expected_version`; instruction history is retained. Archive blocks future use
and revokes grants and skill enablements. The default cannot be archived.

Owned instances start without grants or enabled skills. A disabled template also
blocks its owned instances. The pre-launch ownership migration clears old
template-based grants and skill enablements rather than inferring renewed consent.
Unused template-era proposals expire and undispatched executions fail during this transition. Completed or uncertain outcomes retain their evidence. Provider credentials remain separate.

The agent-facing library searches bounded metadata and loads selected schemas
or skill guidance separately. Search and loading never authorize execution;
calls recheck current grants, connectivity and policy. Search is currently
lexical over the permitted inventory; an indexed implementation and larger
inventory pagination remain follow-up work.

The agent registry is Core's reusable catalog seam. It records what an agent is
for and the capability categories it may later request; it does not run a model,
hold an account, create a connection, grant a capability, or approve an action.

An operator registers a definition with a deployment-scoped external key,
human-readable purpose, and declared requested capability categories. Selecting
an agent stores a separate, versioned model configuration containing only an
adapter label, model label, and non-secret JSON tuning claims. Core deliberately
does not interpret those claims as provider credentials or authority.

`POST /v1/agent-definitions`, `POST /v1/agent-selections`, and
`POST /v1/deployments/{external_key}/agents` require the Core operator service
token. A deployment may select multiple agents. Re-selecting an agent creates a
new model-configuration version while retaining its definition unchanged.

Model configuration rejects keys that look like credentials (`secret`, `token`,
`password`, or `credential`) at every JSON nesting level. Authentication,
connections, capability grants, approvals, and policy are owned by subsequent
platform contracts, so a registry record has no authority by itself. See
[`capability-grants.md`](capability-grants.md) for the explicit, context-bound
authority contract.
