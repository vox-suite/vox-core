# Desktop Timeline API and database audit — 2026-10-16

## Scope and evidence

Reviewed the current desktop API adapter, day store, loading hook, Rust HTTP transport, Core router, Timeline service/repository, and database migrations. Existing uncommitted feature work was preserved. This audit changes only Timeline reads and adds indexes, database regression tests, and a reproducible benchmark.

The native desktop transport and server are Rust; Timeline view state and rendering use TypeScript/React inside Tauri. The native transport already reuses a single reqwest client, so rebuilding HTTP clients was not a bottleneck here.

Validation uses isolated local PostgreSQL 17 with pgvector, the full Core migrations, and synthetic data. No production database, authenticated production API, native UI, network latency, or production query plans were measured. Source review does not establish runtime correctness for every HTTP endpoint.

## Endpoint review

| Operation | Current behavior and review result |
|---|---|
| GET groups | Small static catalog; loaded once by the desktop. No measured reason to add another server cache. |
| GET event-types | Published global and user-owned types; small catalog. Tenant filter reviewed in source. |
| POST event-types | Transaction, schema validation, unique ownership/version constraints. Reviewed in source; not exercised by the new tests. |
| POST events/query | 30-row cap plus one lookahead, descending timestamp/UUID cursor, filters before pagination, evidence fetched in one batch. Optimized and exercised against Postgres. |
| POST events/days | Timezone-aware range aggregation, per-category counts, day hash, global revision. Now returns summaries and global revision in one SQL statement/snapshot. Empty ranges retain the user's global revision. |
| POST events/counts | Legacy timezone-aware aggregate; still compatible with summaries in the regression fixture. |
| POST events | Schema validation, tenant checks, dedupe/upsert, evidence, finance normalization, invalidation. Reviewed in source; ingestion and import throughput were not benchmarked. |
| PATCH event | User-scoped mutation and revision increment. Existing feature edits retained; not independently exercised by these tests. |
| DELETE event | User-scoped soft deletion. Exclusion from day counts, day-hash changes, and explicit deleted-state query verified in Postgres. |

Timeline routes are merged under the actor-authentication middleware. Repository queries always constrain event/evidence reads by user. This was reviewed in source; the storage tests also verify isolation between two synthetic users.

## Implemented improvements

1. Build the event query with bound parameters for supplied filters only. Optional `OR parameter IS NULL` predicates prevented a generic prepared plan from treating date and cursor bounds as index conditions. The active-state predicate is a fixed SQL literal, allowing a partial index even with generic plans. Only joins required by the supplied group/type/spending filters remain. Search, merchant and category values stay bound parameters.
2. Add active-event indexes on `(user_id, occurred_at DESC, id DESC)` and `(user_id, group_id, occurred_at DESC, id DESC)`. Both match the complete cursor ordering, including events sharing a date-only timestamp. Replace the redundant active user/time index; keep existing non-active and type indexes.
3. Return day summaries and the global revision in one statement, eliminating one database round trip and the possibility of reading the global revision from a later snapshot.

## Query benchmark

200,000 synthetic active transaction events for one user across 180 dates. About 1,111 events share each date-only timestamp. One-day request, 31 rows including lookahead. Three executions per case; warm local database; EXPLAIN ANALYZE with per-node timing disabled.

The baseline is the pre-optimization repository query with its original indexes. The revised SQL is the unfiltered day-page shape emitted by the new builder. The baseline has no search predicate; the search filter in pre-existing working-tree edits is absent from this benchmark. No search performance claim is made.

| Planning mode | Baseline execution times | Revised execution times |
|---|---|---|
| Forced generic | 13.983 / 13.567 / 14.172 ms | 0.061 / 0.041 / 0.025 ms |
| Forced custom | 1.493 / 0.847 / 1.011 ms | 0.050 / 0.024 / 0.020 ms |

The generic baseline used a bitmap scan with a 200,000-entry index result, filtering and sorting. Its top plan node reported 9,377 shared buffer hits. The revised plan used the active cursor index, returning 31 rows with 34 buffer accesses and no sort. Forced generic mode demonstrates a prepared-plan risk; it does not prove the production planner currently chooses that plan. The custom-plan comparison also improved in this fixture.

