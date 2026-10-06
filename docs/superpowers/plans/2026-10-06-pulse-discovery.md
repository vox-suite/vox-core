# Pulse Discovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Let the user discover, preview and save trustworthy charts from their spans and connections, starting from an empty Pulse canvas.

**Architecture:** Core profiles permitted sources and compiles approved measurements into bounded aggregate queries. Discovery combines deterministic recipes with AI ranking; previews and saved charts share execution and caches. Desktop uses the existing HTTP platform port and Recharts.

**Tech Stack:** Rust, Axum, SQLx/PostgreSQL, existing Gemini integration, chrono-tz, React/TypeScript, Recharts, Tauri.

**Spec:** [Approved design](../specs/2026-10-06-pulse-discovery-design.md).

## Global Constraints

- Preserve unrelated working-tree edits.
- No category selection or board naming is required before discovering or adding a chart.
- Existing boards remain readable and retain their identifiers and charts.
- AI runs during discovery only. Chart refresh executes saved definitions.
- Daily/weekly buckets use the user's timezone.
- Missing capture coverage is a gap; zero is emitted only for covered periods with zero matching events.
- Connection presence alone never establishes a measurement or permission to access provider data.
- Default discovery TTL is 15 minutes. Chart result TTL is 60 seconds.
- At most four aggregate groups execute per cold board request, using bounded database concurrency of two.
- Time series return at most 366 points per series; categorical series return at most 20 explicit values.
- No provider API redesign, deployment, push, or new daily-rollup infrastructure is included by implication.

## Review Focus

- Connection and schema access revoked between preview and save: reject the save and cached reads.
- Late imports and undated records: preserve event dates and surface unknown dates without import-date substitution.
- Simultaneous refresh and writes: do not publish old results under a new data revision.
- Old boards and delegated Share to Action reads: preserve identifiers, legacy behavior and consent limits.
- Repeat Add actions and changed sources: duplicate requests create one chart; unsupported source sets fail atomically.

## Files and ownership

Core owns all validation and calculations. Create `src/domain/pulse.rs` for the versioned contract, `src/application/pulse/` for recipes, profiling, validation, compiler, execution, discovery and tests, and `src/storage/pulse.rs` for persistence. Create `services/api/routes/pulse.rs` for v2 handlers. Existing legacy chart modules remain available.

Desktop owns presentation and selected settings. Extend `src/features/pulse/api.ts` and `types.ts`; create `components/pulse/chart-card.tsx`, `add-pulse-dialog.tsx`, `suggestions-view.tsx`, `manual-chart-flow.tsx` and `hooks/use-pulse-canvas.ts`. Refactor the existing chart renderer out of `board-view.tsx`; update `pulse-view.tsx`. Do not touch connector ingestion or unrelated Share to Action edits.

## Task 1: Versioned definitions, validation and reliable measurements

**Files:** Create `src/domain/pulse.rs`, `src/application/pulse/mod.rs`, `src/application/pulse/measurements.rs`, `src/application/pulse/validation.rs`, `src/application/pulse/tests.rs`; register modules in existing domain/application module files.

**Interfaces:** `PulseDefinition`, `SourceSelector`, `Measurement`, `Coverage`, `ValidatedDefinition`; `measurement_catalog(&[SourceProfile]) -> Vec<Measurement>`; `validate_definition(&PulseDefinition, &[Measurement], &[SourceProfile]) -> Result<ValidatedDefinition, PulseError>`. `SourceProfile` is defined in this task and populated by Task 2. `PulseError` has invalid-definition, forbidden-source, insufficient-coverage and unavailable-data variants.

