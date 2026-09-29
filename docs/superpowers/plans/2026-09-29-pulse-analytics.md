# Pulse Analytics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user pick one or more of their own categories, get a
list of LLM-suggested charts drawn from that category's real data
shape, select some, and save them as a revisitable board rendered with
recharts in a new "Pulse" desktop tab.

**Architecture:** Two new vox-core tables (`chart_boards`, `charts`)
store board/chart metadata and a small fixed-shape `query_spec` per
chart — never a raw query. Five new vox-core endpoints cover listing
categories, one-shot LLM suggestion, board CRUD, and live chart-data
computation. vox-desktop adds a new sidebar tab, a two-step picker
flow, and a board view, talking to vox-core through the existing Tauri
`core_request` command pattern (no direct HTTP from the frontend).

**Tech Stack:** Rust/axum/sqlx (vox-core), Gemini via `rig` (LLM
suggestion call), Tauri/React/TypeScript + shadcn/ui + recharts
(vox-desktop, new dependency).

**Spec:** `docs/superpowers/specs/2026-09-29-pulse-analytics-design.md`
— read it first; this plan implements it and does not repeat its
rationale.

## Global Constraints

- No test files, no code comments, per this session's standing
  instruction.
- No rollup tables, no auto-refresh/polling on the board view, no
  editing a chart's `query_spec` after creation — all explicitly out
  of scope per the spec.
- The LLM only ever produces a `query_spec` (metric field, aggregation
  kind, group-by). It never produces or influences a raw SQL string.
  Every task that touches the suggestion response must preserve this
  boundary.
- Every new vox-core query scoped to a user must filter by `user_id`
  first, matching every existing query in this codebase.
- `cargo build --all-targets` (vox-core) and the frontend's existing
  type-check/lint command must stay clean after every task that
  touches that repo.

## Review Focus

- **A `schema_ids` array containing a category the requesting user
  doesn't own** (someone else's schema id, or a stale id from a
  deleted category) reaching the suggestion or board-creation
  endpoint — every schema lookup in this feature must filter by
  `(user_id = $1 OR user_id IS NULL)` the same way existing schema
  queries do, so a foreign or missing id is silently excluded rather
  than leaking another user's category description to the LLM prompt
  or crashing.
- **A `query_spec.metric_field` naming a `data` property that isn't
  actually numeric for some rows** (mixed-shape data under one
  schema, or a field that's sometimes a string) — the query builder
  must produce zero/skip that row rather than fail the whole chart's
  data call with a cast error.
- **`group_by` naming a categorical field with very high cardinality**
  (e.g. accidentally grouping by a free-text title instead of a
  bounded enum-like field) — the data endpoint must cap the number of
  groups returned (e.g. top N by value, rest omitted) rather than
  returning an unbounded response.
- **The suggestion endpoint called with zero or with a very large
  number of `schema_ids`** — zero should return a clear 400 rather
  than an empty/confusing LLM prompt; a large list should be capped
  (e.g. reject or truncate past a fixed max) rather than building an
  unbounded prompt.
- **A board's chart referencing a `schema_id` whose category was
  since deleted or renamed** — the data endpoint must handle a schema
  lookup miss for one chart by omitting that chart's data (with an
  error marker) rather than failing the entire board's data response.

---

## Task 1: Migration — chart_boards and charts tables

**Files:**
- Create: `vox-core/migrations/20260929000002_chart_boards.sql`

**What it must contain:**
- `chart_boards`: a UUID primary key (default-generated), a `user_id`
  column that is a non-null foreign key to `users(id)` with cascade
  delete, a non-empty `name` text column, and `created_at`/`updated_at`
  timestamp columns defaulting to now.
- `charts`: a UUID primary key (default-generated), a `board_id`
  column that is a non-null foreign key to `chart_boards(id)` with
  cascade delete, a non-empty `title` text column, a `chart_type` text
  column constrained to exactly `line`, `bar`, `pie`, or `area`, a
  `schema_ids` column typed as an array of UUIDs (non-empty — a chart
  must reference at least one category), a `query_spec` JSONB column
  constrained to be a JSON object, and a `created_at` timestamp
  defaulting to now.
- An index on `chart_boards (user_id, created_at DESC)` for the
  boards-list query, and an index on `charts (board_id)` for the
  per-board chart lookup.

