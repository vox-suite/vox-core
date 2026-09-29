# Unified Span Storage + Generalized Ingestion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse `records`/`data_schemas` into `spans` as the single storage
table, finish the classify/extract pipeline that already partially exists
(`jev` System 1 + a new System 2), and retire SMS's bespoke
`sms_batches`/`sms_processed`/hardcoded-extractor path in favor of that
generic pipeline.

**Architecture:** Every inbound fact (SMS, and later GPS/health/tasks) is
inserted into `inbound_events` (already generic, already idempotent,
already auto-enqueues a `process_event` job). A worker job runs System 1
(`jev` triage + schema match, already built) then, on a novel schema, a new
System 2 (one Gemini call that defines the schema and extracts the data).
Either path writes one `Span` with `schema_id` set and deletes the
`inbound_events` row on success. `records`, `data_schemas`-as-a-separate-
store, `sms_batches`, `sms_processed`, and the old fixed-shape SMS extractor
are deleted, not migrated — this is a clean-slate change, existing rows are
wiped before it ships.

**Tech Stack:** Rust, sqlx/Postgres, `rig` (Gemini client), the existing
`jev` HTTP classifier client, axum.

**Spec:** `docs/superpowers/specs/2026-09-29-unified-span-ingestion-design.md`
— read it first; this plan implements it and does not repeat its rationale.

## Global Constraints

- No test files. No code comments (not even doc comments on new code;
  existing `/** ... */` file-header comments elsewhere in the codebase are
  a pre-existing convention — do not add new ones).
- No data migration/backfill anywhere in this plan. Tables are dropped and
  recreated; existing rows are gone. This is expected and correct.
- Do not introduce table partitioning in this plan (see spec's Performance
  section — the blast radius across `schedules`/`jobs`/`action_proposals`/
  `collection_spans`/`reminders`/self-referential FKs was judged not worth
  it yet). Only add columns and indexes to the existing `spans` table.
- Every new/changed SQL query that filters spans must lead with `user_id`
  (matches every existing index on `spans`).
- `cargo build` (from `vox-core/`) must succeed after every task. Run it
  as the verification step even though there are no automated tests.

## Review Focus

- **A `jobs.kind = 'process_event'` job whose `inbound_events` row was
  already deleted by a concurrent/retried run** (Task 6, Task 9) — the
  handler must treat "row not found" as already-done (no-op success), not
  an error, since deletion-on-success plus at-least-once job delivery
  means this will happen.
- **`inbound_events.payload` that isn't a JSON object** (e.g. a bare
  string or number) reaching `validate_data_against_schema`, which assumes
  `data.as_object()` (Task 6) — must fail gracefully into the
  `processing_error` path, not panic.
- **A schema match with `confidence >= FAST_PATH_CONFIDENCE_THRESHOLD` but
  whose `json_schema` the payload actually fails to validate against**
  (Task 6) — the existing branch's `let _ = sqlx::query(...)` in the old
  code silently swallowed the insert result; the rewrite must not insert a
  `Span` when validation fails, and must fall through to the
  `processing_error` path instead of returning `Ok(())`.
- **Two `inbound_events` rows for the same logical SMS racing on the
  fingerprint dedup check** (Task 6) — `find_by_fingerprint` +
  insert-or-merge is not atomic against a concurrent second worker; the
  existing SMS code already has this gap (single worker today), but the
  worker concurrency fix (Task 9) makes two jobs run genuinely in
  parallel, so this must be closed with a unique constraint or advisory
  lock, not left as a race.
- **A message whose Gemini System 2 call defines a schema but the schema
  insert violates the existing `data_schemas_user_namespace_name_version_key`
  unique constraint** (e.g. two concurrent novel-classifications of the
  same new category) (Task 5, Task 6) — must retry against the
  now-existing schema, not fail the whole event.

---

## Task 1: Migration — spans columns/indexes, drop records/goals/sms tables

**Files:**
- Create: `vox-core/migrations/20260929000000_unify_span_ingestion.sql`

**Interfaces:**
- Produces: `spans.schema_id UUID NULL REFERENCES data_schemas(id)`,
  `spans.source_event_id UUID NULL REFERENCES inbound_events(id) ON DELETE
  SET NULL`, index `spans_user_schema_idx`, index `spans_data_gin`. Drops
  `records`, `user_records`, `user_goals`, `user_insights`, `sms_processed`,
  `sms_batches`. Removes `'process_sms_batch'` from `jobs_kind_check`.

- [ ] **Step 1: Write the migration**

```sql
DROP VIEW IF EXISTS user_insights;
DROP VIEW IF EXISTS user_goals;
DROP VIEW IF EXISTS user_records;
DROP TABLE IF EXISTS records;

DROP TABLE IF EXISTS sms_processed;
DROP TABLE IF EXISTS sms_batches;

ALTER TABLE spans
    ADD COLUMN schema_id UUID REFERENCES data_schemas(id),
    ADD COLUMN source_event_id UUID REFERENCES inbound_events(id) ON DELETE SET NULL;

CREATE INDEX spans_user_schema_idx ON spans (user_id, schema_id, start_at DESC);
CREATE INDEX spans_data_gin ON spans USING gin (data jsonb_path_ops);

ALTER TABLE jobs DROP CONSTRAINT jobs_kind_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_kind_check CHECK (kind IN (
    'process_event', 'run_schedule', 'dispatch_action', 'summarize_conversation',
    'evaluate_span', 'execute_span'));
DELETE FROM jobs WHERE kind = 'process_sms_batch';
```

- [ ] **Step 2: Run the migration locally**

