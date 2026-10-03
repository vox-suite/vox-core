# Connection contract status

A signed host assertion identifies the host and user context. It cannot prove
external-service authorization, account identity, scopes, or credential custody.
The former generic initiation, callback, and host-asserted authorization routes
have been removed because they had no provider-verifying adapter. The unused
generic authorization-session table is dropped by a forward migration.

## Public behavior

- MCP remote extensions use `POST /v1/remote-extensions/{id}/authorize` and
  `POST /v1/connected-apps/callback`. The provider OAuth exchange uses PKCE,
  single-use state, host-context binding, and endpoint/version binding. OAuth
  completion records encrypted tokens and reported tools and creates one
  context-owned `external_connections` record. Its authorized capabilities are
  the intersection of the declared capabilities and reported tool names. It
  does not grant an agent access or mark the extension conformant. The account
  identity is not provider verified, so no account name is invented.
- `POST /v1/connected-apps/read` takes a signed host context, selected agent,
  connection ID, declared read tool name, and JSON object arguments. Core
  rechecks the agent grant and connection at call time. A consequential tool is
  rejected on this route.
- `POST /v1/connected-apps/execute` takes a signed host context, approval ID,
  and idempotency key. It accepts only a remote-extension proposal whose exact
  approved details include a currently declared consequential
  `invocation.tool_name` and object `invocation.arguments`. Core consumes the approval into a durable execution
  before dispatch. A duplicate key returns the existing execution. Ambiguous
  provider results stay `reconciling`; a connector's own confirmation claim
  cannot mark the action succeeded.
- `POST /v1/remote-extensions/{id}/renew-consent` requires a signed host context,
  exact immutable version, and explicit confirmation. The Web review shows the
  current endpoint, operator, recipients, access needs, and capabilities.
  Updating an extension revoked its prior credentials and grants, so the user
  must reconnect and regrant after consenting.
- `POST /v1/connections/list` returns at most 100 `external_connections` records
  owned by the signed host user context. `POST /v1/connections/{id}/disconnect`
  revokes a record and its grants in one transaction. No public generic route
  currently creates an authorized connection.
- PlayStation uses `/v1/playstation/link` with a Sony NPSSO session token, verified account identity and explicit capture consent. Credentials are encrypted with `VOX_CREDENTIAL_KEY`. `/v1/playstation/{id}/status`, `/capture` and `/sync` are signed, context-owned operations. Core polls enabled accounts once a day, establishes a baseline, then records observed playtime increases with provider last-played timestamps and source-event provenance. Exact session boundaries remain unknown. Pausing clears the baseline; disconnecting deletes credentials. See [PlayStation capture](playstation.md).

## Release gates

The governed OAuth connection, selected-agent grant, exact proposal approval,
and durable execution path are wired. The Web catalog now discovers immutable deployment-reviewed manifests through
the package list/install interface. One click installs the reviewed digest
and begins OAuth; account linking still requires an explicit agent grant. Authors must declare and review tool effects; discovered MCP
annotations are not authority. Provider-backed account identity, actual
scopes, provider-side revocation, independent conformance, and a common MCP
protocol contract require validation. See the
[architecture review](../../vox-connections/docs/architecture-review-2026-09-27.md).

## Reviewed package catalog

The operator token protects `/v1/connector-packages/publish` and `/withdraw`.
Fresh signed host assertions protect `/list` and `/install`; deployment scope
comes from the verified context. Installation takes `external_key`, `version`
and the exact reviewed `digest`, creates no agent grants, and commits the
extension and catalog binding atomically. Operators may attest verified
read-only packages; consequential packages require separate behavioral
conformance and enablement. Withdrawal clears credentials/sessions and revokes
bound accounts/grants. See the
[package onboarding guide](../../vox-connections/docs/packages.md).