A later rerun of the final reproducible script under concurrent local build activity measured 26.152 / 39.360 / 35.476 ms before and 0.108 / 0.099 / 0.133 ms after. Absolute microsecond timings vary with local load; the plan shape and work reduction are the stronger evidence.

These are SQL execution times, not API p50/p95, desktop loading times, or promised production speedups. Evidence fetching, authentication, serialization and network time are additional. Category-filtered, spending, search, wide-range summary, and concurrent-request throughput were not performance-benchmarked.

## Regression checks

The new Postgres tests cover complete cursor-index shape, three pages of equal-timestamp events without gaps/duplicates, combined group/type/spending/merchant/category/search filters, ordinary unfiltered day reads, bound SQL-like merchant text, evidence attachment, tenant isolation, legacy counts, Pacific day boundaries, empty users/ranges, day revisions after deletion, and querying an explicit non-active state. The existing spending-before-pagination test is also run.

API binary compilation (`cargo check --bin vox-core-api`) and `git diff --check` passed.

Focused Postgres checks: 3 passed (two new tests plus the existing spending-filter test). The regular Rust library suite: 36 passed, 30 ignored, 0 failed.

The index assertion failed before the migration and passed afterwards. The original behavior test also exposed the account-deletion trigger issue below during fixture cleanup; cleanup now deletes test events before test users.

## Remaining findings and follow-up priorities

- **Account deletion correctness:** deleting a user with timeline events failed in the local database. The event-delete transition trigger tries to recreate a `timeline_revisions` row after the parent user has been removed, violating its user foreign key. Reproduced during test cleanup. This is separate from read performance and remains unfixed.
- **Late desktop responses after sign-out:** `TimelineDayStore.reset()` clears maps, but pending hydration/count/page responses have no session-generation guard and can repopulate them; `inflight` is retained. This is a source-review finding, not a reproduced cross-account UI leak. Fix with session-generation checks and a dedicated delayed-response regression test.
- **Repeated full-range hashing:** day summaries still scan every active event in the requested range and sort event IDs for revision hashes. A 366-day range can be expensive for large histories. Measure summary p95 and range cardinality before introducing persisted per-day revisions; arbitrary device timezones make that design nontrivial.
- **Broad substring searches:** `strpos(lower(...))` merchant/title search cannot use ordinary B-tree indexes. Narrow by date/type first; profile real search workloads before adding expression/trigram indexes and their write cost.
- **Pool pressure:** API pool defaults to ten connections. Each desktop can load six day pages concurrently; each nonempty page makes an event and evidence query. Increasing client concurrency or pool size without load measurements may worsen contention.
- **Write work:** ingestion compiles JSON Schema per event, writes evidence one row at a time, and updates per-user revision/invalidation triggers. Large imports should be measured separately before batching or caching validators.

## Reproduce and deploy

Use a disposable database named `timeline_perf` with pgvector. Apply `scripts/ci-supabase-bootstrap.sql`, then set `VOX_DESKTOP_TEST_DATABASE_URL` and run:

```sh
cargo test --test timeline_performance --test desktop_timeline -- --ignored --test-threads=1
```

Run `scripts/timeline-read-benchmark.sql` through psql against that database. It seeds only synthetic data, temporarily restores the original indexes inside a rolled-back transaction for the baseline, then measures the new query/indexes. It requires the database name `timeline_perf`. Substitute `force_custom_plan` for `force_generic_plan` for the second case. The disposable local container used here was `vox-timeline-perf-20261016` on localhost port 55439; it was removed after verification.

This audit applied the index migration only to the isolated local database. Regular CREATE INDEX runs transactionally and blocks writes while building; schedule a suitable production migration window or use a separately managed concurrent-index rollout for a large live table. The extra group index adds storage/write work. This audit did not run a production migration, deployment, commit or push. The shared checkout changed during the audit: another process committed the in-progress cursor query/index changes in `70a37ac`; the later summary optimization, expanded tests and report remain working-tree changes.
