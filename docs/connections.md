# Connection contract status

Core issue `vox-core#10` was reopened on 2026-09-23. The accepted E13 journey is
not yet complete. A signed host assertion proves which host and user context
made a request; it does not prove external-service authorization, actual account
identity, or credential custody. The original public
`POST /v1/connections/authorize` endpoint accepted all of those as host-supplied
claims. That is insufficient for a production connection.

## Current public behavior

- `POST /v1/connections/initiate` and `POST /v1/connections/callback` now
  return `503`. No provider authorization adapter is configured, so Core refuses
  to invent an authorization URL or accept a caller-supplied account identity.
- Internal connection records no longer synthesize a `vault-<id>` secret
  reference. The legacy reference remains empty until real credential custody
  exists.

- `POST /v1/connections/authorize` also returns `503`. Host-supplied account
  identity, scopes, custody, and lifecycle states are no longer accepted on
  any public connection creation path.
- `POST /v1/connections/list` takes a signed host assertion and
  `{"host_context": {"host_user_id": "...", "organization_external_key": null}}`.
  It returns at most 100 connection records owned by that exact user context.
  The schema stores only an account-reference hash, so it cannot show a verified
  account name or identifier.
- `POST /v1/connections/{id}/disconnect` accepts the same signed context body
  and returns the revoked record. It is idempotent for the owner, returns `404`
  for a different context, clears authorized capabilities, revokes grants for
  that connection in the same transaction, and blocks new Core execution
  attempts. Reconnection cannot revive old grants. It does not assert
  external-provider revocation or undo
  in-flight work. External-operator access may remain.

These routes use fresh host assertions and the existing replay, audience,
origin, and user-context checks. The host credential stays server-side.

## Required before accepting E13 and starting Web E17

1. Implement a provider-verified authorization initiation and callback path
   with state binding, expiry, replay protection, and account identity from the
   provider, rather than a host-authored authorization result.
2. Implement actual platform-held credential custody where a provider supports
   it, or disclose external-operator custody and its weaker revocation
   guarantee. Never claim platform custody without stored protected credentials.
3. Return a safe, provider-verified account display identifier, requested
   access, actual authorized capabilities, data recipients, limitations, and
   operator metadata through the public contract.
4. Add reconnect, provider expiry/revocation, callback cancellation, and
   service-side revocation tests. Preserve truthful in-flight behavior and
   historical references.
5. Run the PostgreSQL lifecycle and cross-context tests on an isolated database
   and provide the consumer-facing contract packet required by the execution
   order. Only then resume `vox-web#2`.

The existing Rust `ConnectionService::record` is an internal primitive used by
tests and adapter code. Its caller must supply evidence from a verified provider
flow before recording `authorized`; the public host route does not expose that
transition.
