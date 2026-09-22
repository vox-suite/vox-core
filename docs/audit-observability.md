# Audit evidence and observability

Core stores versioned audit evidence locally and append-only by default. Audit
is not a second task-history store: it records authority and execution facts,
not conversation text, private reasoning, credentials, payment credentials, or
complete provider payloads.

Each event identifies its user context where applicable, actor category,
aggregate and authority references, timestamp, and a strict structured detail
object. Proposal evidence records capability, provider/model identifiers,
hashed account reference, connection, price/currency, expiry, and integrity
hashes only. The audit schema is versioned; retention and deletion are handled
by the later lifecycle contract.

External observability is disabled until an operator configures a disclosed
sink. Export uses a durable, redacted outbox; a failed sink is retried and may
become unhealthy, but it never blocks Core work and never causes unredacted
fallback logging. Sink credentials remain in deployment secret custody, never
in Core's database or audit records.

`GET /v1/admin/audit-events` requires `VOX_ADMIN_TOKEN` (same credential as
the Redis admin routes), returns `Cache-Control: no-store`, and records every
successful read as `audit.accessed`. Audit events are privileged operational
evidence, not a user-facing task-history interface.
