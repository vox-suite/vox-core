# Unified span storage + generalized classify/extract ingestion

Date: 2026-09-29
Repos touched: vox-core. vox-android is referenced for the GPS source's
shape but its segmentation algorithm is a separate, follow-up spec.

## Problem

vox-core has two parallel systems for storing the same kind of thing:

1. **`spans`** (`domain/spans.rs`, `storage/spans.rs`) — free-text
   `category`, JSONB `data`. This is what's actually in production use
   today: SMS ingestion (`sms_ingestion/handler.rs`) writes finance/bill
   facts here via a hardcoded, fixed-shape extractor
   (`agents/sms_extractor.rs`), and tasks/calendar events live here too.
2. **`records` + `data_schemas`** (`domain/records.rs`,
   `domain/schemas.rs`, `agents/tools/records.rs`) — schema-validated
   JSONB facts, with agent tools (`define_data_schema`,
   `create_user_record`, `list_data_schemas`, `list_user_records`,
   `manage_user_goal`) wired into the conversational agent
   (`agents/conversation.rs:310,411,483-499`). Nothing feeds this
   automatically — it's reactive to conversation only. `records.kind`
   (`fact`/`goal`/`insight`) backs three compatibility views
   (`user_records`, `user_goals`, `user_insights`,
   `migrations/20260923000000_initial_core.sql:419-450`).

There's no ingestion path for GPS, health, or other data sources, and no
generalized way to add one — each new source would mean another
hardcoded extractor like the SMS one. At "hundreds of users" scale, a
single growing `spans` table with today's indexes won't hold up for
either timeline rendering or future analytics.

SMS ingestion also duplicates infrastructure that already exists
generically: `sms_batches`/`sms_processed`
(`migrations/20260928000009_sms_processing.sql`) are a bespoke intake +
dedup ledger, but `inbound_events` (`payload_hash`, a unique constraint
on `(source_kind, source_id, external_event_id)`, `processed_at`,
`processing_error`) already does the same job generically, and already
auto-enqueues a `jobs(kind = 'process_event')` row on insert
(`events/service.rs:56-79`). That job is already picked up by
`events/handler.rs`, which already calls into a `jev` module
(`src/jev/event_triage.rs`, `src/jev/schema_classifier.rs`) — a cheap
external classifier (`JevClient`, gated on `JEV_API_KEY`) that triages
an event (ignore / store / plan-action) and matches it against
`data_schemas`, falling back to `NOVEL_CATEGORY_SENTINEL` when nothing
fits. This is System 1. The only missing piece is System 2: when Jev
says novel, `events/handler.rs:146-153` just logs and returns — no
Gemini call, no schema creation. The "Existing" match branch also still
inserts into `records` (`events/handler.rs:125-137`), which this design
removes.

## Design

### 1. Collapse `records` into `spans`

- Add `schema_id UUID NULL REFERENCES data_schemas(id)` to `spans`
  (nullable — manually created tasks etc. have no schema).
- Add `spans_user_schema_idx ON spans (user_id, schema_id, start_at DESC)`.
- Drop `records`, its indexes, and the `user_records`/`user_goals`/
  `user_insights` views.
- Delete `create_user_record`/`list_user_records`
  (`agents/tools/records.rs`) and their registrations
  (`agents/conversation.rs:310,314,411,415,491,495`). Keep
  `define_data_schema`/`list_data_schemas`/
  `validate_data_against_schema` — repointed at the new pipeline below.
- No data migration or backfill. Existing `spans`/`records` rows are
  wiped in dev before this ships.