- [ ] **Step 1: Write the migration file** with the table/column/
  constraint/index definitions above, matching the SQL style already
  used in this repo's migrations (see
  `migrations/20260929000001_schema_display_tokens.sql` for the
  house style: explicit `CHECK` constraints, `gen_random_uuid()`
  defaults, `now()` defaults).

- [ ] **Step 2: Commit**

```bash
git add migrations/20260929000002_chart_boards.sql
git commit -m "migrate: add chart_boards and charts tables"
```

---

## Task 2: Domain models and storage repository

**Files:**
- Create: `vox-core/src/domain/charts.rs`
- Create: `vox-core/src/storage/charts.rs`
- Modify: `vox-core/src/domain/mod.rs` (add `pub mod charts;`, same
  line shape as its existing `pub mod schemas;`)
- Modify: `vox-core/src/storage/mod.rs` (add `pub mod charts;`, same
  line shape as its existing `pub mod schemas;`)

**What `domain/charts.rs` must contain:**
- A `ChartType` type mirroring the existing `SchemaState`-style enum
  pattern in `domain/schemas.rs` (derive `Serialize`/`Deserialize`,
  snake_case on the wire), with exactly the four variants `Line`,
  `Bar`, `Pie`, `Area`, plus `as_str`/`parse` methods matching the
  pattern `domain/spans.rs`'s `SpanStatus` already uses.
- A `ChartBoard` struct: `id`, `user_id`, `name`, `created_at`,
  `updated_at` — same field shape as the migration's columns.
- A `Chart` struct: `id`, `board_id`, `title`, `chart_type`
  (`ChartType`), `schema_ids` (`Vec<Uuid>`), `query_spec`
  (`serde_json::Value`), `created_at`.
- A `QuerySpec` struct (not persisted as its own row — it's what lives
  inside `Chart.query_spec`, but give it a real Rust type for the
  suggestion/query-builder code to use instead of passing raw
  `Value` around): `metric_field: String`, `aggregation: Aggregation`
  (an enum: `Sum`, `Count`, `Avg`, `Min`, `Max`, same `as_str`/`parse`
  pattern), `group_by: GroupBy` (an enum: `Day`, `Week`, `Month`, or
  `Field(String)` for a categorical group-by — represent this as an
  untagged/adjacently-tagged serde enum so the wire shape stays simple
  JSON, matching how other flexible-shape fields in this codebase are
  modeled).

**What `storage/charts.rs` must contain**, as a `ChartRepository`
struct wrapping a `PgPool` (mirror `storage/schemas.rs`'s
`SchemaRepository` shape exactly — same constructor pattern):
- `create_board(user_id, name) -> ChartBoard`
- `list_boards(user_id) -> Vec<ChartBoard>` (ordered newest first,
  matching the new index from Task 1)
- `get_board(user_id, board_id) -> Option<ChartBoard>` (must filter by
  `user_id` — a board id alone is not enough to authorize a read)
- `add_chart(board_id, title, chart_type, schema_ids, query_spec) ->
  Chart`
- `list_charts_for_board(board_id) -> Vec<Chart>`

- [ ] **Step 1: Write `domain/charts.rs`** with the types above.

