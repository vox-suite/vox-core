# Spaces and Insights performance audit — 2026-10-16

## Evidence and scope

Insights is named Pulse in the Rust code and `/v1/me/pulse` API. Reviewed desktop API adapters, list/result caches and hooks, Rust routes, repositories, chart execution/cache paths, graph reads, and database migrations. Existing Projects work was preserved.

Runtime checks used isolated PostgreSQL 17 with pgvector, all Core migrations and pg_stat_statements, synthetic data only. Production query plans, production API latency, native UI behavior, agent runs, model calls, committing real Plans and provider delivery were not exercised. Reviewing a route does not establish complete authenticated HTTP correctness.

## Implemented changes

### Spaces

- Summary requests now select `'{}'::jsonb AS agent_spec` in SQL. Previously the full specification crossed the database connection, was decoded by Rust, and was then replaced with `{}` in the HTTP handler. Requests without summary still receive the full specification; response shapes are unchanged.
- List SQL includes only supplied search/state predicates. Search text remains bound, and sort expressions remain an allowlist. This exposes the actual filters to the planner, preserving active/committed filtering, dropped exclusion and offset behavior.
- Added partial indexes `(user_id, created_at DESC, id DESC)` and `(user_id, updated_at DESC, id DESC)` for non-dropped spaces. The existing created index lacked the UUID tie-breaker; there was no corresponding updated-order index. The old full index is retained for callers that include dropped rows.

### Insights

- Daily/dimension rollups now use a single typed-array UNNEST INSERT instead of executing one INSERT per result row. The existing transaction, per-metric advisory lock, revision recheck, delete-and-replace behavior, units, dimensions and valid-value metadata remain intact. Numeric values use the same float-to-numeric path as before. All arrays come from the same ordered row collection.
- Removed the duplicate result-cache write in `PulseService::results`; `execute_cached` already owns the cache write. Cache keys, TTLs and force-refresh rules were preserved. Returning a concurrently computed cached result no longer rewrites its created timestamp in the service layer.

## Measurements

### Spaces query plans

10,000 synthetic spaces, 9,000 not dropped, about 8 KB agent specifications, 25-row first page ordered by updated timestamp. Before/after use the original and new indexes respectively. Three EXPLAIN ANALYZE executions per case, per-node timing disabled.

| Planning mode | Before | After |
|---|---|---|
| Forced generic | 1.739 / 1.704 / 1.982 ms | 0.015 / 0.012 / 0.012 ms |
| Forced custom | 1.655 / 1.769 / 1.579 ms | 0.017 / 0.012 / 0.012 ms |

The old plan scanned/filter-sorted the user's list, excluding 1,000 dropped rows. The new plan reads 25 entries directly from the partial updated-order index without a sort. The warm SQL timings are very small; the work reduction and plan shape are stronger evidence than a production speedup ratio. Forcing generic/custom plans is a reproducible comparison, not evidence of the live planner's choices.

A separate Rust debug-build benchmark compared full-spec versus summary projection using the SAME new index. After five warmups, 20 alternating samples measured median repository reads of **1.966 ms full-spec versus 0.802 ms summary**. This includes database transfer and Rust decoding, not HTTP authentication/serialization/network/UI time. It isolates specification omission from the index improvement.

### Insights statement counts and timing

Synthetic music events across 90 dates, one 90-day chart. pg_stat_statements verified actual normalized SQL call counts around a cold preview; the measurement inventory was primed before timing, and result/rollup caches were cold.

| Metric | Before | After |
|---|---|---|
| Rollup INSERT statements | 90 | 1 |
| Result-cache write statements | 2 | 1 |
| Local debug cold preview | 42.986 ms | 13.802 ms |
| Warm preview median | 3.727 ms | 3.537 ms |

Intermediate revised runs measured 15.060–16.931 ms cold and 3.351–4.134 ms warm. Warm timings fluctuate and do not establish a material warm-cache improvement. The repeatable improvement is fewer cold-path database round trips. Inventory construction from a large history and multi-chart/series throughput were not benchmarked.

Raw Spaces plans are saved in `docs/spaces-insights-query-plans-2026-10-16.txt`; SQL fixtures/plans are reproducible with `scripts/spaces-insights-read-benchmark.sql`.

## Endpoint coverage

Both route groups are merged under actor authentication. User-facing parent lookups bind user identity; graph/node helpers taking only a space ID depend on their callers performing the ownership check.

### Spaces (`/v1/me/spaces`)

| Operation | Review and runtime evidence |
|---|---|
| GET list | Summary/full projection, default/updated/title order, active/committed state, case-insensitive search, literal SQL-like search, offset and foreign-user exclusion exercised in Postgres. New sort indexes verified. |
| GET count | Active count and max-updated revision read reviewed; dropped rows excluded in the fixture. |
| POST create | Transaction creates parent, initial node and durable job; optional project ownership checked; runtime spawn and notification reviewed. Repository creation exercised; no agent run triggered by the tests. |
| GET graph | Parent ownership plus node and edge reads. Owned/foreign/empty graph cases plus a two-node, one-edge graph exercised. |
| PATCH rename / DELETE drop | User-scoped SQL and notification reviewed. Dropped exclusion covered through fixtures; HTTP mutations not exercised. |
| POST chat / stop / node retry | Runtime/job scheduling, state and ownership checks reviewed; execution/cancellation not run. |
| POST commit | Graph/plan checks and collection/span persistence reviewed; no external commitment or live execution performed. |
| GET messages | Ownership lookup then complete history read. Source-reviewed; unbounded-history performance remains open. |
| PATCH node | Ownership/state validation and repository update reviewed; editing/stream behavior not exercised through the UI. |