- [ ] Add typed version-2 contracts, using enums for bucket, aggregation, measurement kind and quality. Existing QuerySpec remains unchanged.

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PulseDefinition {
    pub version: u8,
    pub measurement_id: String,
    pub sources: Vec<SourceSelector>,
    pub bucket: Option<Bucket>,
    pub dimension: Option<String>,
    pub period_days: u16,
    pub timezone: String,
    pub chart_type: ChartType,
}
```

`SourceSelector` contains optional schema_id, source, category and action, with at least one selector required. User identity comes from Actor, never the request. `SourceProfile` contains selector, count, first/last event, unknown-date count, status counts, typed field summaries, units, connection freshness and coverage evidence. Allowed period is 1–365 days; version must equal 2; timezone must parse as chrono_tz::Tz. Reject arbitrary expressions, excessive source lists (>20), unsupported dimensions and mixed-unit sums.

- [ ] Write failing recipe tests using pure profile fixtures in `tests.rs`:

```rust
#[test]
fn spotify_track_length_is_never_measured_listening_time() {
    let profiles = profiles_from_json(serde_json::json!([
        {"source":"spotify", "action":"listen", "count":12,
         "fields":{"reported_track_duration_ms":"number"},
         "facts":{"playback_end_known":false}}
    ]));
    let measurements = measurement_catalog(&profiles);
    assert!(measurements.iter().any(|m| m.id == "spotify.plays"));
    assert!(!measurements.iter().any(|m| m.id == "spotify.measured_hours"));
}
```

Define `profiles_from_json` in the test module to deserialize test-only profile summaries; production never parses these fixture-specific shortcuts. Add counterpart tests for YouTube playlist versus watch events, PSN observation intervals disallowing daily allocation, paid subscriptions versus channel snapshots, mixed currency, unknown dates and planned/cancelled events.

- [ ] Run `cargo test --lib application::pulse`; confirm failures before implementing catalog and validator.
- [ ] Implement fixed recipes: event counts, artist/game/category totals, known intervals, cumulative observation deltas, transaction sums by currency and monthly subscription projections. Read provider payload shape from the Core ingestion mapping and pinned vox-connections revision; do not assume local connector changes are deployed.
- [ ] Run the pure tests. Commit only this task's files.

## Task 2: Batched inventory, persistent revisions and dismissed definitions

**Files:** Create `migrations/20261006000000_pulse_discovery.sql`, `src/storage/pulse.rs`, `src/application/pulse/profiling.rs`; extend `src/application/pulse/tests.rs` with database tests.

**Interfaces:** `PulseRepository::load_inventory(&Actor, DateTime<Utc>) -> Result<Inventory, PulseError>`; `Inventory` contains profiles, measurements, revisions and permitted source metadata. `PulseRepository::save_chart(&Actor, SavePulseInput) -> Result<SavedPulseChart, PulseError>`; `SavePulseInput` contains idempotency_key, title, definition. Repository cache methods are used in Task 4.

- [ ] Add `pulse_revisions(user_id PRIMARY KEY, data_revision BIGINT, discovery_revision BIGINT)` and revision triggers on spans INSERT/UPDATE/DELETE. An ownership transfer advances both old/new owners. Changes to data_schemas and the actual connection/consent tables advance discovery revisions for affected owners; a global schema change increments a singleton global revision included in cache keys.
- [ ] Add `pulse_saved_charts` with actor ownership, optional legacy board linkage, version-2 definition, normalized definition hash, timestamps and `(user_id,idempotency_key)` uniqueness; `pulse_dismissals(user_id,definition_hash)` with uniqueness. Add cache storage with user/access identity, definition/profile hashes, revisions, expiration and bounded payload length.

```sql
CREATE TABLE pulse_revisions (
    user_id uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    data_revision bigint NOT NULL DEFAULT 0,
    discovery_revision bigint NOT NULL DEFAULT 0
);
```

- [ ] Create an isolated migrated SQLx test fixture; reuse the database setup patterns in `src/connection_ingestion_tests.rs` and `src/integrations/tests.rs`. Never run destructive fixture setup against DATABASE_URL.

```rust
#[sqlx::test(migrations = "./migrations")]
async fn deletes_advance_revision(pool: PgPool) {
    let fixture = PulseFixture::new(pool).await;
    let span = fixture.insert_dated_expense(100.0, "INR").await;
    let before = fixture.revisions().await;
    fixture.delete_span(span).await;
    assert!(fixture.revisions().await.data_revision > before.data_revision);
}
```

Define `PulseFixture` and its listed methods in `tests.rs`; seed a user/context using existing identity fixtures. Also test edits, access revocation, unschematized spans, global schema changes and actor isolation.
- [ ] Implement up to four inventory queries: actor/access/revisions with permitted connection metadata; schema catalog; full-history source counts/date coverage; bounded field summaries and five representative redacted samples per selected source. Use source/category grouping for schema-less spans. Do not query every source individually. Avoid credentials and notes in model inputs.
- [ ] Rank 20 profiles and cap the derived catalog at 40 measurements. Include full-inventory counts and the profiled window in Inventory.
- [ ] Verify migrations and tests on the isolated database; inspect query plans and record scanned-row counts. Add further indexes only when plans justify them. Commit this task.

## Task 3: Shared bounded query compiler and real previews

**Files:** Create `src/application/pulse/compiler.rs`, `execution.rs`; extend pulse tests. Preserve `src/application/chart_query.rs` for legacy definitions.

**Interfaces:** `compile_groups(&[ValidatedDefinition], &ResolvedPeriod) -> Result<Vec<CompiledGroup>, PulseError>`; `execute_groups(&PgPool, &Actor, &[CompiledGroup]) -> Result<Vec<PulseResult>, PulseError>`. `PulseResult` includes definition hash, series, coverage, quality, computed_at and data_as_of. `ResolvedPeriod` contains absolute from/to bounds and timezone.

- [ ] Write integration tests proving preview totals, event counts without numeric fields, currency separation, interval splitting over local midnight, unknown-date exclusion with coverage reporting, PSN cumulative-delta semantics and weighted averages. Use fixed timestamps, including Asia/Kolkata and a DST timezone.

```rust
#[sqlx::test(migrations = "./migrations")]
async fn previews_count_events_without_numeric_fields(pool: PgPool) {
    let fixture = PulseFixture::new(pool).await;
    fixture.insert_spotify_plays(3).await;
    let result = fixture.preview("spotify.plays", "Asia/Kolkata").await;
    assert_eq!(result.total(), 3.0);
}
```

Extend PulseFixture with `insert_spotify_plays`, `preview` and `PulseResult::total` as a test helper summing count series, not a general production aggregation.
- [ ] Implement parameterized SQL using fixed compiler branches. Bind actor, source selectors, field paths and dates; allow bucket SQL only through enums. Count events with COUNT(*), not COUNT(numeric_field). Flatten provider fields using approved adapters with tested field paths.
- [ ] Group compatible definitions, with at most four groups and concurrency two. Use set-based multi-metric aggregates; do not count one giant UNION with a scan per chart as successful batching. Paginate definitions beyond group bounds. Resolve periods before creating cache keys.
- [ ] Limit output to 366 points per series and 20 categorical values. Return explicit gaps unless capture continuity is evidenced. For averages either carry numerator/denominator into Other or omit Other.
- [ ] Run integration tests and compare EXPLAIN ANALYZE for a six-chart fixture. Add a query-budget observer around repository/compiler execution so tests assert executed statements rather than inferred counts. Commit this task.

## Task 4: Coalesced caches, discovery and atomic chart creation

**Files:** Create `src/application/pulse/cache.rs`, `discovery.rs`, `service.rs`; extend `src/agents/chart_suggester.rs` with a separate v2 ranking interface without breaking legacy consumers; extend pulse repository/tests.

**Interfaces:** `PulseService::discover(&Actor, DiscoveryInput) -> Result<DiscoveryResponse, PulseError>`; `preview(&Actor, PulseDefinition) -> Result<PulseResult, PulseError>`; `save(&Actor, SavePulseInput) -> Result<SavedPulseChart, PulseError>`; `canvas(&Actor, CanvasInput) -> Result<CanvasResponse, PulseError>`. DiscoveryInput includes timezone, refresh; CanvasInput includes cursor, timezone, refresh. Response types live in domain/pulse.rs.

- [ ] Add a fake clock, fake ranking provider and query observer to test cache TTLs, forced refresh, concurrent identical requests, writes during aggregation and model failure. Freeze clock and data independently.

```rust
#[tokio::test]
async fn repeated_discovery_uses_one_model_call() {
    let fixture = DiscoveryFixture::with_fake_clock().await;
    fixture.discover(false).await;
    fixture.discover(false).await;
    assert_eq!(fixture.model_calls(), 1);
}
```

Define DiscoveryFixture around PulseService with an injected `RankingCharts` trait implementation and cache clock. Add separate cross-user cached-result and revoked-access tests, not just cache-key equality assertions.
- [ ] Implement durable cache entries keyed by actor/access fingerprint, global and per-user revisions, canonical definition, resolved period, timezone and discovery/compiler version. Recheck access before cache lookup. A revision read is part of existing inventory/canvas metadata reads.
- [ ] Use a process-local single-flight map capped at 128 active computations and durable cache rows capped at 64 entries per user. Coalesce across instances with transactional advisory locks for identical hashed requests; recheck cache after lock acquisition. Use transaction-local statement timeout of five seconds for interactive aggregates. Locks are released on cancellation.
- [ ] Discovery cache expires after 15 minutes; results after 60 seconds; transient failure records after five seconds. Expired entries are removed during bounded cache writes, at most 64 rows per actor. Never turn an exception into an empty successful result. Do not cache a computation under a revision observed after it started.
- [ ] Generate deterministic recipes first, then make at most one Gemini structured call to rank/name allowed candidates and propose compatible definitions. Strictly validate returned identifiers and definitions; no SQL or invented metrics. On failure return deterministic candidates. Execute at most four preview groups, return up to six supported candidates, and omit saved/dismissed hashes.
- [ ] Save definitions only after current validation in one transaction; lock/check revisions so revocation cannot race persistence. Duplicate idempotency key with the same request returns the existing chart; conflicting content returns conflict. Persist full provenance and measurement qualification. Commit this task after cache, save and concurrency tests pass.

## Task 5: Versioned API and compatible integrations

**Files:** Create `services/api/routes/pulse.rs`; modify `services/api/routes/mod.rs`, `router.rs`, `state.rs`, `main.rs`, `openapi.rs`, `contracts/openapi.json`, `contracts/openapi.base.json`, desktop `src/features/api.gen.ts`; add API integration tests alongside pulse tests.

**Interfaces:** GET `/v1/me/pulse/canvas`, GET `/v1/me/pulse/measurements`, POST `/v1/me/pulse/suggestions`, POST `/v1/me/pulse/preview`, POST `/v1/me/pulse/charts`, POST `/v1/me/pulse/dismissals`. Existing charts/boards routes remain available. GET canvas returns saved definitions and data in one response with next_cursor and legacy board summaries.

- [ ] Test request validation, auth errors, source revocation, mismatched idempotency requests, pagination and cold/warm query bounds through handlers.
- [ ] Wire one shared PulseService into API state. Map validation to 422, forbidden access to 403, idempotency conflict to 409 and transient unavailable data to 503. Expose human-readable errors without SQL internals.

```rust
pub async fn preview_pulse(
    State(state): State<PulseApiState>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<PulseDefinition>,
) -> Result<Json<PulseResult>, PulseApiError> {
    state.service.preview(&actor, input).await.map(Json).map_err(Into::into)
}
```

- [ ] Add legacy-to-canvas adaptation for existing charts without changing their saved IDs or legacy timestamp semantics. Keep delegated board reads on their existing consent-limited path; no automatic inclusion of the new personal canvas in Share to Action grants. Add regression fixtures for existing legacy board reads.
- [ ] Regenerate OpenAPI and desktop types using the repository's established contract generation process. Verify definitions are structurally consistent and the generic desktop HTTP port accepts all new routes; add Tauri route allowlist entries only if that port requires them.
- [ ] Assert typical cold six-chart requests use 3–5 statements, hard cap six before pagination, and warm responses have zero aggregate scans. Exclude authentication but include cache persistence operations in reported totals; any cache bookkeeping that exceeds the budget must be combined into existing statements or separately reported as a spec deviation before claiming the target. Commit this task.

## Task 6: Empty canvas, manual creation and suggestion previews

**Files:** Update `src/components/pulse/pulse-view.tsx`, `board-view.tsx`, `src/features/pulse/api.ts`, `types.ts`; create the desktop files listed under Files and ownership. Use the existing Button, Dialog, Card and live platform port.

**Interfaces:** `pulseApi.getCanvas`, `listMeasurements`, `discover`, `preview`, `saveChart`, `dismiss`; `PulseChartCard({chart,result})`; `usePulseCanvas()` returns items, legacyBoards, loading, refreshing, error, reload and pagination state.

- [ ] Extract Recharts rendering from SingleChart into PulseChartCard. Preserve legacy charts and add unit-aware labels, gaps, source, quality and freshness. Use the same card for previews and saved charts.
- [ ] Implement the plus dialog and both complete branches. Empty success renders one plus control with accessible label Add to Pulse; no decorative empty-state copy. Loading and failure remain explicit.

```tsx
<Button aria-label="Add to Pulse" onClick={() => setAdding(true)}>
  <Plus aria-hidden="true" />