- [ ] **Step 2: Write `storage/charts.rs`** with the repository above,
  using the same `sqlx::query`/`Row` pattern already used in
  `storage/schemas.rs` (plain `query`/`query_scalar`, not the `query!`
  macro — this codebase doesn't use compile-time-checked queries).

- [ ] **Step 3: Register both modules** per the Files section above.

- [ ] **Step 4: Build**

Run: `cd vox-core && cargo build 2>&1 | tail -80`
Expected: clean build (nothing calls these new types yet, so no
downstream breakage possible at this step).

- [ ] **Step 5: Commit**

```bash
git add src/domain/charts.rs src/storage/charts.rs src/domain/mod.rs src/storage/mod.rs
git commit -m "feat(charts): add domain models and storage repository"
```

---

## Task 3: Query-spec builder — turn a QuerySpec into span aggregation SQL

**Files:**
- Create: `vox-core/src/application/chart_query.rs`
- Modify: `vox-core/src/application/mod.rs` (add
  `pub mod chart_query;`, same line shape as its existing
  `pub mod schemas;`)

**What it must contain:** a function (e.g. `compute_chart_data(pool,
user_id, schema_ids, query_spec) -> Result<Vec<ChartDataPoint>,
sqlx::Error>`) where `ChartDataPoint` is a small struct with a `label`
(the time bucket or the categorical group value, as a string) and a
`value` (a number). The function must:

- Build a single parameterized query against `spans` filtered by
  `user_id = $1` and `schema_id = ANY($2)` (binding the chart's
  `schema_ids`), with the aggregation expression selected from a
  small fixed match over `Aggregation` (sum/count/avg/min/max of
  `(data->>metric_field)::numeric`, with `metric_field` itself never
  interpolated into the SQL string — bind it as a parameter and use
  it only inside a `data->>$n` extraction, never string-concatenated
  into the query) — this is the concrete mechanism that keeps the
  "LLM proposes a spec, never SQL" boundary real rather than
  aspirational.
- For a `Day`/`Week`/`Month` group-by, bucket on `date_trunc` of
  `spans.start_at` (falling back to `created_at` when `start_at` is
  null, matching the same fallback already used elsewhere for spans
  without a start time).
- For a `Field(name)` group-by, group on `data->>name` directly (again
  bound as a parameter, never concatenated), and cap the result to
  the top 20 groups by aggregated value, folding any remainder into a
  single `"other"` label — this satisfies the Review Focus item about
  unbounded categorical group-by.
- Treat a row where `metric_field` isn't castable to numeric as
  excluded from the aggregate (a `WHERE` guard using a safe numeric
  check, or `CASE`-based null-out, not a hard failure) — satisfies the
  Review Focus item about mixed-shape data.

- [ ] **Step 1: Write `application/chart_query.rs`** with the function
  and types above.

- [ ] **Step 2: Register the module.**

- [ ] **Step 3: Build**

Run: `cd vox-core && cargo build 2>&1 | tail -80`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src/application/chart_query.rs src/application/mod.rs
git commit -m "feat(charts): add query_spec-to-SQL aggregation builder"
```

---

## Task 4: Chart suggestion agent (one-shot LLM call)

**Files:**
- Create: `vox-core/src/agents/chart_suggester.rs`
- Modify: `vox-core/src/agents/mod.rs` (add
  `pub mod chart_suggester;`, same line shape as its existing
  `pub mod schema_extractor;`)

**What it must contain**, mirroring `agents/schema_extractor.rs`'s
shape exactly (same `SchemaExtracting`-style trait pattern, same
`gemini::Client`/`structured_json` usage):

- A `ChartSuggestionPrompt` struct holding the selected schemas
  (namespace, name, description, json_schema for each) and a small
  sample of recent `data` values per schema (enough for the model to
  see realistic value shapes — a handful of rows per schema, not the
  full history).
- A `ChartSuggestion` struct: `title`, `description`, `chart_type`
  (string, validated against the four allowed values after
  deserializing), `query_spec` (matching the `QuerySpec` shape from
  Task 2).
- A `SuggestingCharts` trait with one method returning
  `Vec<ChartSuggestion>`.
- A `GeminiChartSuggester` implementing it: one Gemini call, one
  prompt instructing the model to propose several chart ideas as a
  JSON array, each shaped like `ChartSuggestion`, using only fields
  that actually appear in the given `json_schema`, using only the
  four allowed `chart_type` values, and using only the five allowed
  `aggregation` values. After deserializing, validate every
  suggestion's `metric_field` actually exists in the schema whose
  `schema_ids` it references and drop (don't fail the whole batch)
  any suggestion that references a field that isn't there — this is
  the concrete defense that keeps a hallucinated field name from
  reaching Task 3's query builder.

- [ ] **Step 1: Write `agents/chart_suggester.rs`** with the above.

- [ ] **Step 2: Register the module.**

- [ ] **Step 3: Build**

Run: `cd vox-core && cargo build 2>&1 | tail -80`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src/agents/chart_suggester.rs src/agents/mod.rs
git commit -m "feat(agents): add one-shot chart-suggestion agent"
```

---

## Task 5: HTTP routes — schemas listing, suggestion, board CRUD, board data

**Files:**
- Create: `vox-core/services/api/routes/charts.rs`
- Modify: `vox-core/services/api/routes/schemas.rs` (add the listing
  handler)
- Modify: `vox-core/services/api/routes/mod.rs` (register `charts`)
- Modify: `vox-core/services/api/router.rs` (wire the new routes and
  state, same `Router::new().route(...).with_state(...)` merge
  pattern already used for `voice_routes`)
- Modify: `vox-core/services/api/state.rs` (add whatever new shared
  state the chart routes need — the `ChartRepository`, the
  `SuggestingCharts` implementation, and read access to schemas/spans
  repositories they already have access to via existing state)
- Modify: `vox-core/services/api/main.rs` (construct the chart
  suggester the same way `main.rs` already constructs the ElevenLabs
  client — reading its config, wrapping in an `Arc`, passing into
  `ApiState::new`)
- Modify: `vox-core/src/config.rs` (no new config expected — the
  chart suggester reuses the existing `gemini_api_key`/`gemini_model`
  fields already used by `schema_extractor.rs`; only touch this file
  if that assumption turns out wrong)

**Endpoints to add**, each requiring the same authenticated-`Actor`
extension every other `/v1/me/...`-style route already uses:

- `GET /v1/me/schemas` — list the caller's own schemas (add this
  handler to `routes/schemas.rs` alongside the existing two; reuse
  `SchemaRepository`/`SchemaService` machinery, add whatever list
  method is missing there rather than duplicating a query).
