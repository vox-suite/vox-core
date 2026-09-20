# Identity adapters and linking

Core is not an identity provider. A host first proves which canonical user
context it is acting for through the signed host-context contract. An enabled
identity adapter can then authenticate an identity **inside that exact context**.
This makes standalone and embedded hosts replaceable without allowing an email,
provider subject, or recovery handle to silently join two contexts.

## Adapter boundary

`POST /v1/identity-adapters` is an operator endpoint and requires the Core
service bearer token. It registers an adapter under one deployment. The initial
adapter kinds are:

- `federated_ed25519`: a configured issuer, audience, and Ed25519 public key.
  Each assertion binds issuer, audience, subject, issue time, expiry, and a
  nonce. Core accepts only signatures from that configured key, short-lived
  assertions, and nonces not previously used for that adapter.
- `passwordless_recovery`: a named recovery channel. A host supplies a signed
  user context to start recovery. Core hashes the recovery handle and generated
  code, asks the configured delivery implementation to send the raw code, and
  deletes the challenge if delivery fails. The default server delivery is
  unavailable until an operator supplies a real channel implementation.

Neither configuration stores provider secrets. Raw recovery codes, federated
proofs, host credentials, and authentication tokens must not be logged.

## Public flow

`POST /v1/identity/passwordless/challenges` and
`POST /v1/identity/authentications` each require a fresh signed host-context
assertion in the same headers used by `/v1/host/context`, plus a body containing
the host context and the adapter request. The assertion is consumed by Core, so
reusing it is rejected. A successful authentication returns an opaque,
ten-minute, one-time authentication token. Only its SHA-256 hash is stored.

`POST /v1/identity/links` consumes two such tokens. `DELETE /v1/identity/links`
consumes two fresh tokens and removes that association. Both operations require
two distinct verified login identities; they leave an append-only link/unlink
event.

## Deliberate non-merging rule

An identity link is an association between two `login_identities`, not an update
to `user_contexts`. It never transfers or combines host-app identity, user
contexts, connections, grants, preferences, conversations, tasks, actions, or
history. Consumers must explicitly choose how an active link is presented; Core
continues enforcing authority using the original context.

Adapter change is additive: register a new external adapter key and migrate a
host to it before retiring the old one. No adapter migration deletes identities,
contexts, or link evidence.

## Verification evidence

On 2026-09-20, the identity-adapter migration and integration suite ran against
an isolated PostgreSQL 17 database. It exercised operator registration,
fresh signed host-context authentication, federated proof replay rejection,
passwordless single-use recovery, adapter replacement, link/unlink audit
events, and preservation of both linked user contexts.
