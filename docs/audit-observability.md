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

`POST /v1/admin/audit-events` requires `VOX_ADMIN_TOKEN`, returns `Cache-Control: no-store`, and records every
successful read as `audit.accessed`. Audit events are privileged operational
evidence, not a user-facing task-history interface.

## Agent traces (Langfuse)

Setting `LANGFUSE_PUBLIC_KEY` and `LANGFUSE_SECRET_KEY` exports agent traces to
Langfuse over OTLP (`LANGFUSE_BASE_URL`, EU cloud by default). Each conversation
turn is one trace whose root `agent` observation is named after the agent that
ran (`shopping-agent`, `maps-agent`, ...), tagged with its channel, and grouped
into a Langfuse session per conversation. Every LLM call (model, tokens,
latency) and tool call (named after the tool, with its outcome) nests under it.
The background agents (`summarizer-agent`, `sms-extractor-agent`,
`event-planner-agent`, `task-executor-agent`) produce their own traces.
`LANGFUSE_TRACING_ENVIRONMENT` and `LANGFUSE_RELEASE` set the Langfuse
environment and release on every trace.

Only agent spans are exported, never log events. By default the spans carry no
conversation text. `LANGFUSE_RECORD_CONTENT=true` adds prompts, replies, and
tool arguments/results, which include callers' personal details and make
Langfuse a disclosed sink for conversation text; agent spans are kept out of
console logs so that text reaches Langfuse only.

## Process diagnostics

Classifier routing logs contain the selected domain and confidence, never the
user prompt. Provider failures retain HTTP status without retaining or logging
the response body, which may echo user content or credentials. Schema lookup
failures use a dedicated content-free error.

Native tool diagnostics omit user-supplied queries, route addresses, note and
collection titles, schema names, call reasons and phone numbers. Trusted record
references, tool names, duration and outcomes remain available for diagnosis.
This is separate from opt-in agent content tracing; enabling content tracing
does not add these values back to process logs. These checks do not certify
all provider SDK or channel-adapter diagnostics as content-free.

## Identity cache and conversation memory

Redis identity synchronization stores minimal routing metadata only. Conversation recaps stay in PostgreSQL and are loaded through the agent/context ownership and retention boundary; the unused user-wide recap projection has been removed. This also removes the full conversation-summary scan from each identity synchronization.

A deployment that ran the recap projection may still retain existing `vox:recaps:*` Redis entries. Code removal stops new copies but does not certify those old entries are gone. Operators must remove only that retired key family from the affected Redis instance through an approved protected maintenance session and retain a count-only verification. Preserve identity keys, queues and PostgreSQL conversation records. Track current Railway cleanup evidence in https://github.com/vox-suite/vox-deploy/issues/19.
