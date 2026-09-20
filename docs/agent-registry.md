# Model-neutral agent registry

The agent registry is Core's reusable catalog seam. It records what an agent is
for and the capability categories it may later request; it does not run a model,
hold an account, create a connection, grant a capability, or approve an action.

An operator registers a definition with a deployment-scoped external key,
human-readable purpose, and declared requested capability categories. Selecting
an agent stores a separate, versioned model configuration containing only an
adapter label, model label, and non-secret JSON tuning claims. Core deliberately
does not interpret those claims as provider credentials or authority.

`POST /v1/agent-definitions`, `POST /v1/agent-selections`, and
`GET /v1/deployments/{external_key}/agents` require the Core operator service
token. A deployment may select multiple agents. Re-selecting an agent creates a
new model-configuration version while retaining its definition unchanged.

Model configuration rejects keys that look like credentials (`secret`, `token`,
`password`, or `credential`) at every JSON nesting level. Authentication,
connections, capability grants, approvals, and policy are owned by subsequent
platform contracts, so a registry record has no authority by itself. See
[`capability-grants.md`](capability-grants.md) for the explicit, context-bound
authority contract.
