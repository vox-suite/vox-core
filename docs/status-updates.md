# Durable status updates

Status events are authenticated, user-context-scoped change hints. They are
not authoritative state and must never be used to infer that a provider action
has succeeded. After every hint, a host fetches the task or execution through
the existing authenticated Core route.

## Cursor recovery

`POST /v1/status-events` accepts the authenticated `host_context`, an optional
`after` cursor, and a bounded `limit` (1–100). It returns events strictly after
the cursor plus `next_cursor`. Cursors are monotonically increasing database
positions, not a promise of contiguous user-specific sequence numbers. A host
persists the last fully processed cursor and may safely replay a page after a
crash. It must tolerate duplicates, delayed hints, and reordered hints.

Events contain only aggregate type/ID, transition type/state, timestamps, and
an empty-or-redacted operational payload. They never contain task content,
credentials, raw provider bodies, payment details, or model reasoning.

## HTTPS webhook hints

Hosts create and inspect subscriptions at
`/v1/status-webhook-subscriptions`; they can rotate the signing secret via
`/{id}/rotate` and disable the subscription with `DELETE /{id}`. Core returns a
secret only at creation and rotation. The self-hosted store encrypts it with
AES-256-GCM before PostgreSQL storage, using `VOX_STATUS_WEBHOOK_KEY` (32 random
bytes encoded as 64 hexadecimal characters) supplied to both Core API and
worker from the deployment secret manager. Generate the key with
`openssl rand -hex 32`; retain it across restarts and backups. Key loss makes
existing webhook subscriptions unusable; rotation requires a planned
re-encryption migration. Without a configured key, subscription creation fails
closed and cursor polling remains available.

Subscription responses include `secret_version`. Creation, rotation, and
disable commit the subscription state and encrypted secret together. Concurrent
rotations serialize on the subscription row; the highest committed version is
the current secret. A delivery already in flight may use the previous secret.
If the host has stopped accepting it, the rejected delivery is retried with
the current secret and the same delivery ID. The host should retain its prior
verification key briefly during planned rotation, but always cursor-poll for
authoritative state. Disable removes the stored secret and stops new delivery
claims; an already sent request cannot be recalled.

Deliveries are durable outbox jobs and are at-least-once. Each request has:

- `X-Vox-Signature-Version: v1`
- `X-Vox-Delivery-Id`: a stable UUID for that delivery
- `X-Vox-Timestamp`: Unix seconds
- `X-Vox-Signature`: lowercase hex HMAC-SHA-256 of
  `timestamp + "." + exact_request_body`, using the current subscription
  secret.

The JSON body is deliberately minimal: a version, delivery/cursor identity,
aggregate identity, transition metadata, and `authoritative: false`. Hosts
should reject stale timestamps, verify the signature against the exact bytes,
deduplicate delivery IDs, and then cursor-poll Core for authoritative state.
The worker uses bounded exponential retry, does not follow redirects, resolves
and pins only public destination addresses, and marks a subscription `unhealthy`
after eight failed attempts. The delivery ID stays stable across retries.
Polling stays available for recovery; disabling a subscription stops future
enqueueing and removes its signing secret. A request already in flight may
finish after disable, so hosts still verify signatures and retrieve current
state rather than treating a hint as authority.

## Provider-originated events

There is no generic public “event callback” API. A provider adapter must
verify the raw request before constructing a `VerifiedIntegrationEvent` and
calling `StatusService::apply_verified_external_event`. This module defines
the `IntegrationExternalEventVerifier` contract but does not wire a production
provider verifier. Core checks the exact existing execution, integration,
enabled registration, connection, and user context in one transaction; it
persists the provider event’s replay identity before applying the normalized
outcome through `ExecutionCoordinator`. Provider-specific ingestion and
reordered-event acceptance remain open E25 work.

Provider events cannot create a task, run, proposal, approval, grant,
connection, or follow-up execution. Consequential follow-up remains subject to
the normal grant, policy, proposal, and approval path. Raw provider requests
are never stored in the status/outbox ledger.

## Operations

Run `StatusService::delivery_worker()` from the deployment worker process with
a stable worker identity. It leases one row at a time, so concurrent workers
cannot dispatch the same row simultaneously; a lost lease is retried with the
same delivery ID. Operators should alert on unhealthy subscriptions and retain
cursor polling as the recovery path. This module provides operational delivery
metadata only; long-term audit retention belongs to the audit subsystem.
