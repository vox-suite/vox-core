# Projects API and database performance audit — 2026-10-16

## Scope and evidence

Reviewed desktop Projects API/caching/hooks, native Rust HTTP and investigation gateways, local SQLite graph reads, Core HTTP routes, repository, project schema/validation, and related Plan attachment code. Runtime checks used disposable PostgreSQL 17 with pgvector and all Core migrations, plus synthetic fixtures. Existing Timeline changes were preserved.

Production API latency, production database plans, real provider/model calls, native UI behavior, and real repository indexing were not measured. A source review of every route is different from exercising every authenticated HTTP operation.

## Endpoint review

All paths below start with `/v1/me/projects`; they are merged under the actor-authentication middleware.

| Endpoint | Behavior and verification |
|---|---|
| GET `/count` | Counts active projects and hashes IDs/revisions; ignores large JSON. Reviewed and benchmarked directly in SQL. |
| GET `/` | 50 project summaries ordered by updated timestamp and UUID. Optimized cursor predicate and JSON projection; fixed false next cursor on exactly 50 items. Storage behavior exercised in Postgres. |
| POST `/` | Validates name, project structure and 1.5 MB document limit, inserts and notifies. Repository creation exercised; HTTP input/status handling reviewed in source. |
| GET `/{id}` | Loads full document for the authenticated owner only, excluding archived projects. Exercised in Postgres. |
| PUT `/{id}` | Full-document optimistic update with revision increment. Verified successful updates, foreign-user rejection, stale revisions, and two simultaneous updates where exactly one succeeds. |
| DELETE `/{id}` | Owner-scoped soft archive and revision increment. HTTP mutation reviewed; archived read/write exclusion exercised with SQL fixtures. |
| POST `/{id}/plans/{plan}/attach` | Checks project ownership, then updates an owned, non-dropped, non-running Plan. Replaced full-document ownership read with EXISTS; attachment not exercised end to end. |
| POST `/{id}/chat` | Legacy server ideation path: bounded message/excerpts, project/citation checks, model request, then revision-checked save. Source-reviewed; no paid model call made. |
| GET `/{id}/agent` | Reads model availability/configuration after ownership check. Now uses EXISTS. |
| POST `/{id}/agent/{action}` | Restricted model-action gateway with request-size/rate limits and streaming response. Now uses EXISTS and a reusable provider HTTP client. Provider behavior reviewed in source and compiled; no live TLS/SSE test performed. |

The desktop list and native graph browser are different paths: list/detail metadata uses Core/Postgres; source symbols and relations use local SQLite.

## Changes made

1. **Cursor bounds reach the index.** Separate first-page and subsequent-page SQL replaces the optional `before IS NULL OR ...` expression. Under forced generic planning, the old deep-page query scanned past 1,901 newer rows. The new query uses the cursor tuple as an index condition. The existing partial index `(user_id, updated_at DESC, id DESC) WHERE NOT archived` already matches the ordering, so no new index or migration is needed.
2. **Remove heavy JSON sections once.** Replace three chained JSONB subtractions with `data - ARRAY['nodes','messages','activity']`. Preserve template, notes, sources and any other metadata. The heavy sections remain absent before Rust decoding, with serde supplying the same empty-array defaults.
3. **Correct final-page detection.** Fetch 51 summaries, return at most 50, and emit a cursor only if the extra row exists. Previously exactly 50 projects caused an unnecessary empty next-page request. The repository's lookahead contract is documented; its only production list caller is the HTTP handler.
4. **Lightweight ownership checks.** Agent configuration, each investigation model request and Plan attachment now query EXISTS instead of transferring and decoding the whole project. Checks still bind both user and project ID and exclude archived projects. Detail reads and chat retain full documents because they need the content.
5. **Reuse the provider connection pool.** Investigation model forwarding now shares one reqwest client per API process. The no-redirect policy and 180-second timeout remain in place. Provider credentials are attached per request. This removes repeated client creation and enables connection reuse; no network speedup is claimed without live measurements.

## Measurements

PostgreSQL fixture: 2,000 synthetic projects for one synthetic user, each with 32 context nodes and 32 messages; average expanded JSON about 41,776 bytes. Deep page uses a cursor near the oldest 100 projects. Warm local measurements; EXPLAIN ANALYZE with per-node timing disabled. The new query fetches 51 rows versus the old query's 50 to support correct lookahead.

| SQL query | Before | After |
|---|---|---|
| Deep page, forced generic plan, three executions | 0.700 / 0.691 / 0.603 ms | 0.197 / 0.204 / 0.192 ms |
| Deep page, forced custom plan, three executions | 0.581 / 0.545 / 0.462 ms | 0.204 / 0.191 / 0.201 ms |
| First page, forced generic plan, two executions | 0.597 / 0.470 ms | 0.202 / 0.193 ms |
| First page, forced custom plan, two executions | 0.537 / 0.443 ms | 0.203 / 0.184 ms |