- `POST /v1/me/charts/suggest` — body: an array of schema UUIDs.
  Loads those schemas (filtered to the caller's own, silently
  dropping any id that doesn't resolve — per Review Focus), loads a
  data sample per schema, calls the Task 4 suggester, returns the
  suggestions. Reject an empty schema-id array with 400. Cap the
  array length at a fixed maximum (e.g. 8) and reject anything larger
  with 400, per Review Focus.
- `POST /v1/me/charts/boards` — body: a name and an array of accepted
  suggestions (same shape as what `/suggest` returned). Creates the
  board, then one chart row per accepted suggestion, returns the
  created board with its charts.
- `GET /v1/me/charts/boards` — list the caller's boards.
- `GET /v1/me/charts/boards/:id` — one board's metadata and its
  charts' metadata (no data points).
- `GET /v1/me/charts/boards/:id/data` — for every chart on the board,
  run Task 3's `compute_chart_data`, return a map/array of `{chart_id,
  data_points}`. A chart whose data computation fails (e.g. its
  schema was deleted) must appear in the response with an error
  marker for that one chart, not abort the whole response — per
  Review Focus.

- [ ] **Step 1: Add the schema-listing handler** to `routes/schemas.rs`
  and whatever backing method it needs on `SchemaService`/
  `SchemaRepository`.

- [ ] **Step 2: Write `routes/charts.rs`** with the four chart
  endpoints above, following the existing handler style (see
  `routes/voice.rs` or `routes/schemas.rs` for the
  `State`/`Extension<Actor>`/`Json` extractor pattern this codebase
  uses throughout).

- [ ] **Step 3: Wire routing, state, and app construction** in
  `router.rs`/`state.rs`/`main.rs`.

- [ ] **Step 4: Build**

Run: `cd vox-core && cargo build --all-targets 2>&1 | tail -100`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add services/api/routes/charts.rs services/api/routes/schemas.rs services/api/routes/mod.rs services/api/router.rs services/api/state.rs services/api/main.rs
git commit -m "feat(api): add Pulse endpoints (schemas list, suggest, board CRUD, board data)"
```

---

## Task 6: vox-desktop — Tauri commands for the five new endpoints

**Files:**
- Modify: `vox-desktop/src-tauri/src/sync_client.rs`
- Modify: `vox-desktop/src-tauri/src/lib.rs` (register the new
  `#[tauri::command]`s in the `invoke_handler` list, same as every
  existing command)

**What to add:** one `#[tauri::command]` per new vox-core endpoint —
`list_schemas`, `suggest_charts`, `create_chart_board`,
`list_chart_boards`, `get_chart_board`, `get_chart_board_data` — each
a thin wrapper around the existing `core_request` helper, exactly the
shape `get_spans`/`create_span`/`get_collections` already use (method,
path, optional JSON body, return `Result<Value, String>`). No new
plumbing pattern — this is mechanical repetition of what's already in
the file six more times.

- [ ] **Step 1: Add the six commands** to `sync_client.rs`.

- [ ] **Step 2: Register them** in `lib.rs`'s `invoke_handler!` list.