### Insights (`/v1/me/pulse`)

| Operation | Review and runtime evidence |
|---|---|
| GET measurements | Inventory/catalog and scoped cache keys reviewed. Current Timeline-backed catalog and isolation from a second user exercised. |
| POST preview | Batched rollups and one cache write verified using pg_stat_statements. Counts, categorical dimensions, missing numeric values remaining null, revision invalidation after changing event data, and warm reuse of computed timestamps exercised. |
| GET canvas / individual chart result | Shared execution/cache/revision pipeline reviewed; calculation path covered by preview tests. Saved-chart HTTP responses were not separately exercised. |
| GET chart list/count/detail | Limits, order, ownership and revision reads reviewed. No additional index proposed for the existing 50-saved-chart limit. |
| POST save / PATCH / DELETE chart | Idempotency, per-user save lock, update row lock and invalidation triggers reviewed in source. Live authenticated CRUD not exercised. |
| POST suggestions | Inventory, discovery cache, advisory lease, model budget and validation reviewed. No paid model request made. |
| GET/POST/DELETE dismissals | User-scoped reads/writes and definition checks reviewed. |

## Tests and remaining findings

Fresh verification: 4 dedicated database tests passed; the library suite reported 36 passed, 30 ignored, 0 failed. API compilation (`cargo check --bin vox-core-api`) and `git diff --check` passed.

The new cold-path test failed before the fix with 90 rollup writes and two result-cache writes. The new Spaces index check also failed before migration. Functional checks cover tenant isolation, full versus summary metadata, filters, same-timestamp ordering, offsets and dropped rows. Insights checks preserve missing numeric values as gaps and verify categorical totals after a revision change. The benchmark uses current timeline_events fixtures rather than legacy spans-based fixtures.

Source-review follow-ups, not claims of reproduced production faults:

- **Spaces polling and history:** `useSpace` reloads the graph and all messages every four seconds while visible, even for settled Plans. Graph reads use three sequential SQL statements; messages add a parent read plus history read. Consider activity-aware polling and bounded history after defining reconnect/worker-update guarantees. Merely disabling polling could hide durable-worker progress.
- **Spaces pagination remains OFFSET-based.** Large offsets still scan preceding index entries. The API's omitted limit defaults to effectively unbounded. Changing either requires caller compatibility and UI paging tests.
- **Spaces search/title sort:** substring search is not a normal B-tree lookup; title sorting still needs a sort. Neither was performance-benchmarked, and no speculative trigram/title indexes were added.
- **Graph snapshot consistency:** parent, nodes and edges are separate reads, so a concurrent worker can change the graph between them. No snapshot/locking change was introduced here.
- **Client cache/session races:** Spaces and Insights hydration reset their shared maps but lack a session-generation check on all late responses. Request tickets guard many UI updates but some shared cache writes occur before the ticket check. Reproduce with delayed responses before fixing this separately.
- **Insights metadata overfetch:** `PulseRepository::metadata` makes six sequential reads, including all unexpired per-user cache payloads and all saved definitions. Measurements, preview, save and dismiss can load more than they need. Targeted per-key reads would need careful cache/principal tests.
- **Cold inventory work:** different chart-result requests can each miss and rebuild inventory; per-chart locks do not coalesce inventory across chart IDs. Discovery holds an advisory transaction while other pool work/model ranking runs. Load-test pool contention before changing locking.
- **Warm request count:** chart-count service still makes count and revision reads separately. A statement combining them could remove a round trip and give them a consistent snapshot; not changed in this pass.
- **Saved-chart correctness:** `get_chart` currently returns `is_pinned: false` rather than the stored value; idempotent-save responses do likewise. PATCH definition handling does not call the same catalog validation as save, so an invalid changed definition can persist and later become unavailable. These source findings are separate from this performance change.
- **Existing test drift:** several ignored Pulse database tests still insert legacy spans or refer to pulse_saved_charts, while the current repository reads timeline_events and pulse_charts. They were not silently counted as passing coverage of the current implementation.

## Reproduction and rollout

Use a disposable pgvector PostgreSQL database named `spaces_insights_perf`, start PostgreSQL with `shared_preload_libraries=pg_stat_statements`, create that extension, and apply `scripts/ci-supabase-bootstrap.sql`. Set `VOX_SPACES_INSIGHTS_TEST_DATABASE_URL` to it. Run the functional Spaces test by name once to apply all migrations, then execute the SQL benchmark through psql to seed the list fixture. Run:

```sh
cargo test --test spaces_insights_performance -- --ignored --test-threads=1 --nocapture
cargo test --lib
cargo check --bin vox-core-api
```

The tests use pg_stat_statements call deltas and must run serially without another workload in this database. The fixture benchmark requires the seeded synthetic user. Replace force_generic_plan with force_custom_plan for the second SQL comparison.

The Spaces index migration was applied only locally. Regular CREATE INDEX is transactional and blocks writes while building; schedule the production migration accordingly or manage a concurrent-index rollout for a large table. The two indexes add write/storage cost. Insights needs no schema change. No production migration, deployment, commit or push was performed by this audit. The disposable container `vox-spaces-insights-perf-20261016`, localhost port 55441, was removed after checks.
