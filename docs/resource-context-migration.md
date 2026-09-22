# Canonical resource ownership

Platform V1 resources are owned exclusively by an authenticated canonical user
context. `ResourceOwner` carries the context ID and its internal user ID from
the HTTP trust boundary into Core services. Conversation, schedule, event,
task, and outbound-call APIs never derive ownership from channel, phone,
provider conversation ID, names, or voice recognition.

## Final wire contract

Host-facing conversation, schedule, and event requests contain a signed
`host_context`. Conversation requests additionally carry `channel` and a
provider conversation identifier as presentation/correlation metadata. Core
resolves the assertion once and supplies `ResourceOwner` to the service. A
service token, `identity` object, or channel-only request receives `401` before
responder, memory, worker, or database mutation.

`VOX_CORE_SERVICE_TOKEN` remains only for explicit operator-controlled
registration and administration endpoints. It grants no user-resource
authority.

## Schema contraction

Migration `20260922000000_contract_legacy_identity.sql` is the pre-launch clean
break. It:

- requires non-null context ownership for conversations, schedules, tasks,
  events, outbound calls, and contact points;
- scopes event idempotency by context;
- renames `user_identities` to `user_contact_points` and scopes each contact by
  context; contact records are delivery metadata, never login authority;
- removes the null-context conversation index and reserved legacy deployment
  and host; and
- aborts with a rebuild diagnostic when legacy-owned records remain.

Historical migrations are immutable. Vox has no deployed customer data, so an
affected developer or test database must be rebuilt from a clean baseline.
There is no rolling compatibility mode or data-preserving fallback.

## Isolation and recovery rules

- A conversation key is `(user_context_id, channel, external_id)`.
- Event idempotency is `(user_context_id, idempotency_key)`.
- Schedule and task reads/writes require the exact owner.
- Equal phone/channel values in different contexts remain separate.
- Voice, name, or spoken-phone mismatch cannot switch owners; it returns a
  non-disclosing verification response.
- Cross-channel linking exists only through the proof-based identity-adapter
  contract.

Rollback restores the last application revision only after rebuilding its
database schema. Never reintroduce nullable authority columns or a channel
identity fallback.