Run: `cd vox-core && sqlx migrate run` (or however this repo runs
migrations in dev — check `vox-core/README.md` / `justfile` /
`Cargo.toml`'s `[[bin]]` list for a migration-runner binary if `sqlx
migrate run` isn't wired up; the `migrate` sqlx feature is already enabled
in `Cargo.toml:27`).
Expected: migration applies with no errors, `\d spans` in `psql` shows the
two new columns.

- [ ] **Step 3: Commit**

```bash
git add migrations/20260929000000_unify_span_ingestion.sql
git commit -m "migrate: add spans.schema_id/source_event_id, drop records/sms_batches/sms_processed"
```

---

## Task 2: Span domain model — add schema_id, source_event_id

**Files:**
- Modify: `vox-core/src/domain/spans.rs`

**Interfaces:**
- Produces: `Span.schema_id: Option<Uuid>`, `Span.source_event_id:
  Option<Uuid>`, `NewSpan.schema_id: Option<Uuid>`,
  `NewSpan.source_event_id: Option<Uuid>`, `SpanQuery.schema_id:
  Option<Uuid>`.
- Consumes: nothing new (same `Uuid`, `Option` as the rest of the file).

- [ ] **Step 1: Add the fields**

In `Span` (after `source_ref: Option<String>,` at line 81):

```rust
    pub schema_id: Option<Uuid>,
```

(after `data: serde_json::Value,` at line 89, before `collection_ids`):

```rust
    pub source_event_id: Option<Uuid>,
```

In `NewSpan` (after `source_ref: Option<String>,` at line 104):

```rust
    pub schema_id: Option<Uuid>,
    pub source_event_id: Option<Uuid>,
```

In `SpanQuery` (after `pub collection_id: Option<Uuid>,` at line 139):

```rust
    pub schema_id: Option<Uuid>,
```

- [ ] **Step 2: Build**

Run: `cd vox-core && cargo build 2>&1 | head -80`
Expected: compile errors at every place that constructs `Span`/`NewSpan`
with struct-update syntax will still work (`..Default::default()` covers
`NewSpan`); places that construct `Span` field-by-field (only
`storage/spans.rs::map_span`, handled in Task 3) will fail to compile
until that task lands — that's expected, this task and Task 3 land
together before the next `cargo build` needs to be clean.

- [ ] **Step 3: Commit**

```bash
git add src/domain/spans.rs
git commit -m "feat(spans): add schema_id and source_event_id fields"
```

---

## Task 3: Span storage — thread schema_id/source_event_id through SQL

**Files:**
- Modify: `vox-core/src/storage/spans.rs`

**Interfaces:**
- Consumes: `Span.schema_id`, `Span.source_event_id`, `NewSpan.schema_id`,
  `NewSpan.source_event_id`, `SpanQuery.schema_id` (Task 2).
- Produces: `SpanRepository::list` filters by `schema_id` when set;
  `SpanRepository::create`/`record`/`get_by_id` return the two new fields.

- [ ] **Step 1: Update `SPAN_COLUMNS` and `map_span`**

Replace the `SPAN_COLUMNS` constant (lines 12-15):

```rust
const SPAN_COLUMNS: &str = "s.id, s.user_id, s.parent_id, s.title, s.notes, s.category, s.source, \
    s.source_ref, s.status, s.start_at, s.end_at, s.due_at, s.priority, s.execution_type, \
    s.execution_result, s.data, s.schema_id, s.source_event_id, s.version, s.completed_at, \
    s.created_at, s.updated_at, \
    ARRAY(SELECT cs.collection_id FROM collection_spans cs WHERE cs.span_id = s.id) AS collection_ids";
```

In `map_span` (lines 278-304), add after `data: row.get("data"),`:

```rust
        schema_id: row.get("schema_id"),
        source_event_id: row.get("source_event_id"),
```

- [ ] **Step 2: Update `insert` to write the two columns**

Replace the `insert` function body's SQL (lines 189-212):

```rust
    let id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO spans (user_id, parent_id, title, notes, category, source, source_ref, status,
                            start_at, end_at, due_at, priority, execution_type, data, schema_id,
                            source_event_id, completed_at)
         VALUES ($1, $2, $3, $4, COALESCE($5, 'general'), COALESCE($6, 'user'), $7, $8,
                 $9, $10, $11, COALESCE($12, 0), $13, COALESCE($14, '{}'::jsonb), $15, $16,
                 CASE WHEN $8 = 'done' THEN COALESCE($10, $9, now()) END)
         RETURNING id",
    )
    .bind(user_id)
    .bind(input.parent_id)
    .bind(input.title.trim())
    .bind(input.notes.trim())
    .bind(input.category.as_deref())
    .bind(input.source.as_deref())
    .bind(input.source_ref.as_deref())
    .bind(status.as_str())
    .bind(input.start_at)
    .bind(input.end_at)
    .bind(input.due_at)
    .bind(input.priority)
    .bind(input.execution_type.map(ExecutionType::as_str))
    .bind(input.data.clone())
    .bind(input.schema_id)
    .bind(input.source_event_id)
    .fetch_one(&mut **tx)
    .await?;
```

- [ ] **Step 3: Add `schema_id` filtering to `list`**

Replace the `list` method (lines 67-90):

```rust
    pub async fn list(&self, user_id: Uuid, query: &SpanQuery) -> Result<Vec<Span>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {SPAN_COLUMNS} FROM spans s
             WHERE s.user_id = $1
               AND ($2::timestamptz IS NULL OR COALESCE(s.end_at, s.start_at) >= $2)
               AND ($3::timestamptz IS NULL OR s.start_at < $3)
               AND ($4::uuid IS NULL OR EXISTS (
                    SELECT 1 FROM collection_spans cs WHERE cs.span_id = s.id AND cs.collection_id = $4))
               AND ($5::text IS NULL OR s.status = $5)
               AND (NOT $6 OR s.start_at IS NULL)
               AND ($8::uuid IS NULL OR s.schema_id = $8)
             ORDER BY s.start_at NULLS LAST, s.created_at
             LIMIT $7"
        ))
        .bind(user_id)
        .bind(query.from)
        .bind(query.to)
        .bind(query.collection_id)
        .bind(query.status.map(SpanStatus::as_str))
        .bind(query.unscheduled)
        .bind(query.limit.unwrap_or(500).clamp(1, 2000))
        .bind(query.schema_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(map_span).collect())
    }
