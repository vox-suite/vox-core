# Projects shell — design spec

Status: approved (in-chat design), pending written-spec review
Repos touched: vox-core, vox-desktop

## Purpose

Vox desktop currently has task management (`tasks-view.tsx`) but no way to
group tasks with other related material. The user wants a **Project**
concept: a container that holds a set of tasks plus related items —
notes, datasets, analytics, reports.

This is the first of six sub-projects in a larger initiative. This spec
covers only the first: the Projects shell (list, create, archive, detail
page navigation) plus filtering tasks by project. The remaining five
(notes, datasets, analytics, reports, and — separately — phone-call
control) are out of scope here and will each get their own spec.

## Background: what already exists

vox-core's `collections` table (`schema/target_core.sql`) already *is*
the project concept — it just isn't exposed as one in the UI yet:

- `collections`: `id, user_id, name, description, kind ('project' |
  'trip' | 'course' | 'area'), status, metadata JSONB`.
- `tasks.collection_id` — FK to `collections`, `ON DELETE SET NULL`.
  Already settable at task creation (`application/tasks.rs`,
  `CreateTaskInput.collection_id`) and returned on every task read.
- `records.collection_id` — same FK, already on the table, used by the
  later "datasets" sub-project (not this one).
- Full CRUD already live and `Actor`-authenticated, same pattern as
  tasks: `services/api/routes/collections.rs` →
  `GET/POST /v1/collections`, `GET/DELETE /v1/collections/{id}`.

The one gap: `TaskRepository::list` (`src/storage/tasks.rs`) takes only
`(user_id, limit)` — no way to filter to one project's tasks. That's the
only backend change this spec requires.

## Data model

No new tables, no migrations. This sub-project only adds a query
parameter to an existing read path.

## Backend changes (vox-core)

1. `src/storage/tasks.rs`: `TaskRepository::list` gains an optional
   `collection_id: Option<Uuid>` parameter; when present, adds
   `AND collection_id = $N` to the query.
2. `src/application/tasks.rs`: `TaskService::list_tasks` threads the new
   parameter through.
3. `services/api/routes/tasks.rs`: `ListTasksQuery` gains an optional
   `collection_id: Option<Uuid>` field, passed through to the service.

No changes to `collections.rs` anywhere — its existing CRUD is reused
as-is.

## Frontend changes (vox-desktop)

### Tauri commands (`src-tauri/src/lib.rs`, `src/lib/tauri.ts`)

Mirror the existing task commands exactly:

- `get_collections()` → `GET /v1/collections`
- `create_collection(payload)` → `POST /v1/collections`
- `archive_collection(id)` → `DELETE /v1/collections/{id}`
- `get_tasks` gains an optional `collection_id` argument, forwarded as a
  query parameter.

### Navigation (`app-sidebar.tsx`, `app.tsx`)

- `DesktopView` gains `"projects"`, with its own sidebar icon (e.g.
  lucide `FolderKanban`), placed next to Home and Tasks — same pattern
  as the existing two entries. Tasks stays exactly as it is today
  (unfiltered, all projects).
- `app.tsx` tracks `selectedProjectId: string | null`. When
  `view === "projects"`: render `<ProjectsView>` if nothing is selected,
  else `<ProjectDetailView projectId={selectedProjectId}>`.

### `src/components/projects-view.tsx` (new)

- Lists the user's collections (name, status badge, task count).
- "New Project" opens a small dialog (name, description, kind — default
  `project`), following the same dialog pattern as
  `new-task-dialog.tsx`.
- Clicking a row selects it (sets `selectedProjectId` in `app.tsx`).
- An archive action per row (calls `archive_collection`).

### `src/components/project-detail-view.tsx` (new)

- Header: project name, status, a back button (clears
  `selectedProjectId`).
- Tab bar using the existing `ui/tabs.tsx` primitive: **Tasks / Notes /
  Datasets / Analytics / Reports**.
- **Tasks tab**: the row-rendering logic in `tasks-view.tsx` (the table/
  list markup for a set of tasks) is extracted into a shared component
  so both views render tasks identically without duplicating markup;
  `project-detail-view.tsx` calls it with
  `get_tasks({ collection_id: projectId })`.
- **Notes / Datasets / Analytics / Reports tabs**: render a "coming in a
  later update" placeholder state (same visual pattern already used by
  `HomeRail`'s `placeholder` text for LMS/Data/Analytics today). This is
  a sequencing boundary, not a scope cut — each becomes real in its own
  upcoming sub-project, in the order already agreed with the user:
  Notes → Datasets → Analytics → Reports.

## Testing

Skipped at the user's request. No new tests are added for this
sub-project (Rust or frontend).

## Explicitly out of scope (future specs)

- Notes, Datasets, Analytics, Reports tab content.
- Threading `collection_id` through the agent's `create_user_record` /
  `list_user_records` tools (needed for Datasets; agent-settable project
  links for records don't exist yet — confirmed by reading
  `src/agents/tools/records.rs`).
- Phone-call control of the desktop app.