- Goals and insights are **not** folded into `spans`. A goal ("save
  ₹50,000 for a TV") doesn't occupy time the way a Span does — it's a
  standing target evaluated *against* time, with progress computed live
  by querying spans (e.g. `SUM(data->>'amount')` for a schema/category
  over a period), not stored state. That's the same shape the future
  analytics/widgets sub-project needs (a named target + a query), so
  it's deferred there rather than half-built now. `manage_user_goal` is
  deleted, not rewritten, along with the `user_goals`/`user_insights`
  views — goal/insight management comes back properly scoped when the
  analytics sub-project starts.

### 2. Finish the classify + extract pipeline that already exists

System 1 (Jev triage + schema match) is already built and already runs
on every `inbound_events` row via the `process_event` job. This design
finishes it rather than building a parallel pipeline:

- **System 2 (new):** in `events/handler.rs`, the `Novel` branch
  (currently just a log line) gets a real handler — one Gemini call
  (`rig::providers::gemini`, same pattern as the deleted SMS extractor)
  that both defines a new schema (namespace/name/description/
  `json_schema`) via the same insert `define_data_schema` uses, and
  extracts the payload into it in the same round trip.
- **Existing-match branch (changed):** `events/handler.rs:125-137`'s
  `INSERT INTO records ...` becomes `INSERT INTO spans (..., schema_id,
  category, data, start_at, source, source_ref) ...` — `category` from
  the schema's `name`, `start_at` from the event's `occurred_at`,
  `source`/`source_ref` from `inbound_events.source_kind`/`id`.
- Both branches validate with the existing
  `validate_data_against_schema` before writing.
- **On success (either branch):** delete the `inbound_events` row in
  the same transaction as the `Span` insert — no reason to keep the raw
  payload once it's been turned into a span. **On failure:** leave the
  row in place with `processing_error` set and `processed_at` left
  `NULL`, so it's visible and retriable; nothing is deleted until a
  span is actually produced.
- SMS ingestion is rewritten to call `EventService::ingest`
  (`events/service.rs`) with `source_kind = "sms"` per message, instead
  of its own `sms_batches` table. `sms_batches`, `sms_processed`
  (`migrations/20260928000009_sms_processing.sql`), and
  `agents/sms_extractor.rs` are all deleted — `inbound_events` already
  provides the same idempotent-intake guarantee via its
  `(source_kind, source_id, external_event_id)` unique constraint. OTP
  filtering and fingerprint/merge-duplicate logic move into the System
  1/2 handling path as pre/post steps (they're dedup/spam filtering,
  not classification) — OTP messages get triaged as `ignore` by Jev
  rather than special-cased Rust string matching, since that's exactly
  what triage is for. The finance-specific extraction knowledge
  (EMI/bill/due-date handling) becomes seed `data_schemas`
  rows/descriptions for the finance namespace, read back by System 2
  instead of being hardcoded into a Rust prompt template.
- New sources (GPS stay/trip events, health readings, tasks) call the
  same `EventService::ingest` with their own `source_kind`; no
  source-specific backend code beyond producing that call.

### 3. GPS as a source (client-side; algorithm out of scope here)

vox-android performs stay-point/trip segmentation on-device (existing
`spans` module: `SpanApi.kt`, `SpanModels.kt`) and sends the server one
event per stay or per trip — never raw high-frequency pings. This
document only asserts the shape it feeds into step 2's pipeline; the
segmentation algorithm itself is a separate, follow-up spec.

### 4. Performance

- Hash-partition `spans` by `user_id` (16 partitions), not range-by-time.
  Range-partitioning on `start_at` was the original idea, but every
  unique constraint on a range-partitioned table must include the
  partition column — `spans_id_user_key UNIQUE (id, user_id)` would
  have to become `(id, user_id, start_at)`, which breaks the FKs from
  `collection_spans`, `reminders`, `sms_processed` (deleted anyway,
  above), and the self-referential `parent_id` FK
  (`migrations/20260923000000_initial_core.sql:1708-1709`), none of
  which carry the referenced span's `start_at`. Hashing on `user_id`
  needs no such rework — `user_id` is already the leading column of
  every existing constraint and FK into `spans` — at the cost of no
  time-based archival (out of scope; nothing today needs it). A given
  user's rows land entirely in one partition, so the dominant query
  (`user_id` + time range) still prunes to a single partition, and no
  one user's growth bloats another's vacuum/index cost.
- Add `spans_user_schema_idx` (above) and
  `GIN (data jsonb_path_ops)` for ad hoc field filtering.
- No OFFSET-based pagination introduced (existing `list()` already uses
  bounded time ranges + `LIMIT`; keep that pattern for any new queries).
- Future analytics/widget queries read from precomputed rollup tables,
  never scan raw `spans` live — out of scope here, noted so these
  index/partition choices aren't revisited when that work starts.

## Explicitly out of scope

- Schema display metadata (icon/color/field-format) and timeline UI
  grouped by category — separate sub-project.
- Analytics/widget suggestion and generation (rollup tables, chart
  specs, live refresh) — separate sub-project.
- Goal and insight tracking (target + live-computed progress query) —
  `manage_user_goal` and the `user_goals`/`user_insights` views are
  deleted with no replacement; rebuilt properly when the analytics
  sub-project starts.
- GPS stay-point/trip segmentation algorithm itself — separate spec.
- Any migration/backfill of existing `records`/`spans` rows — none
  needed; data is wiped and this starts fresh per explicit instruction.
- Fingerprint/merge-duplicate logic — kept as-is, relocated to wrap the
  System 1/2 path instead of the old SMS-specific one. OTP filtering is
  *not* kept as-is: it moves from Rust string matching to Jev triage
  (`ignore` action), since that's what triage already exists to do.
- `inbound_events` retention for *failed* rows (ones never successfully
  turned into a span) — none added here; they persist until retried.
  Only the success path deletes eagerly, per this design.

## Testing

None added. Any existing test that breaks because of the `records`
table/view removal, the SMS extractor/`sms_batches`/`sms_processed`
deletion, or the `manage_user_goal` deletion is to be deleted, not
fixed to pass around the change.

## Comments

No code comments added as part of this work.