- [ ] **Step 3: Build**

Run: `cd vox-desktop/src-tauri && cargo build 2>&1 | tail -80`
Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/sync_client.rs src-tauri/src/lib.rs
git commit -m "feat(pulse): add Tauri commands for the five new vox-core endpoints"
```

---

## Task 7: vox-desktop — add recharts, TypeScript API surface

**Files:**
- Modify: `vox-desktop/package.json` (add `recharts`)
- Modify: `vox-desktop/src/lib/tauri.ts` (add the TypeScript types and
  `api.*` wrapper functions for the six new commands, matching the
  existing `getSpans`/`createSpan` style exactly — one `invoke<T>(...)`
  call per function, plus the TypeScript interfaces for `Schema`,
  `ChartBoard`, `Chart`, `ChartSuggestion`, `QuerySpec`, `ChartDataPoint`
  mirroring the Rust types from Tasks 2 and 4 field-for-field)

- [ ] **Step 1: Add the recharts dependency.**

Run: `npm install recharts` (this project uses npm — `package-lock.json`
is the tracked lockfile).

- [ ] **Step 2: Add the TypeScript types and `api.*` functions** to
  `tauri.ts`.

- [ ] **Step 3: Type-check**

Run: `npm run build` (this project's `build` script is `tsc -b && vite
build`, so this is also the type-check).
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add package.json package-lock.json src/lib/tauri.ts
git commit -m "feat(pulse): add recharts dependency and TypeScript API surface"
```

---

## Task 8: vox-desktop — new "Pulse" tab registration

**Files:**
- Modify: `vox-desktop/src/components/shell/shell-tabs.ts` (add
  `"pulse"` to the `ShellTab` union and `SHELL_TABS` array, using the
  `Activity` icon from lucide-react)
