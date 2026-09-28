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
- Goals and insights fold into `spans` using `category` as the
  discriminator (`category = "goal"` / `"insight"`, with the specific
  subject namespace still coming from `data_schemas.namespace`) instead
  of a new `kind` column: `due_at` carries a goal's target date,
  `status = Done` carries completion, `data` carries
  `target_metric`/`reasoning`. `manage_user_goal` is rewritten to insert
  into `spans` on this convention.
  **This mapping is the one piece not walked through turn-by-turn
  earlier — flagging it explicitly for review.**

### 2. Generalized classify + extract pipeline

- New source-agnostic module, e.g. `vox-core/src/ingestion/classify.rs`:
  takes one raw item (`title`/`body` text, `source_kind`, `occurred_at`)
  and returns a ready-to-insert `Span`.
- **Step 1, no LLM:** pgvector cosine-similarity search over
  `data_schemas.embedding`, scoped to `(user_id OR user_id IS NULL) AND
  state = 'active'`, returning top-K candidates above a similarity
  threshold.
- **Step 2, one LLM call:** given the candidates (or none), a single
  prompt that either (a) picks a candidate and extracts `data` matching
  its `json_schema`, or (b) defines a new schema inline
  (namespace/name/description/json_schema) and extracts into it — the
  same single-round-trip shape `sms_extractor.rs` already uses today,
  generalized to not hardcode the output fields.
- Result is validated with the existing `validate_data_against_schema`;
  on success, insert one `Span` (`schema_id` set, `category` from the
  schema's `name`, `data` from extraction, `start_at` mapped the same
  way `sms_ingestion/handler.rs` already maps point-in-time facts today).
  New schemas from step 2b are persisted the same way
  `define_data_schema` already does (versioned insert into
  `data_schemas`).
- `sms_ingestion/handler.rs` is rewritten to call this pipeline instead
  of `SmsExtracting`/`GeminiSmsExtractor`; `agents/sms_extractor.rs` is
  deleted. OTP filtering and fingerprint/merge-duplicate logic
  (`handler.rs`) stay as pre/post steps around the generic pipeline —
  they're dedup/spam filtering, not classification. The finance-specific
  extraction knowledge (EMI/bill/due-date handling) becomes seed
  `data_schemas` rows/descriptions for the finance namespace, read back
  by the LLM in step 2, instead of being hardcoded into a Rust prompt
  template.
- New sources (GPS stay/trip events, health readings, tasks) call the
  same pipeline; no source-specific backend code beyond parsing that
  source's raw payload into the `(title/body, source_kind, occurred_at)`
  shape.

### 3. GPS as a source (client-side; algorithm out of scope here)

vox-android performs stay-point/trip segmentation on-device (existing
`spans` module: `SpanApi.kt`, `SpanModels.kt`) and sends the server one
event per stay or per trip — never raw high-frequency pings. This
document only asserts the shape it feeds into step 2's pipeline; the
segmentation algorithm itself is a separate, follow-up spec.

### 4. Performance

- Range-partition `spans` by month on `start_at` (partition key falls
  back to `created_at` for null-`start_at` rows). Bounds per-partition
  size as data grows, prunes on the time-bounded queries
  `storage/spans.rs::list` already does, and lets old partitions be
  archived or dropped independently.
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
- GPS stay-point/trip segmentation algorithm itself — separate spec.
- Any migration/backfill of existing `records`/`spans` rows — none
  needed; data is wiped and this starts fresh per explicit instruction.
- OTP detection and duplicate-merge logic changes — kept as-is,
  relocated to wrap the new pipeline instead of the old SMS-specific one.

## Testing

None added. Any existing test that breaks because of the `records`
table/view removal, the SMS extractor deletion, or the `manage_user_goal`
rewrite is to be deleted, not fixed to pass around the change.

## Comments

No code comments added as part of this work.
