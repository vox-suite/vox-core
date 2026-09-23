# Projects Shell Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Projects section to vox-desktop — list/create/archive projects (backed by vox-core's existing `collections` table), a detail page with a working, project-filtered Tasks tab, and four placeholder tabs (Notes/Datasets/Analytics/Reports) reserved for later sub-projects.

**Architecture:** vox-core already has full CRUD for `collections` and a `collection_id` FK on `tasks` — this plan adds one small filter to the existing task-list query, then builds the vox-desktop UI on top of it. vox-desktop's task list currently reads from three sources in sequence (direct Supabase PostgREST, vox-core's REST API, a local on-disk JSON cache) — the `collection_id` filter and a task's project assignment both need to work across all three, or filtering silently breaks whenever the Supabase path (the one that wins in normal operation) is used.

**Tech Stack:** Rust (axum, sqlx) for vox-core; Rust (tauri, reqwest) + React/TypeScript (Vite, radix-ui) for vox-desktop.

**Spec:** vox-core/docs/superpowers/specs/2026-09-24-projects-shell-design.md

## Global Constraints

- Tests are skipped for this sub-project per explicit user instruction — no new Rust or frontend tests are added anywhere in this plan.
- Every task still ends with a compile/build check (`cargo check` or `npm run build`) — that is a basic correctness gate, not a test suite, and stays in scope.
- Follow existing patterns exactly: new Tauri commands mirror `get_tasks`/`create_task`'s structure; new dialogs mirror `new-task-dialog.tsx`; new REST consumption mirrors how `get_tasks`'s vox-core-API branch already calls `services/api`.
- `AppSidebar` (`vox-desktop/src/components/app-sidebar.tsx`) is dead code — it is never rendered anywhere in the app. The real navigation surface is `HomeRail` inside `dashboard-view.tsx`. Do not add UI to `AppSidebar`; only its exported `DesktopView` type is used (by `app.tsx`'s `useState<DesktopView>`), so that type still needs the new `"projects"` value.
- The existing "Project / Collection" field in `NewTaskDialog` is a free-text string today (`newTask.project`, default `"Vox Core"`) and is **not** persisted anywhere real — neither the Supabase insert body nor the vox-core POST body in `create_task` (`src-tauri/src/lib.rs`) includes it. This plan replaces it with a real `collection_id` selection backed by actual collections.

## Review Focus

- **Task list without a `collection_id`:** existing callers of `get_tasks`/`GET /v1/tasks`/`TaskRepository::list` must keep returning all of a user's tasks unfiltered when no collection is specified — the new parameter must default to "no filter," not "filter to null."
- **A task's `collection_id` is `null` in a filtered project's task list:** a task with no project must never show up when filtering by a specific project's id (three separate query paths need the same filter semantics: Supabase PostgREST, vox-core REST, and the local JSON cache).
- **Archiving a project that still has tasks assigned to it:** `collections` has no `ON DELETE` behavior beyond what the DB already defines (`tasks.collection_id` is `ON DELETE SET NULL`, but `archive_collection` only sets `status = 'archived'`, it doesn't delete the row) — confirm archiving a project does not delete or orphan its tasks, only hides the project from the active list.
- **Empty/whitespace project name on create:** `collections.name` has a `CHECK (length(btrim(name)) > 0)` constraint at the DB level — the "New Project" dialog must not let a request through with a blank name, or the user sees a raw 500 instead of a clear validation message.
- **Selecting "No project" in the New Task dialog:** after adding a real collection picker, there must still be a way to create a task with no project (the common case for solo/ungrouped tasks) — an empty option in the picker must translate to `collection_id: None`/`null`, not to a stray empty-string id sent to the backend.

---

## Task 1: vox-core — filter task list by `collection_id`

**Files:**
- Modify: `vox-core/src/storage/tasks.rs:133` (`TaskRepository::list`)
- Modify: `vox-core/src/application/tasks.rs:115` (`TaskService::list_tasks`)
- Modify: `vox-core/services/api/routes/tasks.rs:17-32` (`ListTasksQuery`, `list_tasks` handler)

**Interfaces:**
- Consumes: nothing new.
- Produces: `TaskRepository::list(&self, user_id: Uuid, collection_id: Option<Uuid>, limit: i64) -> Result<Vec<Task>, sqlx::Error>` and `TaskService::list_tasks(&self, actor: &Actor, collection_id: Option<Uuid>, limit: i64) -> Result<Vec<Task>, sqlx::Error>` — both existing callers (the REST route in this same task) are updated together, so there are no other call sites to break.

- [ ] **Step 1: Add the filter to `TaskRepository::list`**

In `vox-core/src/storage/tasks.rs`, replace the existing `list` method:

```rust
    pub async fn list(
        &self,
        user_id: Uuid,
        collection_id: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<Task>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, user_id, collection_id, title, instruction, status, priority,
                   execution_type, feasibility_reasoning, execution_result, due_at,
                   version, cancellation_requested_at, created_at, updated_at, completed_at
            FROM tasks
            WHERE user_id = $1
              AND ($3::UUID IS NULL OR collection_id = $3)
            ORDER BY created_at DESC
            LIMIT $2
            "#,
        )
        .bind(user_id)
        .bind(limit)
        .bind(collection_id)
        .fetch_all(&self.pool)
        .await?;
```

Leave the rest of the method (the `rows.into_iter().map(...)` block that builds `Task` values) exactly as it is.

- [ ] **Step 2: Thread the parameter through `TaskService::list_tasks`**

In `vox-core/src/application/tasks.rs`, replace:

```rust
    pub async fn list_tasks(&self, actor: &Actor, limit: i64) -> Result<Vec<Task>, sqlx::Error> {
        self.repo.list(actor.user_id, limit).await
    }
```

with:

```rust
    pub async fn list_tasks(
        &self,
        actor: &Actor,
        collection_id: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<Task>, sqlx::Error> {
        self.repo.list(actor.user_id, collection_id, limit).await
    }
```

(`Uuid` is already imported in this file — it's used elsewhere in the same `impl` block.)

- [ ] **Step 3: Add the query parameter to the REST route**

In `vox-core/services/api/routes/tasks.rs`, replace:

```rust
#[derive(Deserialize)]
pub struct ListTasksQuery {
    pub limit: Option<i64>,
}

pub async fn list_tasks(
    State(service): State<TaskService>,
    Extension(actor): Extension<Actor>,
    Query(query): Query<ListTasksQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let tasks = service
        .list_tasks(&actor, limit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(tasks))
}
```

with:

```rust
#[derive(Deserialize)]
pub struct ListTasksQuery {
    pub limit: Option<i64>,
    pub collection_id: Option<Uuid>,
}

pub async fn list_tasks(
    State(service): State<TaskService>,
    Extension(actor): Extension<Actor>,
    Query(query): Query<ListTasksQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let tasks = service
        .list_tasks(&actor, query.collection_id, limit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(tasks))
}
```

(`Uuid` is already imported at the top of this file for the `Path(id): Path<Uuid>` extractors on the other handlers.)

- [ ] **Step 4: Build check**

Run: `cd vox-core && cargo check`
Expected: compiles with no errors.

- [ ] **Step 5: Commit**

```bash
cd vox-core
git add src/storage/tasks.rs src/application/tasks.rs services/api/routes/tasks.rs
git commit -m "feat(tasks): filter task list by collection_id"
```

---

## Task 2: vox-desktop — thread `collection_id` through the task read/write paths

**Files:**
- Modify: `vox-desktop/src-tauri/src/lib.rs` (`DesktopTask`, `CreateTaskPayload`, `GetTasksArgs`, `get_tasks`, `create_task`, `TaskManager::filter_and_paginate`)

**Interfaces:**
- Consumes: nothing new from other tasks.
- Produces: `DesktopTask.collection_id: Option<String>`, `CreateTaskPayload.collection_id: Option<String>`, `GetTasksArgs.collection_id: Option<String>` — Task 4 (tauri.ts) mirrors these exact field names and types on the TypeScript side.

- [ ] **Step 1: Add `collection_id` to `DesktopTask`, `CreateTaskPayload`, and `GetTasksArgs`**

In `vox-desktop/src-tauri/src/lib.rs`, in the `DesktopTask` struct (around line 43), add a new field alongside the existing `project_name` field — do not touch `project_name`'s existing alias behavior:

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DesktopTask {
    pub id: String,
    pub title: String,
    #[serde(default, alias = "raw_instruction")]
    pub instruction: String,
    pub status: String,
    #[serde(default = "default_execution_type")]
    pub execution_type: String,
    #[serde(default, alias = "project_id", alias = "collection_id")]
    pub project_name: Option<String>,
    #[serde(default)]
    pub collection_id: Option<String>,
    #[serde(default)]
    pub feasibility_reasoning: Option<String>,
    #[serde(default)]
    pub execution_result: Option<serde_json::Value>,
    #[serde(default)]
    pub due_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}
```

In `CreateTaskPayload` (around line 66), add the same field:

```rust
#[derive(Clone, Debug, Deserialize)]
pub struct CreateTaskPayload {
    pub title: String,
    pub instruction: Option<String>,
    pub execution_type: Option<String>,
    pub project_name: Option<String>,
    pub collection_id: Option<String>,
    pub due_at: Option<String>,
}
```

In `GetTasksArgs` (around line 83), add the same field:

```rust
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct GetTasksArgs {
    #[serde(default)]
    pub page: Option<usize>,
    #[serde(default)]
    pub page_size: Option<usize>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub collection_id: Option<String>,
}
```

- [ ] **Step 2: Filter the local JSON cache by `collection_id`**

In `TaskManager::filter_and_paginate` (around line 243, inside the `.filter(|t| { ... })` closure), add a `matches_collection` check next to the existing `matches_status`/`matches_search`:

```rust
                let matches_collection = match args.collection_id.as_deref() {
                    None => true,
                    Some(cid) => t.collection_id.as_deref() == Some(cid),
                };
```

and change the closure's final line from:

```rust
                matches_status && matches_search
```

to:

```rust
                matches_status && matches_search && matches_collection
```

- [ ] **Step 3: Filter the Supabase PostgREST query in `get_tasks`**

In `get_tasks` (around line 412), after the existing `if let Some(status) = &args.status { ... }` block and before the `if let Some(search) = &args.search { ... }` block (or after both — order doesn't matter), add:

```rust
        if let Some(cid) = &args.collection_id {
            if !cid.is_empty() {
                sb_url.push_str(&format!("&collection_id=eq.{}", cid));
            }
        }
```

- [ ] **Step 4: Filter the vox-core API query in `get_tasks`**

In the same function, the vox-core fallback request builds `core_url` as:

```rust
        let core_url = format!(
            "{}/v1/tasks?limit={}&offset={}",
            config.api_url.trim_end_matches('/'),
            limit,
            offset
        );
```

Change it to also append `collection_id` when present:

```rust
        let mut core_url = format!(
            "{}/v1/tasks?limit={}&offset={}",
            config.api_url.trim_end_matches('/'),
            limit,
            offset
        );
        if let Some(cid) = &args.collection_id {
            if !cid.is_empty() {
                core_url.push_str(&format!("&collection_id={}", cid));
            }
        }
```

(Change `let core_url` to `let mut core_url` — everything below that already uses `&core_url` continues to work unchanged.)

- [ ] **Step 5: Send `collection_id` when creating a task**

In `create_task` (around line 495), the constructed `DesktopTask` currently doesn't set `collection_id` — add it:

```rust
    let new_task = DesktopTask {
        id: uuid::Uuid::new_v4().to_string(),
        title: payload.title.clone(),
        instruction: payload
            .instruction
            .clone()
            .unwrap_or_else(|| payload.title.clone()),
        status: "pending".to_string(),
        execution_type: payload
            .execution_type
            .clone()
            .unwrap_or_else(|| "autonomous".to_string()),
        project_name: payload.project_name.clone(),
        collection_id: payload.collection_id.clone(),
        feasibility_reasoning: None,
        execution_result: None,
        due_at: payload.due_at.clone(),
        created_at: Some(now.clone()),
        completed_at: None,
    };
```

Then, in the same function, the Supabase insert body:

```rust
        let mut sb_body = serde_json::json!({
            "title": new_task.title,
            "raw_instruction": new_task.instruction,
            "status": new_task.status,
            "execution_type": new_task.execution_type,
            "user_id": session.user_id,
        });
        if let Some(due) = &new_task.due_at {
            sb_body["due_at"] = serde_json::json!(due);
        }
```

gets one more conditional field:

```rust
        if let Some(cid) = &new_task.collection_id {
            if !cid.is_empty() {
                sb_body["collection_id"] = serde_json::json!(cid);
            }
        }
```

(add this right after the existing `due_at` block, before the `client.post(&sb_url)` call).

And the vox-core POST body:

```rust
        let body = serde_json::json!({
            "title": new_task.title,
            "instruction": new_task.instruction,
            "priority": 0,
            "due_at": new_task.due_at,
        });
```

becomes:

```rust
        let mut body = serde_json::json!({
            "title": new_task.title,
            "instruction": new_task.instruction,
            "priority": 0,
            "due_at": new_task.due_at,
        });
        if let Some(cid) = &new_task.collection_id {
            if !cid.is_empty() {
                if let Ok(parsed) = uuid::Uuid::parse_str(cid) {
                    body["collection_id"] = serde_json::json!(parsed);
                }
            }
        }
```

(vox-core's `CreateTaskInput.collection_id` is a `Uuid`, so this must send a real UUID, not an arbitrary string — parsing and skipping silently on failure matches how the rest of this function already treats the vox-core POST as best-effort.)

- [ ] **Step 6: Build check**

Run: `cd vox-desktop/src-tauri && cargo check`
Expected: compiles with no errors.

- [ ] **Step 7: Commit**

```bash
cd vox-desktop
git add src-tauri/src/lib.rs
git commit -m "feat(tasks): thread collection_id through read and write paths"
```

---

## Task 3: vox-desktop — Tauri commands for collections

**Files:**
- Modify: `vox-desktop/src-tauri/src/lib.rs` (new `Collection` struct, `get_collections`, `create_collection`, `archive_collection` commands, command registration)

**Interfaces:**
- Consumes: `AuthManager` (existing `State`), same pattern as `get_tasks`.
- Produces: `Collection { id, name, description, kind, status }` (serialized to the frontend); Tauri commands `get_collections() -> Result<Vec<Collection>, String>`, `create_collection(name: String, description: Option<String>, kind: Option<String>) -> Result<Collection, String>`, `archive_collection(id: String) -> Result<(), String>`. Task 4 (tauri.ts) calls these three commands by these exact names and argument shapes.

- [ ] **Step 1: Add the `Collection` struct**

In `vox-desktop/src-tauri/src/lib.rs`, near the other data structs (after `DesktopTask`/`CreateTaskPayload`), add:

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Collection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_collection_kind")]
    pub kind: String,
    #[serde(default = "default_collection_status")]
    pub status: String,
}

fn default_collection_kind() -> String {
    "project".to_string()
}

fn default_collection_status() -> String {
    "active".to_string()
}
```

- [ ] **Step 2: Add the three commands**

Unlike tasks, collections have no local cache and no Supabase-direct path — this is a new feature with a single source of truth. Add these commands near `get_tasks`/`create_task`:

```rust
#[tauri::command]
async fn get_collections(auth: State<'_, AuthManager>) -> Result<Vec<Collection>, String> {
    let session = auth.current_session().ok_or("Not signed in")?;
    let config = auth.config();
    let client = reqwest::Client::new();
    let url = format!("{}/v1/collections", config.api_url.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .header("authorization", format!("Bearer {}", session.vox_token))
        .timeout(std::time::Duration::from_millis(5000))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("collections request failed: {}", resp.status()));
    }
    resp.json::<Vec<Collection>>().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn create_collection(
    name: String,
    description: Option<String>,
    kind: Option<String>,
    auth: State<'_, AuthManager>,
) -> Result<Collection, String> {
    let session = auth.current_session().ok_or("Not signed in")?;
    let config = auth.config();
    let client = reqwest::Client::new();
    let url = format!("{}/v1/collections", config.api_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "name": name,
        "description": description.unwrap_or_default(),
        "kind": kind.unwrap_or_else(|| "project".to_string()),
    });
    let resp = client
        .post(&url)
        .header("authorization", format!("Bearer {}", session.vox_token))
        .json(&body)
        .timeout(std::time::Duration::from_millis(5000))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("create collection failed: {}", resp.status()));
    }
    resp.json::<Collection>().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn archive_collection(id: String, auth: State<'_, AuthManager>) -> Result<(), String> {
    let session = auth.current_session().ok_or("Not signed in")?;
    let config = auth.config();
    let client = reqwest::Client::new();
    let url = format!(
        "{}/v1/collections/{}",
        config.api_url.trim_end_matches('/'),
        id
    );
    let resp = client
        .delete(&url)
        .header("authorization", format!("Bearer {}", session.vox_token))
        .timeout(std::time::Duration::from_millis(5000))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() || resp.status().as_u16() == 404 {
        Ok(())
    } else {
        Err(format!("archive collection failed: {}", resp.status()))
    }
}
```

- [ ] **Step 3: Register the commands**

Find the `tauri::generate_handler!` (or `.invoke_handler(tauri::generate_handler![...])`) call that already lists `get_tasks, create_task, update_task` (search for `get_tasks,` in `lib.rs`) and add the three new commands to that same list:

```rust
            get_collections,
            create_collection,
            archive_collection,
```

- [ ] **Step 4: Build check**

Run: `cd vox-desktop/src-tauri && cargo check`
Expected: compiles with no errors.

- [ ] **Step 5: Commit**

```bash
cd vox-desktop
git add src-tauri/src/lib.rs
git commit -m "feat(projects): add get_collections/create_collection/archive_collection commands"
```

---

## Task 4: vox-desktop — `tauri.ts` API surface for collections and `collection_id`

**Files:**
- Modify: `vox-desktop/src/lib/tauri.ts`

**Interfaces:**
- Consumes: the Tauri commands from Tasks 2 and 3, by name.
- Produces: `Collection` type, `api.getCollections()`, `api.createCollection(payload)`, `api.archiveCollection(id)`; `DesktopTask.collection_id`, `GetTasksArgs.collection_id`, `CreateTaskPayload.collection_id`. Tasks 6, 11, 12, 13 (the React components) import and call these.

- [ ] **Step 1: Add `collection_id` to the existing task types**

In `vox-desktop/src/lib/tauri.ts`, find the `DesktopTask` type and add a field:

```ts
export type DesktopTask = {
  id: string;
  title: string;
  instruction: string;
  status: string;
  execution_type: string;
  project_name?: string | null;
  collection_id?: string | null;
  feasibility_reasoning?: string | null;
  execution_result?: unknown;
  due_at?: string | null;
  created_at?: string | null;
  completed_at?: string | null;
};
```

Find `GetTasksArgs` and add:

```ts
export type GetTasksArgs = {
  page?: number;
  page_size?: number;
  status?: string;
  search?: string;
  collection_id?: string;
};
```

Find `CreateTaskPayload` and add:

```ts
export type CreateTaskPayload = {
  title: string;
  instruction?: string;
  execution_type?: string;
  project_name?: string;
  collection_id?: string;
  due_at?: string;
};
```

- [ ] **Step 2: Add the `Collection` type and API methods**

Add a new type near the task types:

```ts
export type Collection = {
  id: string;
  name: string;
  description: string;
  kind: string;
  status: string;
};

export type CreateCollectionPayload = {
  name: string;
  description?: string;
  kind?: string;
};
```

In the `api` object (where `getTasks`, `createTask`, etc. are defined), add:

```ts
  getCollections: () => invoke<Collection[]>("get_collections"),
  createCollection: (payload: CreateCollectionPayload) =>
    invoke<Collection>("create_collection", payload),
  archiveCollection: (id: string) => invoke<void>("archive_collection", { id }),
```

- [ ] **Step 3: Build check**

Run: `cd vox-desktop && npm run build`
Expected: builds with no TypeScript errors. (This will show errors in files that construct `CreateTaskPayload`/`GetTasksArgs` object literals if any required-field assumptions changed — there shouldn't be any, since every new field is optional.)

- [ ] **Step 4: Commit**

```bash
cd vox-desktop
git add src/lib/tauri.ts
git commit -m "feat(projects): add collections API surface to tauri.ts"
```

---

## Task 5: vox-desktop — extract `TaskTable`/`EmptyTasks` into a shared component

**Files:**
- Create: `vox-desktop/src/components/task-table.tsx`
- Modify: `vox-desktop/src/components/tasks-view.tsx`

**Interfaces:**
- Consumes: `DesktopTask` (from `@/lib/tauri`).
- Produces: `TaskTable({ tasks, onToggleStatus, onInspect })` and `EmptyTasks({ onNewTask })`, exported from `task-table.tsx`. Task 12 (`project-detail-view.tsx`) imports both.

- [ ] **Step 1: Create `task-table.tsx` with the extracted components**

Move `EmptyTasks` and `TaskTable` (currently lines 147-254 of `tasks-view.tsx`, unchanged) into a new file:

```tsx
import { Check, Clock, Plus, Sparkles } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { VoxLogo } from "@/components/vox-logo";
import { statusBadgeVariant } from "@/lib/status";
import type { DesktopTask } from "@/lib/tauri";
import { cn } from "@/lib/utils";

export function EmptyTasks({ onNewTask }: { onNewTask: () => void }) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 py-20 text-center">
      <VoxLogo size={54} />
      <h3 className="text-lg font-medium">No tasks found</h3>
      <p className="max-w-sm text-sm text-ash">
        Ask your Vox agent to create an autonomous task or click New Task to track one.
      </p>
      <Button className="shadow-btn-lift mt-2 gap-1.5" onClick={onNewTask}>
        <Plus className="size-4" />
        Create a Task
      </Button>
    </div>
  );
}

export function TaskTable({
  tasks,
  onToggleStatus,
  onInspect,
}: {
  tasks: DesktopTask[];
  onToggleStatus: (task: DesktopTask) => void;
  onInspect: (task: DesktopTask) => void;
}) {
  return (
    <table className="mt-3 w-full border-collapse text-left text-sm">
      <thead>
        <tr className="border-b border-border text-[11px] uppercase tracking-wide text-smoke">
          <th className="py-2 pr-3 font-medium">Status</th>
          <th className="py-2 pr-3 font-medium">Task & Instruction</th>
          <th className="py-2 pr-3 font-medium">Execution Type</th>
          <th className="py-2 pr-3 font-medium">Project</th>
          <th className="py-2 pr-3 font-medium">Due</th>
          <th className="py-2 font-medium">Actions</th>
        </tr>
      </thead>
      <tbody>
        {tasks.map((task) => {
          const done = task.status === "completed";
          const exec = task.status === "executing";
          return (
            <tr
              key={task.id}
              className={cn("border-b border-border/60", done && "opacity-60")}
            >
              <td className="py-3 pr-3 align-top">
                <Badge
                  variant={statusBadgeVariant(task.status)}
                  className={cn(
                    exec && "border-coral-pulse/35 bg-ember-hush text-coral-pulse",
                    done && "border-success-green/28 bg-success-green/12 text-success-green",
                  )}
                >
                  {exec ? (
                    <span className="size-1.5 animate-pulse rounded-full bg-coral-pulse shadow-[0_0_6px_#ff6363]" />
                  ) : done ? (
                    <Check className="size-3" />
                  ) : null}
                  {task.status}
                </Badge>
              </td>
              <td className="py-3 pr-3 align-top">
                <div className="font-medium text-pure-white">{task.title}</div>
                <div className="mt-0.5 line-clamp-2 text-xs text-ash">
                  {task.instruction}
                </div>
              </td>
              <td className="py-3 pr-3 align-top">
                <Badge variant="outline" className="gap-1">
                  {task.execution_type === "autonomous" ? (
                    <Sparkles className="size-3 text-coral-pulse" />
                  ) : null}
                  {task.execution_type}
                </Badge>
              </td>
              <td className="py-3 pr-3 align-top">
                <Badge variant="outline">{task.project_name ?? "General"}</Badge>
              </td>
              <td className="py-3 pr-3 align-top">
                <div className="flex items-center gap-1.5 text-ash">
                  <Clock className="size-3.5" />
                  <span>{task.due_at ?? "—"}</span>
                </div>
              </td>
              <td className="py-3 align-top">
                <div className="flex items-center gap-1.5">
                  <Button
                    variant="secondary"
                    size="icon"
                    className="size-8"
                    title={done ? "Reopen" : "Complete"}
                    onClick={() => onToggleStatus(task)}
                  >
                    <Check className="size-3.5" />
                  </Button>
                  <Button variant="ghost" size="sm" onClick={() => onInspect(task)}>
                    View
                  </Button>
                </div>
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}
```

- [ ] **Step 2: Update `tasks-view.tsx` to import from the new file**

In `vox-desktop/src/components/tasks-view.tsx`:

1. Delete the `EmptyTasks` and `TaskTable` function definitions (lines 147-254).
2. Replace the import block at the top — remove `Check, Clock, Sparkles` from the `lucide-react` import (only `PanelLeftClose, Plus, RefreshCw` remain, since those icons are now only used inside `task-table.tsx`), remove the `statusBadgeVariant` and `cn` imports if nothing else in this file uses them (check: `cn` is not used elsewhere in `tasks-view.tsx` after the extraction — remove it; `statusBadgeVariant` likewise), and add:

```tsx
import { EmptyTasks, TaskTable } from "@/components/task-table";
```

The resulting top-of-file imports should read:

```tsx
import { PanelLeftClose, Plus, RefreshCw } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { EmptyTasks, TaskTable } from "@/components/task-table";
import type { DesktopTask } from "@/lib/tauri";
```

(The rest of `tasks-view.tsx` — the `TasksView` function itself — is unchanged; it already calls `<EmptyTasks onNewTask={onNewTask} />` and `<TaskTable tasks={tasks} onToggleStatus={onToggleStatus} onInspect={onInspect} />` exactly as before, just now resolved via import instead of local definition.)

- [ ] **Step 3: Build check**

Run: `cd vox-desktop && npm run build`
Expected: builds with no errors or unused-import warnings (this project's `tsconfig.app.json` has `noUnusedLocals: true`, so a leftover unused import fails the build, not just warns).

- [ ] **Step 4: Commit**

```bash
cd vox-desktop
git add src/components/task-table.tsx src/components/tasks-view.tsx
git commit -m "refactor(tasks): extract TaskTable/EmptyTasks into a shared component"
```

---

## Task 6: vox-desktop — real project picker in `NewTaskDialog`

**Files:**
- Modify: `vox-desktop/src/components/new-task-dialog.tsx`
- Modify: `vox-desktop/src/app.tsx` (the `newTask`/`createTask`/`emptyNewTask` wiring, and passing `collections` down)

**Interfaces:**
- Consumes: `Collection[]` (from Task 4).
- Produces: `NewTaskForm.collectionId: string` (replaces `project: string`); `NewTaskDialog` gains a `collections: Collection[]` prop.

- [ ] **Step 1: Update `NewTaskForm` and the dialog's project field**

In `vox-desktop/src/components/new-task-dialog.tsx`, change the type:

```tsx
export type NewTaskForm = {
  title: string;
  instruction: string;
  execType: string;
  collectionId: string;
  due: string;
};
```

Add `Collection` to the imports:

```tsx
import type { Collection } from "@/lib/tauri";
```

Change the component signature to accept `collections`:

```tsx
export function NewTaskDialog({
  open,
  form,
  collections,
  onOpenChange,
  onChange,
  onSubmit,
}: {
  open: boolean;
  form: NewTaskForm;
  collections: Collection[];
  onOpenChange: (open: boolean) => void;
  onChange: (patch: Partial<NewTaskForm>) => void;
  onSubmit: () => void;
}) {
```

Replace the "Project / Collection" field:

```tsx
            <div className="grid gap-1.5">
              <Label>Project / Collection</Label>
              <Input
                value={form.project}
                onChange={(e) => onChange({ project: e.target.value })}
              />
            </div>
```

with:

```tsx
            <div className="grid gap-1.5">
              <Label>Project / Collection</Label>
              <Select
                value={form.collectionId || "none"}
                onValueChange={(v) => onChange({ collectionId: v === "none" ? "" : v })}
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="none">No project</SelectItem>
                  {collections.map((c) => (
                    <SelectItem key={c.id} value={c.id}>
                      {c.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
```

(`Select`/`SelectTrigger`/`SelectValue`/`SelectContent`/`SelectItem` are already imported at the top of this file; `Input` stays imported since the "Task Title", "Detailed Instruction", and "Due Date" fields still use it.)

- [ ] **Step 2: Update `app.tsx`'s task-creation wiring**

In `vox-desktop/src/app.tsx`:

Change `emptyNewTask`:

```ts
const emptyNewTask: NewTaskForm = {
  title: "",
  instruction: "",
  execType: "autonomous",
  collectionId: "",
  due: "Today",
};
```

Change `createTask`:

```ts
  async function createTask() {
    const title = newTask.title.trim();
    if (!title) return;
    await api.createTask({
      title,
      instruction: newTask.instruction.trim() || undefined,
      execution_type: newTask.execType,
      collection_id: newTask.collectionId || undefined,
      due_at: newTask.due,
    });
    setNewTask(emptyNewTask);
    setShowNewTask(false);
    await loadTasks();
  }
```

Add a `collections` state and loader near the existing `tasks`/`loadTasks` state (this is also reused by Task 13, but the dialog needs it now, so it's introduced here):

```ts
  const [collections, setCollections] = useState<Collection[]>([]);

  const loadCollections = useCallback(async () => {
    try {
      const res = await api.getCollections();
      setCollections(res);
    } catch {
      /* keep previous list */
    }
  }, []);

  useEffect(() => {
    if (auth.signed_in) void loadCollections();
  }, [auth.signed_in, loadCollections]);
```

Add `Collection` to the `@/lib/tauri` import list at the top of the file (alongside the existing `AuthState`, `DesktopTask` type imports).

Find where `<NewTaskDialog` is rendered and add the `collections` prop:

```tsx
        form={newTask}
        collections={collections}
```

- [ ] **Step 3: Build check**

Run: `cd vox-desktop && npm run build`
Expected: builds with no errors.

- [ ] **Step 4: Commit**

```bash
cd vox-desktop
git add src/components/new-task-dialog.tsx src/app.tsx
git commit -m "feat(projects): replace free-text project field with a real collection picker"
```

---

## Task 7: vox-desktop — add "Projects" to the home rail

**Files:**
- Modify: `vox-desktop/src/components/home-rail.tsx`

**Interfaces:**
- Consumes: nothing new.
- Produces: `HomeRailId` gains `"projects"`. Task 8 consumes this to add a `handleRailSelect` branch.

- [ ] **Step 1: Add the type and rail item**

In `vox-desktop/src/components/home-rail.tsx`, add `FolderKanban` to the `lucide-react` import:

```tsx
import {
  BarChart3,
  BookOpen,
  Database,
  FolderKanban,
  ListTodo,
  Mic,
  Settings,
} from "lucide-react";
```

Add `"projects"` to the type:

```tsx
export type HomeRailId =
  | "lms"
  | "tasks"
  | "projects"
  | "voice"
  | "data"
  | "analytics"
  | "settings";
```

Add a new entry to `ITEMS`, right after the `"tasks"` entry:

```tsx
  {
    id: "projects",
    label: "Projects",
    hint: "Tasks, notes, and datasets by project",
    icon: FolderKanban,
    accent: "hover:text-electric-sky data-[active=true]:text-electric-sky",
  },
```

- [ ] **Step 2: Build check**

Run: `cd vox-desktop && npm run build`
Expected: builds with no errors.

- [ ] **Step 3: Commit**

```bash
cd vox-desktop
git add src/components/home-rail.tsx
git commit -m "feat(projects): add Projects entry to the home rail"
```

---

## Task 8: vox-desktop — wire the rail item to navigation

**Files:**
- Modify: `vox-desktop/src/components/dashboard-view.tsx` (`DashboardView` props, `handleRailSelect`)
- Modify: `vox-desktop/src/components/app-sidebar.tsx` (`DesktopView` type)

**Interfaces:**
- Consumes: `HomeRailId` (Task 7).
- Produces: `DashboardView` gains an `onOpenProjects: () => void` prop; `DesktopView` gains `"projects"`. Task 13 (`app.tsx`) supplies both.

- [ ] **Step 1: Extend `DesktopView`**

In `vox-desktop/src/components/app-sidebar.tsx`:

```tsx
export type DesktopView = "dashboard" | "tasks" | "projects";
```

(`AppSidebar` itself is not rendered anywhere and is not otherwise modified by this plan.)

- [ ] **Step 2: Add the `onOpenProjects` prop and rail branch**

In `vox-desktop/src/components/dashboard-view.tsx`, find `handleRailSelect`:

```tsx
  function handleRailSelect(id: HomeRailId) {
    if (id === "tasks") {
      setRailActive(null);
      onOpenTasks();
      return;
    }
    if (id === "settings") {
      setRailActive("settings");
      onOpenSettings();
      return;
    }
    if (id === "voice") {
      setRailActive((prev) => (prev === "voice" ? null : "voice"));
      return;
    }
    setRailActive((prev) => (prev === id ? null : id));
  }
```

Add a `"projects"` branch, following the same pattern as `"tasks"`:

```tsx
  function handleRailSelect(id: HomeRailId) {
    if (id === "tasks") {
      setRailActive(null);
      onOpenTasks();
      return;
    }
    if (id === "projects") {
      setRailActive(null);
      onOpenProjects();
      return;
    }
    if (id === "settings") {
      setRailActive("settings");
      onOpenSettings();
      return;
    }
    if (id === "voice") {
      setRailActive((prev) => (prev === "voice" ? null : "voice"));
      return;
    }
    setRailActive((prev) => (prev === id ? null : id));
  }
```

Add `onOpenProjects` to the `DashboardView` function's destructured props and its type block (find `onOpenTasks,` in both the destructuring list and the type object — add `onOpenProjects,` right after it in the destructuring list, and `onOpenProjects: () => void;` right after `onOpenTasks: () => void;` in the type block).

- [ ] **Step 3: Build check**

Run: `cd vox-desktop && npm run build`
Expected: fails at this step — `app.tsx` doesn't pass `onOpenProjects` to `<DashboardView>` yet. That's expected; Task 13 supplies it. Confirm the only error is the missing `onOpenProjects` prop on the `<DashboardView>` call site in `app.tsx`, not something else — if there's a different error, stop and re-check this task's edits before moving on.

- [ ] **Step 4: Commit**

```bash
cd vox-desktop
git add src/components/dashboard-view.tsx src/components/app-sidebar.tsx
git commit -m "feat(projects): wire the Projects rail item to a new onOpenProjects callback"
```

---

## Task 9: vox-desktop — `new-project-dialog.tsx`

**Files:**
- Create: `vox-desktop/src/components/new-project-dialog.tsx`

**Interfaces:**
- Consumes: nothing new.
- Produces: `NewProjectForm` type, `NewProjectDialog` component. Task 10 (`projects-view.tsx`) renders it.

- [ ] **Step 1: Create the dialog**

Mirrors `new-task-dialog.tsx`'s structure with a smaller form (name, description, kind):

```tsx
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";

export type NewProjectForm = {
  name: string;
  description: string;
  kind: string;
};

export function NewProjectDialog({
  open,
  form,
  onOpenChange,
  onChange,
  onSubmit,
}: {
  open: boolean;
  form: NewProjectForm;
  onOpenChange: (open: boolean) => void;
  onChange: (patch: Partial<NewProjectForm>) => void;
  onSubmit: () => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="shadow-key border-0 bg-ink sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>New Project</DialogTitle>
        </DialogHeader>
        <div className="grid gap-4">
          <div className="grid gap-1.5">
            <Label>Name</Label>
            <Input
              placeholder="e.g. Home Renovation"
              value={form.name}
              onChange={(e) => onChange({ name: e.target.value })}
            />
          </div>
          <div className="grid gap-1.5">
            <Label>Description</Label>
            <Textarea
              rows={3}
              placeholder="What this project is for…"
              value={form.description}
              onChange={(e) => onChange({ description: e.target.value })}
            />
          </div>
          <div className="grid gap-1.5">
            <Label>Kind</Label>
            <Select
              value={form.kind}
              onValueChange={(v) => v && onChange({ kind: v })}
            >
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="project">Project</SelectItem>
                <SelectItem value="trip">Trip</SelectItem>
                <SelectItem value="course">Course</SelectItem>
                <SelectItem value="area">Area</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>
        <DialogFooter>
          <Button variant="secondary" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button className="shadow-btn-lift" onClick={onSubmit}>
            Create Project
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
```

- [ ] **Step 2: Build check**

Run: `cd vox-desktop && npm run build`
Expected: builds with no errors (this file isn't imported by anything yet, so it just needs to type-check standalone).

- [ ] **Step 3: Commit**

```bash
cd vox-desktop
git add src/components/new-project-dialog.tsx
git commit -m "feat(projects): add new-project-dialog component"
```

---

## Task 10: vox-desktop — `projects-view.tsx`

**Files:**
- Create: `vox-desktop/src/components/projects-view.tsx`

**Interfaces:**
- Consumes: `Collection` (Task 4), `NewProjectDialog`/`NewProjectForm` (Task 9).
- Produces: `ProjectsView({ collections, onSelectProject, onCreateProject, onArchiveProject, onCollapse })`. Task 13 (`app.tsx`) renders it.

- [ ] **Step 1: Create the view**

```tsx
import { useState } from "react";
import { Archive, PanelLeftClose, Plus } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  NewProjectDialog,
  type NewProjectForm,
} from "@/components/new-project-dialog";
import { VoxLogo } from "@/components/vox-logo";
import type { Collection } from "@/lib/tauri";

const emptyNewProject: NewProjectForm = {
  name: "",
  description: "",
  kind: "project",
};

export function ProjectsView({
  collections,
  onSelectProject,
  onCreateProject,
  onArchiveProject,
  onCollapse,
}: {
  collections: Collection[];
  onSelectProject: (id: string) => void;
  onCreateProject: (form: NewProjectForm) => Promise<void>;
  onArchiveProject: (id: string) => void;
  onCollapse: () => void;
}) {
  const [showNew, setShowNew] = useState(false);
  const [form, setForm] = useState<NewProjectForm>(emptyNewProject);

  const active = collections.filter((c) => c.status !== "archived");

  async function handleSubmit() {
    if (!form.name.trim()) return;
    await onCreateProject(form);
    setForm(emptyNewProject);
    setShowNew(false);
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center justify-between border-b border-border bg-ink px-7 py-4">
        <div className="flex items-center gap-3">
          <h1 className="text-lg font-semibold tracking-tight">Projects</h1>
          <Badge variant="secondary" className="font-mono">
            {active.length} projects
          </Badge>
        </div>
        <div className="no-drag flex items-center gap-2.5">
          <Button size="sm" className="shadow-btn-lift gap-1.5" onClick={() => setShowNew(true)}>
            <Plus className="size-4" />
            New Project
          </Button>
          <Button
            variant="secondary"
            size="icon"
            title="Collapse to Dashboard (Esc)"
            onClick={onCollapse}
          >
            <PanelLeftClose className="size-4" />
          </Button>
        </div>
      </header>

      <div className="no-drag min-h-0 flex-1 overflow-auto px-7 py-6">
        {active.length === 0 ? (
          <div className="flex flex-col items-center justify-center gap-3 py-20 text-center">
            <VoxLogo size={54} />
            <h3 className="text-lg font-medium">No projects yet</h3>
            <p className="max-w-sm text-sm text-ash">
              Create a project to group tasks, notes, and datasets together.
            </p>
            <Button className="shadow-btn-lift mt-2 gap-1.5" onClick={() => setShowNew(true)}>
              <Plus className="size-4" />
              New Project
            </Button>
          </div>
        ) : (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {active.map((c) => (
              <button
                key={c.id}
                type="button"
                onClick={() => onSelectProject(c.id)}
                className="group flex flex-col items-start gap-2 rounded-xl border border-border bg-obsidian p-4 text-left transition hover:border-electric-sky/50"
              >
                <div className="flex w-full items-center justify-between">
                  <span className="font-medium text-pure-white">{c.name}</span>
                  <Badge variant="outline">{c.kind}</Badge>
                </div>
                {c.description ? (
                  <p className="line-clamp-2 text-xs text-ash">{c.description}</p>
                ) : null}
                <div className="mt-auto flex w-full items-center justify-between pt-2">
                  <Badge variant="secondary">{c.status}</Badge>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="size-7 opacity-0 transition group-hover:opacity-100"
                    title="Archive project"
                    onClick={(e) => {
                      e.stopPropagation();
                      onArchiveProject(c.id);
                    }}
                  >
                    <Archive className="size-3.5" />
                  </Button>
                </div>
              </button>
            ))}
          </div>
        )}
      </div>

      <NewProjectDialog
        open={showNew}
        form={form}
        onOpenChange={setShowNew}
        onChange={(patch) => setForm((prev) => ({ ...prev, ...patch }))}
        onSubmit={() => void handleSubmit()}
      />
    </div>
  );
}
```

- [ ] **Step 2: Build check**

Run: `cd vox-desktop && npm run build`
Expected: builds with no errors.

- [ ] **Step 3: Commit**

```bash
cd vox-desktop
git add src/components/projects-view.tsx
git commit -m "feat(projects): add projects-view component"
```

---

## Task 11: vox-desktop — `project-detail-view.tsx`

**Files:**
- Create: `vox-desktop/src/components/project-detail-view.tsx`

**Interfaces:**
- Consumes: `Collection` (Task 4), `TaskTable`/`EmptyTasks` (Task 5), `api.getTasks`/`api.updateTask` (existing, extended by Task 2/4).
- Produces: `ProjectDetailView({ project, onBack, onInspectTask })`. Task 13 renders it.

- [ ] **Step 1: Create the view**

```tsx
import { useCallback, useEffect, useState } from "react";
import { ArrowLeft } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { EmptyTasks, TaskTable } from "@/components/task-table";
import { api, type Collection, type DesktopTask } from "@/lib/tauri";

function ComingLater({ label }: { label: string }) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 py-16 text-center">
      <p className="text-sm text-ash">{label} — coming in a later update.</p>
    </div>
  );
}

export function ProjectDetailView({
  project,
  onBack,
  onInspectTask,
}: {
  project: Collection;
  onBack: () => void;
  onInspectTask: (task: DesktopTask) => void;
}) {
  const [tasks, setTasks] = useState<DesktopTask[]>([]);
  const [loading, setLoading] = useState(false);

  const loadTasks = useCallback(async () => {
    setLoading(true);
    try {
      const res = await api.getTasks({
        page: 1,
        page_size: 50,
        collection_id: project.id,
      });
      setTasks(res.items ?? []);
    } catch {
      /* keep previous list */
    } finally {
      setLoading(false);
    }
  }, [project.id]);

  useEffect(() => {
    void loadTasks();
  }, [loadTasks]);

  async function toggleStatus(task: DesktopTask) {
    await api.updateTask({
      task_id: task.id,
      status: task.status === "completed" ? "pending" : "completed",
    });
    await loadTasks();
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex items-center gap-3 border-b border-border bg-ink px-7 py-4">
        <Button variant="secondary" size="icon" onClick={onBack} title="Back to Projects">
          <ArrowLeft className="size-4" />
        </Button>
        <h1 className="text-lg font-semibold tracking-tight">{project.name}</h1>
        <Badge variant="secondary">{project.status}</Badge>
        <Badge variant="outline">{project.kind}</Badge>
      </header>

      <div className="no-drag min-h-0 flex-1 overflow-auto px-7 py-6">
        <Tabs defaultValue="tasks">
          <TabsList>
            <TabsTrigger value="tasks">Tasks</TabsTrigger>
            <TabsTrigger value="notes">Notes</TabsTrigger>
            <TabsTrigger value="datasets">Datasets</TabsTrigger>
            <TabsTrigger value="analytics">Analytics</TabsTrigger>
            <TabsTrigger value="reports">Reports</TabsTrigger>
          </TabsList>
          <TabsContent value="tasks">
            {loading ? null : tasks.length === 0 ? (
              <EmptyTasks onNewTask={() => undefined} />
            ) : (
              <TaskTable
                tasks={tasks}
                onToggleStatus={(t) => void toggleStatus(t)}
                onInspect={onInspectTask}
              />
            )}
          </TabsContent>
          <TabsContent value="notes">
            <ComingLater label="Notes" />
          </TabsContent>
          <TabsContent value="datasets">
            <ComingLater label="Datasets" />
          </TabsContent>
          <TabsContent value="analytics">
            <ComingLater label="Analytics" />
          </TabsContent>
          <TabsContent value="reports">
            <ComingLater label="Reports" />
          </TabsContent>
        </Tabs>
      </div>
    </div>
  );
}
```

(`EmptyTasks`'s `onNewTask` is a no-op here — this view has no "create task" entry point of its own in this round; clicking it does nothing, which is acceptable for an empty state that mainly communicates "no tasks in this project yet." Wiring project detail's empty state to open the New Task dialog pre-filled with this project is reasonable future polish, not required for this plan.)

- [ ] **Step 2: Build check**

Run: `cd vox-desktop && npm run build`
Expected: builds with no errors.

- [ ] **Step 3: Commit**

```bash
cd vox-desktop
git add src/components/project-detail-view.tsx
git commit -m "feat(projects): add project-detail-view component"
```

---

## Task 12: vox-desktop — wire it all together in `app.tsx`

**Files:**
- Modify: `vox-desktop/src/app.tsx`

**Interfaces:**
- Consumes: everything from Tasks 6, 7, 8, 10, 11.
- Produces: a working `"projects"` view end to end.

- [ ] **Step 1: Add imports**

Add to the top of `vox-desktop/src/app.tsx`:

```tsx
import { ProjectsView } from "@/components/projects-view";
import { ProjectDetailView } from "@/components/project-detail-view";
import type { NewProjectForm } from "@/components/new-project-dialog";
```

Add `Collection` to the existing `@/lib/tauri` type import (this may already be done in Task 6 — if so, skip; otherwise add it here).

- [ ] **Step 2: Add selected-project state**

Near the other `useState` declarations:

```ts
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
```

- [ ] **Step 3: Add `onOpenProjects` and reset selection on view change**

`handleViewChange` currently reads:

```ts
  function handleViewChange(next: DesktopView) {
    setView(next);
    if (next === "tasks") void loadTasks();
  }
```

Change it to also refresh collections when entering the Projects view, and clear any previously-selected project so navigating back to Projects always starts at the list:

```ts
  function handleViewChange(next: DesktopView) {
    setView(next);
    if (next === "tasks") void loadTasks();
    if (next === "projects") {
      setSelectedProjectId(null);
      void loadCollections();
    }
  }
```

- [ ] **Step 4: Pass `onOpenProjects` to `DashboardView`**

Find the `<DashboardView` call site and add:

```tsx
            onOpenProjects={() => handleViewChange("projects")}
```

(alongside the existing `onOpenTasks={() => handleViewChange("tasks")}`).

- [ ] **Step 5: Add project create/archive handlers**

```ts
  async function createProject(form: NewProjectForm) {
    await api.createCollection({
      name: form.name.trim(),
      description: form.description.trim() || undefined,
      kind: form.kind,
    });
    await loadCollections();
  }

  async function archiveProject(id: string) {
    await api.archiveCollection(id);
    if (selectedProjectId === id) setSelectedProjectId(null);
    await loadCollections();
  }
```

- [ ] **Step 6: Extend the view-render logic to a three-way branch**

Replace:

```tsx
        {view === "dashboard" ? (
          <DashboardView
            ...
          />
        ) : (
          <TasksView
            ...
          />
        )}
```

with:

```tsx
        {view === "dashboard" ? (
          <DashboardView
            orbState={orbState}
            orbSpeed={orbSpeed}
            isActive={isActive}
            isSpeaking={isSpeaking}
            callState={callState}
            label={label}
            subLabel={subLabel}
            callError={callError}
            pendingCount={pendingCount}
            onToggleCall={() => void toggleCall()}
            onOpenTasks={() => handleViewChange("tasks")}
            onOpenProjects={() => handleViewChange("projects")}
            onOpenSettings={() => setShowProfile((v) => !v)}
          />
        ) : view === "tasks" ? (
          <TasksView
            tasks={tasks}
            totalTasks={totalTasks}
            page={page}
            totalPages={totalPages}
            pageSize={PAGE_SIZE}
            filter={filter}
            search={search}
            tasksLoading={tasksLoading}
            onFilterChange={(v) => {
              setFilter(v);
              setPage(1);
            }}
            onSearchChange={(v) => {
              setSearch(v);
              setPage(1);
            }}
            onReload={() => void loadTasks()}
            onNewTask={() => setShowNewTask(true)}
            onCollapse={() => setView("dashboard")}
            onPageChange={setPage}
            onToggleStatus={(task) => void toggleTaskStatus(task)}
            onInspect={setInspectTask}
          />
        ) : selectedProjectId ? (
          <ProjectDetailView
            project={
              collections.find((c) => c.id === selectedProjectId) ?? {
                id: selectedProjectId,
                name: "Project",
                description: "",
                kind: "project",
                status: "active",
              }
            }
            onBack={() => setSelectedProjectId(null)}
            onInspectTask={setInspectTask}
          />
        ) : (
          <ProjectsView
            collections={collections}
            onSelectProject={setSelectedProjectId}
            onCreateProject={createProject}
            onArchiveProject={(id) => void archiveProject(id)}
            onCollapse={() => setView("dashboard")}
          />
        )}
```

(Keep every existing prop on `DashboardView` and `TasksView` exactly as they were — this step only adds the new `onOpenProjects` prop to `DashboardView` and adds the two new branches; it doesn't remove or rename anything on the first two branches.)

- [ ] **Step 7: Build check**

Run: `cd vox-desktop && npm run build`
Expected: builds with no errors. This is the integration point for the whole plan — if any earlier task's interface doesn't line up (a prop name mismatch, a missing export), it surfaces here.

- [ ] **Step 8: Full app build**

Run: `cd vox-desktop && npm run tauri build` (or `set -a; source .env; set +a; npm run tauri build` if `.env` isn't already exported in the shell)
Expected: produces `target/release/bundle/macos/Vox.app` with no errors.

- [ ] **Step 9: Commit**

```bash
cd vox-desktop
git add src/app.tsx
git commit -m "feat(projects): wire Projects view into app navigation"
```

---

## Manual verification (no automated tests this round)

After Task 12, install the built `.app` and check by hand:

1. Click "Projects" in the home rail → empty state shows ("No projects yet").
2. Create a project (name required — try submitting blank first and confirm nothing happens) → it appears in the grid.
3. Open a task's "Project / Collection" picker in New Task → the created project appears alongside "No project" (the default); create one task with "No project" left selected (confirm it's created fine, showing "General" in the Tasks view), then create a second task with the real project selected.
4. Open the project's detail page → its Tasks tab shows that task; Notes/Datasets/Analytics/Reports tabs show "coming in a later update".
5. Go to the main Tasks view → the same task still shows up there too (unfiltered), with its project name in the Project column.
6. Archive the project → it disappears from the Projects grid; the task it contained still exists and still shows in the main Tasks view (not deleted).
