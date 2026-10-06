# Pulse discovery and chart execution

Date: 2026-10-06
Status: Written design for review; product implementation has not started.
Scope: vox-core and vox-desktop. Existing provider ingestion remains authoritative.

## Intent

Pulse starts with an empty canvas and one accessible plus button. The plus
button offers Create manually and Suggestions. Suggestions automatically discover
useful charts from the user's permitted spans and connection data. The user sees
real previews and chooses what to save. No category selection or board naming is
required before discovering or adding a chart. Saved charts update as data arrives.

Existing boards remain readable and retain their identifiers and charts. The
canvas treats a chart as the primary item; existing board storage can group these
items internally. Legacy boards remain available through an optional grouping view.

## Verified starting point

- Desktop Pulse currently requires category selection and board naming.
- POST /v1/me/charts/suggest accepts at most eight selected schemas and reads
  five recent data objects for each. It calls Gemini and returns proposed charts.
- QuerySpec contains only metric_field, aggregation, and group_by.
- Suggestions currently validate field presence, rather than measurement meaning,
  numeric types, full source ownership, coverage, or units.
- Board metadata and board data are separate requests. Each performs two initial
  repository reads. Data then performs one schema read and one aggregation for
  each valid chart: approximately 4 + 2N database statements per board opening,
  excluding authentication. Six charts therefore use approximately 16 statements.
- Aggregations have no date bound and use start_at with created_at fallback.
  Numeric-field count excludes events whose selected field is not numeric.
- The existing spans index includes (user_id, schema_id, start_at DESC).
- Recharts already renders chart results; no new visualization library is needed.

The user's live account inventory has not been inspected. Provider findings below
are source-contract findings, not claims about connected accounts or stored rows.

## User experience

1. Load the canvas. Show loading or retry states while its contents are unknown;
   show only the plus button once a successful load confirms an empty canvas.
2. Plus opens Create manually / Suggestions.
3. Suggestions opens cached candidates immediately when available, with a visible
   freshness indicator. Otherwise show one discovery loading state.
4. Each candidate includes title, short reason, source labels, measured period,
   coverage, measurement qualification, and a preview generated from real data.
5. Allow day/week/month and period changes only where measurement supports them.
6. Add saves a chart atomically and returns it on the canvas. Repeated submission
   with the same idempotency key must not create duplicate charts.
7. Manual creation chooses an available measurement, compatible grouping and
   period, then previews and saves through the same validation path.
8. Dismissed and already-saved definitions are suppressed from normal suggestions.
   A refresh action requests discovery again without silently adding charts.
9. If no chart is supported, explain missing coverage or data. Connector-only
   opportunities are separate from ready-to-add charts and have no invented preview.

## Discovery pipeline

### Inventory and profiling

Core reads permitted schemas, span profiles, and permitted connection metadata.
Profile the user's full recorded date coverage and counts server-side. Compute
field completeness and numeric/categorical summaries over a bounded recent window
(default 90 days, maximum interactive window 365 days). Use a maximum of five
representative samples per selected source, excluding credentials and unnecessary
notes. Include unschematized spans through source/category profiles; discovery
does not create schemas or rewrite existing spans.

Batch source summaries rather than issue one request for every schema. Limit
model context to the 20 best-covered profiles and 40 approved measurement
candidates, while retaining full inventory counts. The UI must not describe this
as exhaustive model inspection of every raw record.

Connection access, sync consent, authorization state, last successful sync, and
capture capability are distinct. Connection presence alone never establishes a
measurement or permission to access provider data. Discovery reads already-synced
data and does not initiate provider refreshes on every visit.

### Measurement catalog

A measurement has a stable identifier, allowed source selectors, numeric or event
semantics, unit, timestamp source, allowed dimensions, allowed time buckets,
quality classification, and coverage requirements. Types include event_count,
numeric_sum, numeric_average, known_interval_duration, cumulative_delta, and
recurring_cost_projection. Each is implemented by a fixed Rust query compiler.

Provider recipes:

- Spotify: recorded plays and artist counts from listen events. Summed reported
  track duration is only an estimated full-track duration, never measured listening
  time. playback_end_known=false prevents measured-hours recipes.
- YouTube: playlist actions and subscription snapshots are distinct from watch
  events. Imported Takeout watch events support counts, not hours, when
  duration_known=false. Channel subscriptions differ from paid subscriptions.
- PlayStation: lifetime totals support per-game totals. Cumulative increases
  describe an observation interval; do not allocate that increase to individual
  days or hours when exact session timing is unknown. Lifetime snapshots and
  incremental activity must not be summed together.
- Expenses: distinguish completed payments, refunds, pending entries and plans.
  Aggregate by transaction date and currency. Mixed currencies produce separate
  series unless a supported conversion policy explicitly exists.
- Paid subscriptions: use explicit amount, currency, interval and active status.
  Monthly normalized cost is a projection, distinct from observed payments.
- Generic sources: enable counts and numeric recipes only after confirming field
  meaning and units. Exclude planned/cancelled activities from actual-use metrics.