- Modify: `vox-desktop/src/components/shell/app-sidebar.tsx`'s
  `NAV_ITEMS` array the same way (this file currently duplicates
  `SHELL_TABS` rather than importing it — follow the existing
  duplication rather than refactoring it away as part of this feature;
  that's an unrelated cleanup)
- Modify: `vox-desktop/src/components/home-shell.tsx` (add the
  `activeTab === "pulse"` branch rendering the new top-level Pulse
  component from Task 9, alongside the existing `"agent"`/`"timeline"`
  branches)

- [ ] **Step 1: Register the tab** in both files above.

- [ ] **Step 2: Add the render branch** in `home-shell.tsx` (it can
  render a placeholder component until Task 9 lands — but land Task 9
  in the same work session so the tab is never shipped empty).

- [ ] **Step 3: Type-check** — run `npm run build`.

- [ ] **Step 4: Commit**

```bash
git add src/components/shell/shell-tabs.ts src/components/shell/app-sidebar.tsx src/components/home-shell.tsx
git commit -m "feat(pulse): register the Pulse tab in the sidebar and shell"
```

---

## Task 9: vox-desktop — Pulse landing view and board list

**Files:**
- Create: `vox-desktop/src/components/pulse/pulse-view.tsx`
  (top-level component rendered by Task 8's branch)
- Create: `vox-desktop/src/hooks/use-chart-boards.ts` (data-fetching
  hook mirroring `hooks/use-spans.ts`'s shape: loads via
  `api.listChartBoards()`, exposes `boards`/`loading`/`error`/a
  `reload` function; no polling interval is needed here since boards
  don't change from outside this UI)

**Behavior:** on mount, load boards via the new hook. Render each as a
card (name, chart count) that navigates to Task 11's board view.
Render a "+" button that opens Task 10's picker flow. Empty state
(“no boards yet”) when the list is empty.

- [ ] **Step 1: Write `use-chart-boards.ts`.**

- [ ] **Step 2: Write `pulse-view.tsx`** with the landing view above,
  using this project's existing card/button components from
  `@/components/ui/*` (the same shadcn primitives already used
  throughout the app) rather than introducing new one-off styling.

- [ ] **Step 3: Type-check.**

- [ ] **Step 4: Commit**

```bash
git add src/components/pulse/pulse-view.tsx src/hooks/use-chart-boards.ts
git commit -m "feat(pulse): add landing view and board list"
```

---

## Task 10: vox-desktop — category picker + suggestion picker flow

**Files:**
- Create: `vox-desktop/src/components/pulse/create-board-flow.tsx`
- Modify: `vox-desktop/src/components/pulse/pulse-view.tsx` (wire the
  "+" button to open this flow, and on successful board creation,
  reload the board list and navigate to the new board)

**Behavior, two steps in one component (local state for which step is
active, no new routing needed since this is a modal/overlay within the
Pulse tab, not a URL-addressable route in this app):**

- **Step 1 — category picker:** loads schemas via
  `api.listSchemas()`, renders each as a selectable chip/card showing
  its name, description, and its icon_token/color_token resolved to
  an actual icon+color (this needs a small icon_token→lucide-icon and
  color_token→CSS-color lookup table — a fixed array of 24 icons and
  24 colors indexed by the token integer, since the backend
  intentionally stores only the index per the schema design; build
  this lookup table once here and reuse it anywhere else in the app
  that ever needs to render a schema's icon/color). Multi-select.
  "Next" disabled until at least one is selected.
- **Step 2 — suggestion picker:** on entering this step, call
  `api.suggestCharts(selectedSchemaIds)`, show a loading state, then
  render the returned suggestions as a checklist (title, description,
  a small icon for the chart_type). Multi-select, all checked by
  default. A name field for the board. "Create" calls
  `api.createChartBoard(name, selectedSuggestions)`, then closes the
  flow and hands control back to `pulse-view.tsx` to reload and
  navigate.

- [ ] **Step 1: Write the icon_token/color_token lookup table** as a
  small shared module (e.g. `src/lib/schema-tokens.ts`) rather than
  inlining it in this component, since Task 9's board cards and Task
  11's board view will also want to show a chart's source category's
  icon/color.

- [ ] **Step 2: Write `create-board-flow.tsx`** with the two steps
  above.

- [ ] **Step 3: Wire it into `pulse-view.tsx`.**

- [ ] **Step 4: Type-check.**

- [ ] **Step 5: Commit**

```bash
git add src/components/pulse/create-board-flow.tsx src/components/pulse/pulse-view.tsx src/lib/schema-tokens.ts
git commit -m "feat(pulse): add category picker and chart-suggestion picker flow"
```

---

## Task 11: vox-desktop — board view with recharts rendering

**Files:**
- Create: `vox-desktop/src/components/pulse/board-view.tsx`
- Create: `vox-desktop/src/hooks/use-chart-board-data.ts` (loads a
  single board's metadata via `api.getChartBoard(id)` and its data via
  `api.getChartBoardData(id)`, exposes a `reload` function for the
  manual-refresh button)
- Modify: `vox-desktop/src/components/pulse/pulse-view.tsx` (render
  `board-view.tsx` when a board card is selected, with a back button
  to return to the landing view — plain local state for "which board
  is open," no new routing)

**Behavior:** for each chart on the board, render a recharts
`LineChart`/`BarChart`/`PieChart`/`AreaChart` matching its
`chart_type`, fed by that chart's data points from the `/data`
response. A chart whose data response carries an error marker (per
Task 5's Review Focus handling) renders a small inline error state
instead of a broken chart. A manual refresh button re-calls the data
hook's `reload`.

- [ ] **Step 1: Write `use-chart-board-data.ts`.**

- [ ] **Step 2: Write `board-view.tsx`** with the chart-type-to-recharts-
  component mapping and per-chart rendering above.

- [ ] **Step 3: Wire navigation** in `pulse-view.tsx`.

- [ ] **Step 4: Type-check.**

- [ ] **Step 5: Commit**

```bash
git add src/components/pulse/board-view.tsx src/hooks/use-chart-board-data.ts src/components/pulse/pulse-view.tsx
git commit -m "feat(pulse): add board view with recharts rendering"
```

---

## Final verification

- [ ] `cd vox-core && cargo build --all-targets` — clean.
- [ ] `cd vox-desktop/src-tauri && cargo build` — clean.
- [ ] vox-desktop's type-check/lint command — clean.
- [ ] Manually exercise the whole flow against a dev environment:
      open Pulse, create a board from 1-2 real categories, confirm the
      suggestion list looks sane and every suggestion's `query_spec`
      only references fields that actually exist on the chosen
      schema(s), select a few, create the board, confirm charts
      render, confirm the manual refresh re-fetches data, confirm a
      second board can be created and both persist across an app
      restart.