</Button>
```

- [ ] Suggestions calls automatic discovery with `Intl.DateTimeFormat().resolvedOptions().timeZone`; it does not require schemas. Show source, coverage, qualification, real preview, period/bucket controls, dismiss and Add. Changing settings cancels stale preview responses using a request generation token.
- [ ] Manual creation selects a permitted measurement, compatible dimensions and period, previews, then saves with the same service. Save uses one idempotency key per confirmed definition and retains it across network retry. No board-name input is required.
- [ ] Subscribe through `platform().live.subscribe` while visible; invalidate relevant sources, coalesce refresh to once per five seconds, unsubscribe on unmount and stop hidden-tab refresh. On visibility restoration perform one freshness check. If live event details cannot identify source, debounce a canvas refresh rather than polling.
- [ ] Add UI verification for successful empty state, network error, no supported suggestions, real preview, estimate labels, manual save, duplicate click, stale preview, legacy board navigation, visibility behavior and keyboard navigation. Use a deterministic platform-port harness, isolated from real-account data; do not claim native validation from that harness.
- [ ] Run desktop `npm run build` and changed-file ESLint/format checks. Inspect the native empty/add/suggestion flow with Computer Use; if unavailable, explicitly retain the native appearance limitation. Commit only task files.

## Task 7: Completion evidence and performance report

**Files:** Add `docs/pulse-discovery-verification.md`; update task checkboxes here. Fix regressions in the task that owns the behavior.

- [ ] Run `cargo fmt --check`, targeted pure/database/API tests, `cargo check --all-targets`, desktop build, changed-file lint and `git diff --check`. Run isolated integration tests serially if their shared fixtures require it. Do not broaden testing without a new failure or concern.
- [ ] Seed representative isolated datasets at 1,000 and 100,000 spans with six compatible and six heterogeneous charts. Capture executed statements, scanned rows, p50/p95, cache hits and pool wait for cold/warm/repeated refresh; record hardware and fixture size. No production performance inference from fixtures.
- [ ] Confirm lifecycle scenarios: edit/delete after preview, revoked source before save, same idempotency key replay, sparse history, provider failure, cache eviction, restart, and concurrent requests. Run existing delegated-integration regression checks.
- [ ] Record local implementation, test results and native validation separately from push/deployment/live verification. No push or deploy without an explicit request. Report any inaccessible live-account inventory and performance measurements as missing evidence.

## Plan self-review

Every spec section maps to a task: UX (6), discovery/profiling (1–2,4), execution (3), persistence/access/caching (2,4–5), database budgets (3–5,7), compatibility (5–6), verification (7). Review Focus cases are assigned to Tasks 1–5. All cross-task contract names are defined above. New source examples remain contract-based until authenticated live inventory is inspected.

Execution recommendation: native implementation in this chat. The tasks share compiler,
cache and API contracts; one implementer can keep those interfaces consistent. An
independent final review should check access isolation, measurement semantics and
query budgets after checks pass. Subagent-driven execution is an available alternative
only if selected by the user; no agents have been started.

## Implementation status

The original checklist is retained as the proposed validation plan; it is not a claim that every proposed experiment was performed. Completed implementation, substitutions and actual evidence are recorded below.

## Completion evidence

Implemented and locally verified. See [verification and implementation rulings](../../pulse-discovery-verification.md) for consolidated module choices, review fixes, query budgets and live-validation limits.