### Candidate generation and ranking

Generate dependable candidates from catalog recipes. Gemini may rank them,
write descriptions, and propose additional definitions using allowed measurements
and compatible dimensions. It cannot write SQL or define executable expressions.

Rank by coverage, freshness, usefulness, diversity, and novelty. Return at most
six ready candidates. Zero candidates is valid. Validate every source, field,
dimension, timestamp, unit and calculation before running preview queries.
Return deterministic recipe suggestions if Gemini is unavailable. Treat provider
and span content as untrusted data rather than model instructions.

Cross-source comparisons require overlapping observed periods and compatible
units or separately labelled series. No causal claims. Initial delivery prioritizes
single-source recipes; cross-source comparisons require the same compiler and
coverage validation before appearing.

## Chart definition and execution

Introduce a versioned definition with measurement_id, source selectors, filters,
aggregation, dimension, bucket, period, timezone, unit, quality and display type.
Use an explicit event timestamp policy. Unknown event dates are reported as
undated coverage, not silently assigned the import date. Legacy definitions keep
legacy rendering until converted through validation.

Daily/weekly buckets use the user's timezone, supplied by the client and validated
against supported timezone names. Split known intervals across bucket boundaries;
preserve uncertainty for cumulative observation intervals. Event count counts
events, independent of numeric field presence. Missing capture coverage is a gap;
zero is emitted only for covered periods with zero matching events.

Unify metadata and data in an authenticated canvas/board response. Check ownership
once, load definitions once, and fetch all referenced schemas in one query. Group
compatible definitions into bounded parameterized aggregate queries. Do not
replace database fan-out with an unbounded raw-span download. At most four aggregate
groups execute per cold board request, using bounded database concurrency of two.
Larger boards paginate visible charts. Time series return at most 366 points per
series; categorical series return at most 20 explicit values. For average metrics,
do not produce an unweighted average-of-averages in an Other category.

Preview execution uses this same compiler. Unsupported definitions fail before
persisting. All chart creation occurs in a transaction; do not create a board and
silently skip invalid charts. Save requires full source validity, rather than
silently narrowing a requested source set.

## Database and caching policy

AI runs during discovery only. Chart refresh executes saved definitions.

- Store reusable discovery results by actor, access revision, profile revision,
  timezone and discovery version. Default discovery TTL is 15 minutes.
- Cache chart result sets for 60 seconds by actor/access scope, definition hash,
  timezone, resolved date range and data revision. Keep cache size bounded.
- Coalesce simultaneous identical computation requests. Cache errors separately
  for no more than five seconds; never cache errors as empty successful charts.
- Span creation, edits and deletion advance the relevant data revision. Connection
  changes advance the discovery revision. Schema/access changes invalidate
  definitions and cache keys. Auth/access checks precede cache reads.
- Use existing status events where reliable, with TTL as a fallback; expose
  computed_at, data_as_of and freshness in responses.
- Refresh visible charts when a relevant event arrives, debounced to at most once
  per five seconds while visible. No background polling for hidden Pulse views.
- Manual refresh bypasses result TTL, but still coalesces concurrent work.

Targets, excluding authentication: a warm board request performs no span aggregate
scans; a typical cold board request performs 3-5 statements (definitions/ownership,
schema access, and 1-3 aggregate groups), with a hard upper bound of six statements
before pagination. Discovery uses up to four profiling/catalog queries and four
preview aggregate groups per cold request. Model calls are at most one per actual
discovery job; retries are explicit and bounded.

Query count is not a latency guarantee. Measure scanned rows, execution plans,
p50/p95 latency, pool wait, cache hits, coalescing and model time independently.
Verify existing indexes with EXPLAIN ANALYZE on representative data before adding
new indexes. Keep date predicates compatible with indexes. A partial index for
undated records may be needed for inventory, based on measured query plans.

Do not add materialized daily rollups initially. Add them only if bounded,
indexed aggregate queries miss the agreed latency target on representative data.
Rollups would need correct edit/delete handling, timezone semantics and provenance;
they are not a free substitute for a cache.

## Delivery boundaries and verification

Implementation covers automatic profiling, recipes, AI ranking, validated previews,
manual creation, empty canvas, saved charts, bounded queries and caching together.
Reuse existing authentication, connection policy, chart rendering and storage.
Version APIs and generated desktop types together; preserve existing API consumers,
including Share to Action delegated board reads, without widening their access.

Verification must exercise cross-user isolation including cached results, revoked
access, cache invalidation after edits/deletes, unsupported duration measurements,
currency separation, cumulative totals versus deltas, unknown dates, timezone
midnight boundaries, duplicate saves, Gemini fallback, sparse coverage, and query
count bounds. Compare previews and saved chart results for the same definition.
Run Core checks and meaningful integration tests against an isolated database;
run desktop build/lint and native UI inspection for the empty/create/suggestion
flows. No live account performance or provider coverage claim without live evidence.

No provider API redesign, deployment, push, or new daily-rollup infrastructure is
included by implication. Preserve unrelated working-tree edits.
