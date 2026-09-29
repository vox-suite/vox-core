# Pulse: LLM-suggested chart boards from a user's own categories

Date: 2026-09-29
Repos touched: vox-core (data model, LLM suggestion, chart data queries),
vox-desktop (new sidebar tab, picker flow, chart rendering).

## Problem

The unified-span-ingestion work earlier this session gave every inbound
fact a `schema_id` pointing at a `data_schemas` category (namespace/name/
description/json_schema, now also color_token/icon_token). There is
still no way for a user to look across their own categories and see
patterns — no charting, no aggregation, nothing beyond the raw timeline.
This was explicitly deferred as "analytics/widgets — separate
sub-project" in the original span-ingestion spec. This document is that
sub-project: a new "Pulse" tab where a user picks one or more
categories, an LLM suggests chart ideas drawn from their own schema
shapes and recent data, the user picks the ones they want, and those
become a saved, revisitable board.

## Design

### 1. Data model

Two new tables in vox-core:

- `chart_boards`: `id`, `user_id`, `name`, `created_at`, `updated_at`.
  One row per saved board (e.g. "Finances", "Sleep").
- `charts`: `id`, `board_id` (FK, cascade delete), `title`,
  `chart_type` (`line` | `bar` | `pie` | `area`), `schema_ids` (uuid
  array — which categories this chart draws from), `query_spec`
  (JSONB), `created_at`. One row per chart on a board.

`query_spec` is a small, fixed-shape JSON object, not a free-form
query: `metric_field` (which `data` property to aggregate),
`aggregation` (`sum` | `count` | `avg` | `min` | `max`), and
`group_by` (a time bucket — `day` | `week` | `month` — or a categorical
field name to group by instead). This is deliberately constrained:
**the LLM proposes a `query_spec`, never a SQL string.** A Rust query
builder is the only thing that turns a `query_spec` into an actual
parameterized query against `spans.data`. An LLM-authored SQL fragment
run against user data is not a boundary worth crossing for this
feature.

### 2. Chart suggestion (System 2's sibling, not System 2 itself)

New flow, three new vox-core endpoints:

- `GET /v1/me/schemas` — lists the user's own `data_schemas` rows
  (namespace, name, description, color_token, icon_token). Doesn't
  exist today (`services/api/routes/schemas.rs` only has
  `get_schema_by_name`, which needs an exact name) — needed to power
  the category picker.
- `POST /v1/me/charts/suggest`, body `{ schema_ids: [uuid, ...] }` —
  loads those schemas (`json_schema`, `description`) plus a small
  sample of each schema's recent `spans` rows (for realistic value
  shapes, not the whole history), and makes one Gemini call asking for
  a list of chart ideas: title, chart_type, one-line description, and
  a `query_spec`. This is a one-shot structured-output call, the same
  pattern as `agents/schema_extractor.rs` — a new sibling module, not
  a change to System 2's novel-schema path, since this isn't ingestion.
  Nothing is persisted by this call; the response is a plain list the
  client shows as checkboxes.
- `POST /v1/me/charts/boards`, body `{ name, charts: [...selected
  suggestions...] }` — persists a board and its charts from what the
  user checked.

### 3. Reading a board

- `GET /v1/me/charts/boards` — list boards (id, name, chart count) for
  the Pulse tab's landing view.
- `GET /v1/me/charts/boards/:id` — one board's charts (metadata only:
  title, chart_type, schema_ids, query_spec — not data points).
- `GET /v1/me/charts/boards/:id/data` — runs every chart's `query_spec`
  live against `spans` and returns the resulting data points per
  chart. This is also what a manual refresh calls. No precomputed
  rollup tables in this pass — the same reasoning as the ingestion
  spec's performance section: at "hundreds of users" scale with
  per-category span counts still modest, a live aggregate query is
  fine, and rollups are real, deferrable infrastructure to build only
  once there's an actual scale reason to.

### 4. Desktop: Tauri command layer

vox-desktop's frontend never calls vox-core's HTTP API directly — it
goes through Tauri IPC (`invoke("get_spans", ...)` → a `#[tauri::command]`
in `src-tauri/src/sync_client.rs` → `core_request`, a small shared
helper that adds the bearer token and hits vox-core). The five new
endpoints above each get a matching thin command in the same file,
following the exact shape `get_spans`/`create_span` already use — no
new pattern, just more of the same one.

### 5. Desktop: UI flow

New tab, id `"pulse"`, added to `ShellTab` alongside `"agent"`/
`"timeline"` in `shell-tabs.ts` and the sidebar's nav list — same
mechanism as the existing tabs, nothing new to invent there. Icon:
`Activity` from lucide-react (reads as a heartbeat line, matches the
"Pulse" name).

- **Landing view**: saved boards as cards (name + chart count), plus a
  "+" button.
- **"+" flow**: step 1, multi-select categories from `GET
  /v1/me/schemas` (rendered with their icon_token/color_token so
  categories are visually distinct); "Next". Step 2, call `POST
  /v1/me/charts/suggest`, show a loading state, then the returned
  suggestions as a checklist (title + description + a small chart-type
  icon); user multi-selects, names the board, submits — calls `POST
  /v1/me/charts/boards`, then navigates to the new board.
- **Board view**: fetches `GET /v1/me/charts/boards/:id/data` and
  renders each chart via **recharts** (line/bar/pie/area matching
  `chart_type`) — a new dependency; vox-desktop has no charting library
  today. Recharts is shadcn/ui's own charting pairing, and vox-desktop
  already uses shadcn/ui throughout, so this matches the existing
  design system rather than introducing a second one.

## Explicitly out of scope

- Precomputed rollup tables for chart data — live query only, per
  section 3's reasoning. Revisit if a specific board's query becomes
  slow at real usage volume.
- Editing a chart's `query_spec` by hand after creation, or re-running
  the suggestion flow to add charts to an existing board — v1 is
  create-a-board-once; both are natural, additive follow-ups.
- Auto-refresh / polling on the board view. Data is fetched when the
  board opens; a manual refresh (re-calling the `/data` endpoint) is
  in scope, background polling is not.
- Any change to System 2 (`schema_extractor.rs`) or the ingestion
  pipeline. This feature only reads `spans`/`data_schemas`; it doesn't
  touch how they're written.
- Mobile (vox-android). Desktop only for this pass.

## Testing

None added, per this session's standing instruction. Any existing test
that breaks because of these additions is to be deleted, not fixed to
pass around the change.

## Comments

No code comments added as part of this work.