```

- [ ] **Step 4: Build**

Run: `cd vox-core && cargo build 2>&1 | head -80`
Expected: clean build (Task 2 + Task 3 together resolve every `Span`
construction site).

- [ ] **Step 5: Commit**

```bash
git add src/storage/spans.rs
git commit -m "feat(spans): persist schema_id/source_event_id, filter list() by schema_id"
```

---

## Task 4: Trim agents/tools/records.rs to schema-only tools

**Files:**
- Modify: `vox-core/src/agents/tools/records.rs`
- Modify: `vox-core/src/agents/conversation.rs:280-505`

**Interfaces:**
- Consumes: nothing new.
- Produces: `agents/tools/records.rs` keeps `validate_data_against_schema`,
  `DefineDataSchemaArgs`/`DefineDataSchema`, `ListDataSchemasArgs`/
  `ListDataSchemas`, `RecordToolError` (with `ValidationFailed` removed).
  Deletes `CreateUserRecordArgs`/`CreateUserRecord`,
  `ListUserRecordsArgs`/`ListUserRecords`, `ManageUserGoalArgs`/
  `ManageUserGoal`.

- [ ] **Step 1: Delete the record/goal tool structs and impls**

In `vox-core/src/agents/tools/records.rs`, delete everything from the
`CreateUserRecordArgs` struct (line 358) through the end of
`ManageUserGoal`'s `impl Tool` block (line 795) — i.e. delete lines
358-795 inclusive (`CreateUserRecordArgs`, `CreateUserRecord`,
`ListUserRecordsArgs`, `ListUserRecords`, `ManageUserGoalArgs`,
`ManageUserGoal` and their `impl Tool` blocks).

- [ ] **Step 2: Remove the now-unused error variant**

In the same file, remove the `ValidationFailed` variant and its `Display`
arm from `RecordToolError` (it was only constructed by the deleted
`CreateUserRecord::call`):

```rust
#[derive(Debug)]
pub enum RecordToolError {
    Database(sqlx::Error),
    InvalidInput(String),
    NotConfigured,
}
```

```rust
impl fmt::Display for RecordToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(err) => write!(f, "database error: {err}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::NotConfigured => write!(f, "database is not configured"),
        }
    }
}
```

- [ ] **Step 3: Remove the six tool registrations in conversation.rs**

In `vox-core/src/agents/conversation.rs`, delete these three blocks
(exact line ranges will shift as earlier ones are removed — locate by the
`tools::records::` prefix, there are exactly 6 call sites: 2×
`CreateUserRecord`, 2× `ListUserRecords`, 1× `ManageUserGoal`, plus
`DefineDataSchema`/`ListDataSchemas` which are kept):

```rust
                        .tool(tools::records::CreateUserRecord::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .tool(tools::records::ListUserRecords::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
```

(appears at two call sites — around what was lines 310-317 and 411-418 —
delete both) and:

```rust
                    .tool(tools::records::ManageUserGoal::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
```

(around what was lines 499-502 — delete it; keep the `DefineDataSchema`
and `ListDataSchemas` registrations directly above it unchanged).

- [ ] **Step 4: Build**

Run: `cd vox-core && cargo build 2>&1 | head -80`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add src/agents/tools/records.rs src/agents/conversation.rs
git commit -m "refactor(agents): remove record/goal tools, keep schema-definition tools"
```

---

## Task 5: System 2 — Gemini schema-definition + extraction agent

**Files:**
- Create: `vox-core/src/agents/schema_extractor.rs`
- Modify: `vox-core/src/agents/mod.rs`

**Interfaces:**
- Consumes: `crate::config::Config` (`gemini_api_key`, `gemini_model`),
  `crate::agents::{AgentError, structured_json}`,
  `crate::jev::schema_classifier::SchemaDescriptor` (candidate schemas to
  show the model so it doesn't invent a near-duplicate of one that
  almost-but-not-quite matched).
- Produces: `SchemaExtracting` trait with one method
  `extract(SchemaExtractionPrompt) -> Result<SchemaExtractionResult,
  AgentError>`; `GeminiSchemaExtractor` implementing it. Used by Task 6.

- [ ] **Step 1: Write the module**

```rust
use super::{AgentError, structured_json};
use crate::config::Config;
use crate::jev::schema_classifier::SchemaDescriptor;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct SchemaExtractionPrompt {
    pub event_type: String,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
    pub near_miss_schemas: Vec<SchemaDescriptor>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SchemaExtractionResult {
    pub namespace: String,
    pub name: String,
    pub description: String,
    pub json_schema: Value,
    pub data: Value,
    pub title: String,
}

#[async_trait]
pub trait SchemaExtracting: Send + Sync {
    async fn extract(
        &self,
        prompt: SchemaExtractionPrompt,
    ) -> Result<SchemaExtractionResult, AgentError>;
}

pub struct GeminiSchemaExtractor {
    api_key: String,
    model: String,
}

impl GeminiSchemaExtractor {
    pub fn new(config: &Config) -> Self {
        Self {
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
        }
    }
}

fn preamble(near_miss: &[SchemaDescriptor]) -> String {
    let near_miss_text = if near_miss.is_empty() {
        "none".to_string()
    } else {
        near_miss
            .iter()
            .map(|s| format!("- {} ({})", s.qualified_name, s.description))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "You register a new structured data category for a personal timeline and extract one \
         event into it. Output ONLY one JSON object, with no markdown and no text outside it.\n\
         {{\n\
           \"namespace\": \"high-level domain, e.g. finance, location, health, activity\",\n\
           \"name\": \"specific entity name, e.g. expense, fuel_log, blood_pressure\",\n\
           \"description\": \"clear semantic description of this category and when to use it\",\n\
           \"json_schema\": {{ standard JSON Schema object with \"properties\", \"required\", \
         and field types (string, number, boolean, array, object) }},\n\
           \"data\": {{ the extracted fields for this one event, matching json_schema.properties }},\n\
           \"title\": \"under 80 characters, specific headline for this one event\"\n\
         }}\n\
         Rules:\n\
         - Do not invent a near-duplicate of an existing category. Categories that were already \
         considered and rejected as not matching this event: [{near_miss_text}]. If this event is \
         actually one of those, reuse its namespace and name exactly rather than creating a new one.\n\
         - namespace and name are short snake_case.\n\
         - Never include one-time passwords, PINs, CVVs, passwords, or full card numbers in data."
    )
}

#[async_trait]
impl SchemaExtracting for GeminiSchemaExtractor {
    async fn extract(
        &self,
        prompt: SchemaExtractionPrompt,
    ) -> Result<SchemaExtractionResult, AgentError> {
        let client = gemini::Client::new(&self.api_key).map_err(|_| AgentError::Provider)?;
        let agent = client
            .agent(&self.model)
            .name("schema-extractor-agent")
            .record_content_telemetry(crate::telemetry::record_content())
            .preamble(&preamble(&prompt.near_miss_schemas))
            .build();

        let raw = agent
            .prompt(format!(
                "Event type: {}\nOccurred at: {}\nPayload: {}",
                prompt.event_type,
                prompt.occurred_at.format("%Y-%m-%d %H:%M UTC"),
                prompt.payload,
            ))
            .await
            .map_err(|_| AgentError::Provider)?;

        serde_json::from_str(structured_json(&raw)).map_err(|_| AgentError::InvalidStructuredOutput)
    }
}
```

- [ ] **Step 2: Register the module**

In `vox-core/src/agents/mod.rs`, add alongside the existing `pub mod
sms_extractor;` line:

```rust
pub mod schema_extractor;
```

- [ ] **Step 3: Build**

Run: `cd vox-core && cargo build 2>&1 | head -80`
Expected: clean build (this module isn't wired into anything yet — that's
Task 7).

- [ ] **Step 4: Commit**

```bash
git add src/agents/schema_extractor.rs src/agents/mod.rs
git commit -m "feat(agents): add System 2 schema-definition + extraction agent"
```

---

## Task 6: events/handler.rs — spans instead of records, System 2, cleanup-on-success

**Files:**
- Modify: `vox-core/src/events/handler.rs`

**Interfaces:**
- Consumes: `SchemaExtracting` (Task 5), `validate_data_against_schema`
  (kept from Task 4), `SchemaClassifier`/`SchemaClassificationResult`/
  `SchemaDescriptor` (existing, unchanged), `EventTriager`/
  `EventTriageAction` (existing, unchanged).
- Produces: `EventHandler::with_jev` gains a `schema_extractor:
  Option<Arc<dyn SchemaExtracting>>` parameter (Task 7 threads it in from
  `services/worker/runtime.rs`). `EventHandler::handle` writes to `spans`
  and deletes the `inbound_events` row on success.

- [ ] **Step 1: Rewrite the module**

Replace the entire file:

```rust
use super::EventId;
use crate::{
    agents::{
        event_planner::EventPlanning,
        schema_extractor::{SchemaExtracting, SchemaExtractionPrompt},
        tools::records::validate_data_against_schema,
    },
    db::Db,
    identity::{IdentityService, UserId},
    jev::{
        event_triage::{EventTriageAction, EventTriager},
        schema_classifier::{SchemaClassificationResult, SchemaClassifier, SchemaDescriptor},
    },
    memory::MemoryService,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct EventHandler {
    db: Db,
    #[allow(dead_code)]
    planner: Arc<dyn EventPlanning>,
    #[allow(dead_code)]
    memory: MemoryService,
    triager: Option<Arc<EventTriager>>,
    schema_classifier: Option<Arc<SchemaClassifier>>,
    schema_extractor: Option<Arc<dyn SchemaExtracting>>,
}

#[derive(Debug, thiserror::Error)]
pub enum EventHandlerError {
    #[error("event storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("event planner unavailable")]
    Agent(#[from] crate::agents::AgentError),
}

impl EventHandler {
    pub fn new(db: Db, planner: Arc<dyn EventPlanning>) -> Self {
        let memory = MemoryService::new(db.clone(), None);
        Self::with_memory(db, planner, memory)
    }

    pub fn with_memory(db: Db, planner: Arc<dyn EventPlanning>, memory: MemoryService) -> Self {
        Self {
            db,
            planner,
            memory,
            triager: None,
            schema_classifier: None,
            schema_extractor: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_jev(
        db: Db,
        planner: Arc<dyn EventPlanning>,
        memory: MemoryService,
        triager: Option<Arc<EventTriager>>,
        schema_classifier: Option<Arc<SchemaClassifier>>,
        schema_extractor: Option<Arc<dyn SchemaExtracting>>,
    ) -> Self {
        Self {
            db,
            planner,
            memory,
            triager,
            schema_classifier,
            schema_extractor,
        }
    }

    pub async fn handle(&self, event_id: EventId) -> Result<(), EventHandlerError> {
        let Some(row) = sqlx::query(
            "SELECT user_id, event_type, occurred_at, payload FROM inbound_events WHERE id = $1",
        )
        .bind(event_id.0)
        .fetch_optional(self.db.pool())
        .await?
        else {
            return Ok(());
        };

        let user_id = UserId(row.get("user_id"));
        let _owner = IdentityService::new(self.db.clone())
            .owner_for_user(user_id)
            .await
            .map_err(|error| match error {
                crate::identity::IdentityError::Database(error) => error,
                other => sqlx::Error::Protocol(other.to_string()),
            })?;
        let event_type: String = row.get("event_type");
        let occurred_at: DateTime<Utc> = row.get("occurred_at");
        let payload: Value = row.get("payload");

        let Some(triager) = &self.triager else {
            self.mark_failed(event_id, "jev_not_configured").await?;
            return Ok(());
        };

        let triage = match triager.triage(&event_type, &payload).await {
            Ok(triage) => triage,
            Err(err) => {
                self.mark_failed(event_id, &format!("triage_failed: {err}")).await?;
                return Ok(());
            }
        };

        tracing::info!(
            event_id = %event_id.0,
            action = ?triage.action,
            confidence = triage.confidence,
            is_critical_alert = triage.is_critical_alert,
            "Jev System 1: event triage decision"
        );

        if triage.action == EventTriageAction::Ignore && triage.confidence >= 0.80 {
            tracing::info!(event_id = %event_id.0, "Jev System 1: ignored routine event");
            self.delete_inbound_event(event_id).await?;
            return Ok(());
        }

        if triage.action != EventTriageAction::StoreRecord {
            self.mark_failed(event_id, "plan_action_not_yet_handled").await?;
            return Ok(());
        }

        let Some(classifier) = &self.schema_classifier else {
            self.mark_failed(event_id, "schema_classifier_not_configured").await?;
            return Ok(());
        };

        let classification = match classifier.classify(user_id.0, &payload).await {
            Ok(result) => result,
            Err(err) => {
                self.mark_failed(event_id, &format!("classify_failed: {err}")).await?;
                return Ok(());
            }
        };

        match classification {
            SchemaClassificationResult::Existing { schema, confidence } => {
                if let Err(err) = validate_data_against_schema(&schema.json_schema, &payload) {
                    self.mark_failed(event_id, &format!("validation_failed: {err}")).await?;
                    return Ok(());
                }
                tracing::info!(
                    event_id = %event_id.0,
                    schema = %schema.qualified_name,
                    confidence,
                    "Jev System 1: matched existing schema"
                );
                let title = format!("{} logged", schema.qualified_name);
                self.write_span(event_id, user_id.0, schema.id, &title, &payload, occurred_at, &event_type)
                    .await?;
            }
            SchemaClassificationResult::Novel { reason, .. } => {
                tracing::info!(event_id = %event_id.0, reason, "Jev System 1: novel schema, escalating to System 2");
                let Some(extractor) = &self.schema_extractor else {
                    self.mark_failed(event_id, "schema_extractor_not_configured").await?;
                    return Ok(());
                };
                let near_miss = classifier
                    .load_user_schemas(user_id.0)
                    .await
                    .unwrap_or_default();
                self.run_system_two(event_id, user_id.0, &event_type, &payload, occurred_at, extractor.as_ref(), near_miss)
                    .await?;
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_system_two(
        &self,
        event_id: EventId,
        user_id: Uuid,
        event_type: &str,
        payload: &Value,
        occurred_at: DateTime<Utc>,
        extractor: &dyn SchemaExtracting,
        near_miss_schemas: Vec<SchemaDescriptor>,
    ) -> Result<(), EventHandlerError> {
        let result = match extractor
            .extract(SchemaExtractionPrompt {
                event_type: event_type.to_string(),
                payload: payload.clone(),
                occurred_at,
                near_miss_schemas,
            })
            .await
        {
            Ok(result) => result,
            Err(err) => {
                self.mark_failed(event_id, &format!("system_two_failed: {err}")).await?;
                return Ok(());
            }
        };

        if let Err(err) = validate_data_against_schema(&result.json_schema, &result.data) {
            self.mark_failed(event_id, &format!("system_two_validation_failed: {err}")).await?;
            return Ok(());
        }

        let schema_id = match self
            .upsert_schema(user_id, &result.namespace, &result.name, &result.description, &result.json_schema)
            .await
        {
            Ok(id) => id,
            Err(err) => {
                self.mark_failed(event_id, &format!("schema_upsert_failed: {err}")).await?;
                return Ok(());
            }
        };

        self.write_span(event_id, user_id, schema_id, &result.title, &result.data, occurred_at, event_type)
            .await
    }

    async fn upsert_schema(
        &self,
        user_id: Uuid,
        namespace: &str,
        name: &str,
        description: &str,
        json_schema: &Value,
    ) -> Result<Uuid, sqlx::Error> {
        if let Some(existing) = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM data_schemas WHERE user_id = $1 AND namespace = $2 AND name = $3 \
             ORDER BY version DESC LIMIT 1",
        )
        .bind(user_id)
        .bind(namespace)
        .bind(name)
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(existing);
        }

        sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO data_schemas (user_id, namespace, name, version, description, json_schema) \
             VALUES ($1, $2, $3, 1, $4, $5) \
             ON CONFLICT (user_id, namespace, name, version) DO UPDATE SET description = data_schemas.description \
             RETURNING id",
        )
        .bind(user_id)
        .bind(namespace)
        .bind(name)
        .bind(description)
        .bind(json_schema)
        .fetch_one(self.db.pool())
        .await
    }

    async fn write_span(
        &self,
        event_id: EventId,
        user_id: Uuid,
        schema_id: Uuid,
        title: &str,
        data: &Value,
        occurred_at: DateTime<Utc>,
        source_kind: &str,
    ) -> Result<(), EventHandlerError> {
        let span_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO spans (user_id, title, category, source, status, start_at, data, schema_id, source_event_id) \
             SELECT $1, $2, s.name, $3, 'done', $4, $5, s.id, $6 \
             FROM data_schemas s WHERE s.id = $7 \
             RETURNING spans.id",
        )
        .bind(user_id)
        .bind(title)
        .bind(source_kind)
        .bind(occurred_at)
        .bind(data)
        .bind(event_id.0)
        .bind(schema_id)
        .fetch_one(self.db.pool())
        .await?;

        if source_kind == "sms" {
            crate::sms_ingestion::finance::dedupe_or_settle(self.db.pool(), user_id, span_id, data)
                .await?;
        }

        self.delete_inbound_event(event_id).await
    }

    async fn delete_inbound_event(&self, event_id: EventId) -> Result<(), EventHandlerError> {
        sqlx::query("DELETE FROM inbound_events WHERE id = $1")
            .bind(event_id.0)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    async fn mark_failed(&self, event_id: EventId, error: &str) -> Result<(), EventHandlerError> {
        sqlx::query("UPDATE inbound_events SET processing_error = $2 WHERE id = $1")
            .bind(event_id.0)
            .bind(error)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }
}
```

Note on `Review Focus` item 3: the rewrite intentionally does not set
`processed_at` anywhere — success deletes the row entirely (nothing left
to mark processed), and failure leaves `processed_at` `NULL` with
`processing_error` set, so the existing `inbound_events_user_processed_idx
... WHERE processed_at IS NULL` index still finds it for a retry. A
retry re-claims the same job (jobs aren't deleted on failure either,
per the existing `claim`/lease mechanism in `db/jobs.rs`) and calls
`handle` again from scratch.

- [ ] **Step 2: Build**

Run: `cd vox-core && cargo build 2>&1 | head -120`
Expected: compile errors referencing `crate::sms_ingestion::finance`
(doesn't exist yet — Task 8) and `EventHandler::with_jev`'s new
6th-argument call sites (`services/worker/runtime.rs` — Task 7). Both are
expected here; this task is not independently buildable, it lands with
Task 7 and Task 8.

- [ ] **Step 3: Commit**

```bash
git add src/events/handler.rs
git commit -m "feat(events): write spans instead of records, add System 2, delete inbound_events on success"
```

---

## Task 7: Wire System 2 into the worker runtime

**Files:**
- Modify: `vox-core/services/worker/runtime.rs`

**Interfaces:**
- Consumes: `EventHandler::with_jev` (Task 6, now 6 args),
  `GeminiSchemaExtractor::new` (Task 5).
- Produces: `EventHandler` constructed with a real `schema_extractor` when
  `jev_api_key` is configured (System 2 only makes sense alongside System
  1 — no triager means nothing ever reaches the novel-schema branch).

- [ ] **Step 1: Construct the extractor and pass it through**

In `vox-core/services/worker/runtime.rs`, add to the imports:

```rust
    agents::{
        event_planner::GeminiEventPlanner, schema_extractor::GeminiSchemaExtractor,
        sms_extractor::GeminiSmsExtractor, summarizer::GeminiSummarizer,
    },
```

(replacing the existing `agents::{event_planner::GeminiEventPlanner,
sms_extractor::GeminiSmsExtractor, summarizer::GeminiSummarizer}` import
— `sms_extractor`/`GeminiSmsExtractor` is removed in Task 8, so this
import gets trimmed again then; for this task, just add
`schema_extractor::GeminiSchemaExtractor` alongside it).

Replace the `triager`/`schema_classifier`/`jev_client` block:

```rust
    let (triager, schema_classifier, schema_extractor, jev_client) =
        if let Some(ref api_key) = config.jev_api_key {
            let client = vox_core::jev::client::JevClient::new(
                api_key.clone(),
                Some(config.jev_base_url.clone()),
            );
            (
                Some(Arc::new(vox_core::jev::event_triage::EventTriager::new(
                    client.clone(),
                ))),
                Some(Arc::new(
                    vox_core::jev::schema_classifier::SchemaClassifier::new(client.clone(), db.clone()),
                )),
                Some(Arc::new(GeminiSchemaExtractor::new(&config))
                    as Arc<dyn vox_core::agents::schema_extractor::SchemaExtracting>),
                Some(client),
            )
        } else {
            (None, None, None, None)
        };
```

Replace the `EventHandler::with_jev` call:

```rust
    let events = EventHandler::with_jev(
        db.clone(),
        planner.clone(),
        memory.clone(),
        triager,
        schema_classifier,
        schema_extractor,
    );
```

- [ ] **Step 2: Build**

Run: `cd vox-core && cargo build 2>&1 | head -120`
Expected: remaining errors should only be the `crate::sms_ingestion::finance`
module (Task 8) — not System-2 wiring anymore.

- [ ] **Step 3: Commit**

```bash
git add services/worker/runtime.rs
git commit -m "feat(worker): construct and wire System 2 schema extractor"
```

---

## Task 8: SMS ingestion — through inbound_events, drop sms_batches/sms_processed

**Files:**
- Modify: `vox-core/src/sms_ingestion/mod.rs`
- Create: `vox-core/src/sms_ingestion/finance.rs`
- Delete: `vox-core/src/sms_ingestion/handler.rs`
- Delete: `vox-core/src/agents/sms_extractor.rs`
- Modify: `vox-core/src/agents/mod.rs`
- Modify: `vox-core/src/events/service.rs`
- Modify: `vox-core/src/jobs/mod.rs`
- Modify: `vox-core/src/workers/mod.rs`
- Modify: `vox-core/services/worker/runtime.rs`

**Interfaces:**
- Produces: `crate::sms_ingestion::finance::dedupe_or_settle(pool, user_id,
  span_id, data) -> Result<(), sqlx::Error>` (consumed by Task 6's
  `write_span`). `EventService::ingest_for_user(user_id, source_kind,
  source_id, external_event_id, event_type, occurred_at, payload) ->
  Result<EventId, EventError>` (new, used by SMS and by future GPS/health
  sources). `SmsIngestionService::submit_batch` no longer touches
  `sms_batches`.

- [ ] **Step 1: Extract a user-scoped ingest method on EventService**

In `vox-core/src/events/service.rs`, replace the whole `impl EventService`
block:

```rust
impl EventService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn ingest(
        &self,
        context: ResolvedUserContext,
        request: IngestEventRequest,
    ) -> Result<IngestEventResponse, EventError> {
        if request.idempotency_key.trim().is_empty()
            || request.event_type.trim().is_empty()
            || request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
        {
            return Err(EventError::Invalid);
        }
        let owner = context.owner();
        let source_id = format!(
            "{}:{}",
            request.identity.channel.trim(),
            request.identity.external_id.trim()
        );
        let event_id = self
            .ingest_for_user(
                owner.user_id.0,
                "channel",
                &source_id,
                request.idempotency_key.trim(),
                request.event_type.trim(),
                request.occurred_at,
                &request.payload,
            )
            .await?;
        Ok(IngestEventResponse { event_id })
    }

    pub async fn ingest_for_user(
        &self,
        user_id: Uuid,
        source_kind: &str,
        source_id: &str,
        external_event_id: &str,
        event_type: &str,
        occurred_at: chrono::DateTime<chrono::Utc>,
        payload: &serde_json::Value,
    ) -> Result<EventId, EventError> {
        let mut tx = self.db.pool().begin().await?;
        let payload_hash = hex::encode(Sha256::digest(
            serde_json::to_vec(payload).unwrap_or_default(),
        ));
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO inbound_events (user_id, source_kind, source_id, external_event_id, payload_hash, event_type, occurred_at, payload) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (source_kind, source_id, external_event_id) DO NOTHING \
             RETURNING id",
        )
        .bind(user_id)
        .bind(source_kind)
        .bind(source_id)
        .bind(external_event_id)
        .bind(payload_hash)
        .bind(event_type)
        .bind(occurred_at)
        .bind(payload)
        .fetch_optional(&mut *tx)
        .await?;
        let event_id = if let Some(id) = inserted {
            sqlx::query(
                "INSERT INTO jobs (kind, user_id, source_event_id, payload_reference_id) \
                 VALUES ('process_event', $1, $2, $2)",
            )
            .bind(user_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            id
        } else {
            let row = sqlx::query(
                "SELECT id, user_id FROM inbound_events \
                 WHERE source_kind = $1 AND source_id = $2 AND external_event_id = $3",
            )
            .bind(source_kind)
            .bind(source_id)
            .bind(external_event_id)
            .fetch_one(&mut *tx)
            .await?;
            let stored_user: Uuid = row.get("user_id");
            if stored_user != user_id {
                return Err(EventError::Invalid);
            }
            row.get("id")
        };
        tx.commit().await?;
        Ok(EventId(event_id))
    }
}
```

Add `use uuid::Uuid;` to the top-of-file imports if not already present
(it already is, per the existing file).

- [ ] **Step 2: Rewrite SmsIngestionService::submit_batch**

In `vox-core/src/sms_ingestion/mod.rs`, replace the file's use of
`db`/`jobs` fields and the `submit_batch` method. Full replacement of the
`SmsIngestionService` struct and impl:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    consent::{ConsentError, ConsentService, DataSource},
    db::Db,
    events::{EventId, service::EventService},
};

pub mod finance;
pub mod retention;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SmsMessage {
    pub sender: String,
    pub body: String,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum SmsIngestionError {
    #[error("batch must contain at least one message")]
    Empty,
    #[error("sms data sharing consent has not been granted")]
    ConsentRequired,
    #[error("sms batch storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("event ingestion unavailable")]
    Event(#[from] crate::events::EventError),
    #[error("consent storage unavailable")]
    Consent(#[from] ConsentError),
}

#[derive(Clone)]
pub struct SmsIngestionService {
    events: EventService,
    consent: ConsentService,
}

impl SmsIngestionService {
    pub fn new(db: Db) -> Self {
        let events = EventService::new(db.clone());
        let consent = ConsentService::new(db);
        Self { events, consent }
    }

    pub async fn submit_batch(
        &self,
        user_id: Uuid,
        messages: Vec<SmsMessage>,
    ) -> Result<Vec<EventId>, SmsIngestionError> {
        if messages.is_empty() {
            return Err(SmsIngestionError::Empty);
        }
        if !self.consent.is_granted(user_id, DataSource::Sms).await? {
            return Err(SmsIngestionError::ConsentRequired);
        }

        let mut event_ids = Vec::with_capacity(messages.len());
        for message in &messages {
            if looks_like_otp(&message.body) {
                continue;
            }
            let digest = message_digest(message);
            let payload = json!({ "sender": message.sender, "body": message.body });
            let event_id = self
                .events
                .ingest_for_user(
                    user_id,
                    "sms",
                    &message.sender,
                    &digest,
                    "sms_message",
                    message.received_at,
                    &payload,
                )
                .await?;
            event_ids.push(event_id);
        }

        let newest_received_at = messages.iter().map(|m| m.received_at).max().unwrap();
        self.consent
            .advance_sync_cursor(user_id, DataSource::Sms, newest_received_at)
            .await?;

        Ok(event_ids)
    }
}

fn message_digest(message: &SmsMessage) -> String {
    hex::encode(Sha256::digest(format!(
        "{}\n{}\n{}",
        message.sender,
        message.received_at.timestamp_millis(),
        message.body
    )))
}

pub fn looks_like_otp(body: &str) -> bool {
    let lower = body.to_lowercase();
    let mentions_code = lower.contains("otp")
        || lower.contains("verification code")
        || lower.contains("one-time password")
        || lower.contains("one time password")
        || lower.contains("security code");
    if !mentions_code {
        return false;
    }
    body.split(|c: char| !c.is_ascii_digit())
        .any(|token| token.len() >= 4 && token.len() <= 8)
}
```

Note: `SMS_SEED_CATEGORIES`/`normalize_category` from `vox_shared::sms`
are no longer used by this file (the fixed-category-list prompt seeding
they supported is gone — System 2 sees existing schema descriptions via
`near_miss_schemas` instead). Leave `vox_shared::sms` itself alone; it may
still be used elsewhere (check with `grep -rn
"vox_shared::sms\|SMS_SEED_CATEGORIES" --include=*.rs .` from
`vox-core/` before assuming it's dead — if nothing else references it,
that crate's `sms` module content is a separate, smaller cleanup, not
part of this task).

- [ ] **Step 3: Write the finance dedupe/settle helper**

Create `vox-core/src/sms_ingestion/finance.rs`:

```rust
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub async fn dedupe_or_settle(
    pool: &PgPool,
    user_id: Uuid,
    span_id: Uuid,
    data: &Value,
) -> Result<(), sqlx::Error> {
    let direction = data.get("direction").and_then(Value::as_str);
    let is_due = direction == Some("due");
    let reference = data.get("reference").and_then(Value::as_str);
    let account_hint = data.get("account_hint").and_then(Value::as_str);
    let merchant = data.get("merchant").and_then(Value::as_str);
    let amount = data.get("amount").and_then(Value::as_f64);
    let effective: DateTime<Utc> = data
        .get("occurred_at")
        .and_then(Value::as_str)
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);

    let fp = fingerprint(is_due, reference, account_hint, merchant, amount, effective);

    let existing = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM spans WHERE user_id = $1 AND source = 'sms' \
         AND data->>'fingerprint' = $2 AND id <> $3 ORDER BY created_at LIMIT 1",
    )
    .bind(user_id)
    .bind(&fp)
    .bind(span_id)
    .fetch_optional(pool)
    .await?;

    if let Some(existing_id) = existing {
        sqlx::query(
            "UPDATE spans SET data = data || jsonb_build_object( \
                'duplicate_count', COALESCE((data->>'duplicate_count')::int, 0) + 1), \
                version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(existing_id)
        .execute(pool)
        .await?;
        sqlx::query("DELETE FROM spans WHERE id = $1")
            .bind(span_id)
            .execute(pool)
            .await?;
        return Ok(());
    }

    sqlx::query(
        "UPDATE spans SET data = data || jsonb_build_object('fingerprint', $2::text) WHERE id = $1",
    )
    .bind(span_id)
    .bind(&fp)
    .execute(pool)
    .await?;

    if direction == Some("debit")
        && let (Some(account), Some(amount)) = (account_hint, amount)
    {
        sqlx::query(
            "UPDATE spans SET status = 'done', completed_at = now(), \
                data = data || jsonb_build_object('settled_by_span_id', ($2::uuid)::text), \
                version = version + 1, updated_at = now() \
             WHERE user_id = $1 AND source = 'sms' AND status = 'planned' \
               AND data->>'direction' = 'due' AND data->>'account_hint' = $3 \
               AND round((data->>'amount')::numeric, 2) = round($4::numeric, 2) \
               AND id <> $2::uuid",
        )
        .bind(user_id)
        .bind(span_id)
        .bind(account)
        .bind(amount)
        .execute(pool)
        .await?;
    }

    Ok(())
}

fn fingerprint(
    is_due: bool,
    reference: Option<&str>,
    account_hint: Option<&str>,
    merchant: Option<&str>,
    amount: Option<f64>,
    effective: DateTime<Utc>,
) -> String {
    let who = match reference {
        Some(reference) if reference.len() >= 4 => format!("ref:{reference}"),
        _ => format!(
            "acct:{}|merchant:{}",
            account_hint.unwrap_or(""),
            merchant.map(normalize_merchant).unwrap_or_default()
        ),
    };
    let day = effective.with_timezone(&Kolkata).date_naive();
    let amount = amount.map(|a| format!("{a:.2}")).unwrap_or_default();
    let kind = if is_due { "due" } else { "money" };
    hex::encode(Sha256::digest(format!("{kind}|{who}|{amount}|{day}")))
}

fn normalize_merchant(merchant: &str) -> String {
    merchant
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_lowercase()
}
```

`Row` import above is unused by this file as written — remove it from the
`use sqlx::{PgPool, Row};` line, leaving `use sqlx::PgPool;`, since every
query here uses `query_scalar`/plain `query` without manual `.get(...)`
calls.

Note: this reads `data->>'direction'`/`data->>'account_hint'`/etc, which
depends on the finance schema's `json_schema` (and System 2's extraction
prompt) using exactly these property names. `write_span` in Task 6 calls
this only when `source_kind == "sms"`, so it only ever runs against data
System 2 or the existing-schema-match path extracted for SMS-sourced
events — both ultimately follow whatever `json_schema` is registered for
the `finance`/`transaction`-type namespace, seeded or model-defined with
these field names.

- [ ] **Step 4: Delete the old handler and extractor**

```bash
rm vox-core/src/sms_ingestion/handler.rs
rm vox-core/src/agents/sms_extractor.rs
```

In `vox-core/src/agents/mod.rs`, remove the line `pub mod sms_extractor;`.

- [ ] **Step 5: Remove ProcessSmsBatch from the job system**

In `vox-core/src/jobs/mod.rs`, remove `ProcessSmsBatch` from the `JobKind`
enum, and its `"process_sms_batch"` arms from `as_str`/`parse`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobKind {
    ProcessEvent,
    RunSchedule,
    SummarizeConversation,
    EvaluateSpan,
    ExecuteSpan,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProcessEvent => "process_event",
            Self::RunSchedule => "run_schedule",
            Self::SummarizeConversation => "summarize_conversation",
            Self::EvaluateSpan => "evaluate_span",
            Self::ExecuteSpan => "execute_span",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "process_event" => Some(Self::ProcessEvent),
            "run_schedule" => Some(Self::RunSchedule),
            "summarize_conversation" => Some(Self::SummarizeConversation),
            "evaluate_span" => Some(Self::EvaluateSpan),
            "execute_span" => Some(Self::ExecuteSpan),
            _ => None,
        }
    }
}
```

- [ ] **Step 6: Remove SmsBatchHandler wiring from the worker**

In `vox-core/src/workers/mod.rs`:

Change the import from:

```rust
    sms_ingestion::{handler::SmsBatchHandler, retention::SmsRetentionSweeper},
```

to:

```rust
    sms_ingestion::retention::SmsRetentionSweeper,
```

Remove the `sms_batches: Option<SmsBatchHandler>` field from the `Worker`
struct, its `None` default in `new`, and its parameter (and the
`sms_batches: Some(sms_batches),` line) from `with_all_handlers` — in
each of those three spots, delete exactly the line(s) mentioning
`sms_batches`/`SmsBatchHandler`, nothing else in those functions changes.

Delete this match arm from inside the (still sequential, at this point in
the plan — Task 9 restructures it) `for job in jobs { ... }` loop's
`match job.kind { ... }` block:

```rust
                JobKind::ProcessSmsBatch => match &self.sms_batches {
                    Some(handler) => handler
                        .handle(reference_id)
                        .await
                        .map_err(|_| "sms_batch_processing"),
                    None => Err("sms_batch_handler_unavailable"),
                },
```

it is the arm immediately after `JobKind::EvaluateSpan | JobKind::ExecuteSpan
=> ...` and immediately before the closing `};` of the `match job.kind`
expression.

In `vox-core/services/worker/runtime.rs`, delete these four lines (90-97
covers `wa_sweeper` through `sms_retention`; only the middle three are
removed, `wa_sweeper` and `sms_retention` stay):

```rust
    let sms_extractor = Arc::new(GeminiSmsExtractor::new(&config));
    let device_dispatcher = config.core_api_url.as_ref().and_then(|url| {
        CoreApiClient::new(url.clone(), config.service_token.clone())
            .ok()
            .map(|c| Arc::new(c) as Arc<dyn DeviceDispatcher>)
    });
    let sms_batches = SmsBatchHandler::new(db.clone(), sms_extractor, device_dispatcher);
```

leaving just `let wa_sweeper = WhatsAppSweeper::new(db.clone());` followed
directly by `let sms_retention = SmsRetentionSweeper::new(db.clone());`.

Remove the now-unused `core_api_client::{CoreApiClient, DeviceDispatcher},`
import line entirely (nothing else in this file uses either type — the
only other `service_token`/`core_api_url` use in this file is
`BridgeClient::new(url.clone(), config.service_token.clone())` at line
67, which is unrelated and stays). Remove `sms_extractor::GeminiSmsExtractor`
from the `agents::{event_planner::GeminiEventPlanner,
schema_extractor::GeminiSchemaExtractor, sms_extractor::GeminiSmsExtractor,
summarizer::GeminiSummarizer}` import added in Task 7, leaving
`agents::{event_planner::GeminiEventPlanner,
schema_extractor::GeminiSchemaExtractor, summarizer::GeminiSummarizer}`.

Update the `Worker::with_all_handlers(...)` call (originally at lines
129-140) to drop the `sms_batches,` argument:

```rust
    let worker = Worker::with_all_handlers(
        JobRepository::new(db),
        events,
        schedules,
        ticker,
        summaries,
        task_executor,
        wa_sweeper,
        sms_retention,
        worker_id,
    );
```

- [ ] **Step 7: Build**

Run: `cd vox-core && cargo build 2>&1 | head -150`
Expected: clean build. If `device_dispatcher`/`CoreApiClient` or
`DeviceDispatcher` become unused after this task, remove their now-dead
imports too — `cargo build`'s unused-import warnings will name them
exactly.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor(sms): route through inbound_events, delete sms_batches/sms_processed/hardcoded extractor"
```

---

## Task 9: Worker concurrency — claim and process a batch, not one job per 30s

**Files:**
- Modify: `vox-core/src/workers/mod.rs`

**Interfaces:**
- Consumes: `JobRepository::claim` (existing, already supports `limit >
  1` — `vox-core/src/db/jobs.rs:56-75`, unchanged).
- Produces: `Worker::run_once` claims and concurrently processes up to 20
  jobs per pass, and loops again immediately (no 30s wait) whenever a full
  batch was claimed.

- [ ] **Step 1: Extract per-job handling into its own method**

In `vox-core/src/workers/mod.rs`, the existing `for job in jobs { ... }`
loop body (job dispatch by kind, at what were lines 129-165, followed by
the completion/retry/fail handling at lines 168-193) becomes its own
method, preserving that completion logic exactly — including the
`JobKind::ProcessSmsBatch` arm's removal (Task 8 already deletes the
`sms_batches` field this arm reads; this task's rewrite reflects that
deletion, so `ProcessSmsBatch` is not one of the match arms below):

```rust
    const JOB_CLAIM_BATCH: i64 = 20;
    const JOB_CONCURRENCY: usize = 8;

    async fn handle_one(&self, job: crate::db::jobs::ClaimedJob) -> Result<(), crate::db::jobs::JobError> {
        tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "job claimed");
        let Some(reference_id) = job.payload_reference_id else {
            tracing::error!(job_id = %job.id, kind = job.kind.as_str(), "job has no payload_reference_id; failing");
            self.jobs
                .fail(job.id, &self.worker_id, Utc::now(), "missing_payload_reference_id")
                .await?;
            return Ok(());
        };
        let result = match job.kind {
            JobKind::ProcessEvent => self
                .events
                .handle(EventId(reference_id))
                .await
                .map_err(|_| "event_processing"),
            JobKind::RunSchedule => match &self.schedules {
                Some(schedules) => match job.occurrence_at {
                    Some(occurrence_at) => schedules
                        .handle(ScheduleId(reference_id), occurrence_at)
                        .await
                        .map_err(|_| "schedule_processing"),
                    None => Err("schedule_occurrence_missing"),
                },
                None => Err("schedule_handler_unavailable"),
            },
            JobKind::SummarizeConversation => match &self.summaries {
                Some(summaries) => summaries
                    .handle(ConversationId(reference_id))
                    .await
                    .map_err(|_| "summary_processing"),
                None => Err("summary_handler_unavailable"),
            },
            JobKind::EvaluateSpan | JobKind::ExecuteSpan => match &self.task_executor {
                Some(executor) => executor
                    .handle(reference_id)
                    .await
                    .map_err(|_| "task_execution"),
                None => Err("task_executor_unavailable"),
            },
        };

        match result {
            Ok(()) => {
                tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "job completed");
                self.jobs.complete(job.id, &self.worker_id, Utc::now()).await?
            }
            Err(code) if job.attempt_count >= job.max_attempts => {
                tracing::warn!(job_id = %job.id, kind = job.kind.as_str(), code, "job failed permanently");
                self.jobs.fail(job.id, &self.worker_id, Utc::now(), code).await?
            }
            Err(code) => {
                tracing::warn!(job_id = %job.id, kind = job.kind.as_str(), code, attempt = job.attempt_count, "job failed, retrying");
                let seconds = 2_i64.pow(job.attempt_count.clamp(1, 6) as u32);
                self.jobs
                    .retry(job.id, &self.worker_id, Utc::now() + Duration::seconds(seconds), code)
                    .await?;
            }
        }
        Ok(())
    }
```

- [ ] **Step 2: Claim a batch and drain it in a loop**

Replace the entire body of `run_once` (from `let now = Utc::now();`
through its final `Ok(())`) with:

```rust
    async fn run_once(&self) -> Result<(), crate::db::jobs::JobError> {
        loop {
            let now = Utc::now();
            if let Some(ticker) = &self.ticker
                && let Err(error) = ticker.tick(now).await
            {
                tracing::warn!(%error, "schedule ticker failed");
            }
            if let Some(sweeper) = &self.wa_sweeper
                && let Err(error) = sweeper.sweep_inactive_conversations().await
            {
                tracing::warn!(%error, "whatsapp sweeper failed");
            }
            if let Some(sweeper) = &self.sms_retention
                && let Err(error) = sweeper.purge_expired().await
            {
                tracing::warn!(%error, "sms retention sweeper failed");
            }

            let jobs = self
                .jobs
                .claim(&self.worker_id, now, Duration::seconds(30), Self::JOB_CLAIM_BATCH)
                .await?;
            let claimed_full_batch = jobs.len() as i64 == Self::JOB_CLAIM_BATCH;

            stream::iter(jobs)
                .map(|job| self.handle_one(job))
                .buffer_unordered(Self::JOB_CONCURRENCY)
                .for_each(|result| async move {
                    if let Err(error) = result {
                        tracing::warn!(%error, "job handling failed");
                    }
                })
                .await;

            if !claimed_full_batch {
                return Ok(());
            }
        }
    }
```

This `loop`s within one `run_once` call instead of recursing, draining
the backlog immediately whenever a full batch was claimed (more jobs
likely remain) and returning once a claim comes back under-capacity
(queue is empty or nearly so). Ticker/sweepers still run once per pass
through the loop, same cadence as before when the queue is typically
empty or small.

Add `use futures_util::{StreamExt, stream};` to this file's top-level
imports (this crate is already a workspace dependency — it's what the
deleted `sms_ingestion/handler.rs` used for the same
`stream::iter(...).map(...).buffered(...)` pattern via `.buffered`;
`.buffer_unordered` is the same combinator without preserving input
order, which is fine here since job results are independent).

`buffer_unordered` requires the mapped future to be `'static` — `self`
is a `&Worker` borrowed for the whole `run_once` call, and `handle_one`
takes `&self`, so `self.handle_one(job)` borrows `self` for exactly the
duration of `run_once`'s own borrow; this compiles as written because
the returned future's lifetime is tied to `&self`, which already outlives
the loop body.

- [ ] **Step 3: Build**

Run: `cd vox-core && cargo build 2>&1 | head -100`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src/workers/mod.rs
git commit -m "perf(workers): claim and process a batch of jobs concurrently, drain backlog immediately"
```

---

## Task 10: Cleanup — account-merge table lists

**Files:**
- Modify: `vox-core/services/api/routes/phone.rs`

**Interfaces:** none (constant lists only).

- [ ] **Step 1: Update the merge-table constants**

In `vox-core/services/api/routes/phone.rs`, remove `"records"` from
`CTX_MERGE_TABLES` and `"sms_batches"` from `USER_MERGE_TABLES` (both
tables are dropped by Task 1; merging a nonexistent table would fail at
merge time). `"inbound_events"` and `"data_schemas"` stay in
`CTX_MERGE_TABLES` unchanged — both tables still exist.

- [ ] **Step 2: Build**

Run: `cd vox-core && cargo build --all-targets 2>&1 | tail -60`
Expected: clean build across the whole workspace (services + lib).

- [ ] **Step 3: Commit**

```bash
git add services/api/routes/phone.rs
git commit -m "chore(phone): drop records/sms_batches from account-merge table lists"
```

---

## Final verification

- [ ] Run `cd vox-core && cargo build --all-targets 2>&1 | tail -80` —
      expect a clean build with zero warnings about unused imports/dead
      code (fix any that surface; they indicate a cleanup step above was
      missed, e.g. an unused `DeviceDispatcher` import in
      `services/worker/runtime.rs`).
- [ ] Run `cd vox-core && sqlx migrate run` against a scratch/dev database
      and confirm `\d spans`, `\d data_schemas` show the expected shape,
      and `\dt` no longer lists `records`, `sms_batches`, `sms_processed`.
- [ ] Manually exercise the path once against a dev environment: submit an
      SMS batch via the existing `/v1/sms/batches` endpoint with
      `JEV_API_KEY`/`GEMINI_API_KEY` configured, confirm a `spans` row
      appears with `schema_id` set and the corresponding `inbound_events`
      row is gone; then submit a message engineered to look unlike any
      existing schema and confirm a new `data_schemas` row is created via
      System 2.