The custom baseline already used the cursor index, so its improvement principally demonstrates the JSON projection change. The generic baseline additionally filtered 1,901 rows; the revised generic plan uses an index range. These are deliberate plan-mode comparisons, not evidence that production currently selects generic plans.

SQL execution alone hides the ownership-check savings: SELECT * and EXISTS both take only a few microseconds to locate a row, while EXPLAIN excludes row transfer and Rust document decoding. The repository benchmark alternated both reads for 35 iterations, discarding five warmups. In a local Rust debug build, 30 samples measured:

| Repository operation | Median | p95 sample |
|---|---|---|
| Previous full-project ownership read | 1,154 microseconds | 1,233 microseconds |
| EXISTS ownership check | 514 microseconds | 620 microseconds |

A previous run measured 1,195/528 microseconds at the median. Absolute timings vary with local load and build mode. This is a moderate-size fixture, not a worst-case document test or an API latency promise. The count/revision query took 0.55–0.56 ms on this fixture; no change was justified from that measurement.

Raw SQL plans are in `docs/projects-query-plans-2026-10-16.txt`. The reproducible SQL is `scripts/projects-read-benchmark.sql`.

## Verification

- The new pagination lookahead test failed against the original repository (50 returned, 51 required), then passed with the change.
- Postgres tests: 3 passed, including the ownership benchmark. They cover summary field preservation, excluded heavy sections, equal-timestamp ordering, page boundaries, missing/foreign cursors, tenant isolation, owned/missing/archived existence, creation/read/update, stale revisions and concurrent write conflicts.
- Rust library suite: 36 passed, 30 ignored, 0 failed. Ignored suites require their own database fixtures; they were not implicitly run.
- API binary compilation (`cargo check --bin vox-core-api`) and `git diff --check` passed.
- The actual HTTP next-cursor decision was reviewed in source; the test exercises repository lookahead and boundary sizes, not an authenticated HTTP response.

## Remaining findings

- **Project cursors are mutable.** The cursor contains only an ID; the server looks up its current updated timestamp. Editing or archiving projects between page requests can change page boundaries. Unknown/foreign IDs currently yield an empty page. A timestamp-plus-ID cursor would need a compatible contract change and dedicated tests.
- **List cache refreshes can overlap.** Each `projectList` invocation calls `/count`, and concurrent consumers/live updates have no shared in-flight request. Overlapping refresh/page calls can duplicate work or replace appended pages with an older result. Account-epoch checks are already present. This is a source-review finding; a delayed-response client test should precede a fix.
- **Count/revision remains linear.** Every count request aggregates every active project ID and revision. The synthetic result is small enough to leave it alone; measure high-cardinality tenants and count endpoint p95 before adding a persisted revision table.
- **Full-document writes.** Saving any note or diagram rewrites the project JSON and returns it. At the 1.5 MB input limit this can affect bandwidth, JSON CPU, TOAST storage and concurrent-edit conflicts. Splitting messages/diagrams or patch semantics is a larger product/storage change; no evidence here justifies doing that speculatively.
- **Local graph pagination scans all matches.** `src-tauri/src/projects/graph.rs::page` reads and filters every matching SQLite symbol to compute the exact total, retaining only 80. Large local indexes may be slow despite fast server APIs. Any SQL paging optimization must preserve approved-file membership and path containment checks; local source/index benchmarks were not run.
- **Plan attachment has a race.** The ownership check and Plan update remain separate statements. Concurrent archival can occur between them, as it could before this optimization. No transaction/locking behavior changed in this audit.
- **Provider and native runtime costs are separate.** Connection setup, model generation, streamed delivery, local source scanning, diagram compilation and rendering were not benchmarked.

## Reproduce and rollout

Use a disposable pgvector PostgreSQL database named `projects_perf`. Apply `scripts/ci-supabase-bootstrap.sql`; set `VOX_PROJECTS_TEST_DATABASE_URL` and run the two functional tests first (they apply Core migrations). Seed/measure with `scripts/projects-read-benchmark.sql` through psql, then run:

```sh
cargo test --test projects_performance -- --ignored --test-threads=1 --nocapture
cargo test --lib
cargo check --bin vox-core-api
```

The benchmark test requires the synthetic fixtures; without them run the functional tests by name first. Replace `force_generic_plan` with `force_custom_plan` to reproduce the second planning case. The disposable container used here was `vox-projects-perf-20261016`, localhost port 55440, and was removed after verification.

Changes are local source changes. No schema migration, production deployment, commit or push was performed by this audit. Client API shapes are unchanged.
