# Projects

Status words used below: **Built** means it is in code today. **Built, not verified** means it is in code but has not been tested end to end with real accounts. **Planned** means agreed direction, not built. **Open** means it still needs a decision. Facts come from the code and docs in this repo as of 2026-10-15.

## 1. What it is

A Project is a long-lived place to understand something you are working on. You add your own material to it, such as a code repository, specifications, notes, a PRD or data files. Vox reads that material on your device, answers questions about it with a diagram, and keeps a set of **context** statements that every Plan in the project starts from.

Plans end. Real work does not. A Plan has one goal and finishes; a Project lasts for months and feeds many Plans. Each Plan can add what it learned back into the Project, so the next Plan starts already informed.

Examples: an engineering codebase, a product launch, a marketing campaign, a side project, office work, a 3D-printing build.

## 2. Why it matters (the pitch in plain words)

- Ask "how does this work?" about your own material and get a diagram that explains it, built from what is actually in your files, not from how such things usually work.
- Every statement and diagram element can point to the exact file and lines it came from. Click it and read the source.
- Your files stay on your computer. Only the short excerpts a question needs are sent to the model.
- Context does not rot silently. When a cited file changes, Vox notices and asks you to review the affected context.
- You stay in charge. Context you write or edit wins over anything the model suggests, and every edit keeps the earlier versions.
- Plans start informed. A Plan attached to a Project receives its current context automatically.
- Same Project on desktop and Android. Android sees the summaries and references without needing the files.

## 3. Vision

### 3.1 The intent

The purpose of a Project is to **explain**: a pipeline, an idea, a flow, a decision, a document, in detail and down to the truth. Vox should understand the material well enough that you could share its explanation with someone who has never seen the material, and they would understand it.

### 3.2 Principles

- **Generic, not use-case specific.** The same Project and the same explanation agent work for any material: code, documents, PRDs, specs, data, notes. Nothing in the product is tuned to one domain, one folder layout or one kind of question.
- **The agent decides how to explain.** Vox gives the agent the intent and the building blocks (diagram schemas, source tools). The agent decides what a box means, what an arrow means, which representation fits, what to include and how to lay it out. Vox does not predefine boxes or arrows.
- **Truth to the bottom.** The agent keeps following the material wherever it leads, across files and sources, until it understands the whole answer. It does not stop at a boundary because the next part is harder to reach.
- **No made-up or fallback data.** Nothing is invented, filled in from general knowledge or shown as a placeholder. If something is not in the sources, Vox says so plainly instead of showing a guess that looks real.
- **Evidence over trust.** Claims point to the files and lines that establish them. Inferred things are visibly inferred.
- **Local first.** Files, indexes and raw chunks stay on the device. The server holds short summaries and references only.
- **User edits win.** The model proposes; the user decides. History is kept for every change.

### 3.3 Where it is going

1. A Project you can understand at a glance: an explanation canvas for the questions you asked, plus the context that stays current.
2. Context that keeps itself honest: precise drift detection per cited file, and quick "is this still right?" reviews only where it matters.
3. Material beyond text and code: PDFs and other documents converted locally into readable text.
4. Large and multi-repo projects handled cheaply: free local structure first, cheap summaries next, deep reads only when a question needs them, each paid for once and saved back as context.
5. Plans and Projects working as one loop: a Plan reads the context, does the work, and proposes context updates you accept in one click.

## 4. What the user sees

### Projects list (desktop and Android)

- Search ("Find a project"), refresh, **Import project** and **New project**.
- **Import project** creates the project and opens Sources to add folders or files.
- **New project** asks "What are you working on?" plus optional notes. If notes are given they become the first context statement, a goal called "Project vision".
- Lists load 50 projects per page.

### Project page (desktop)

Header: project name, number of sources and files, **New plan**, and a menu with Edit project, Model connection, Add decision and Archive project.

| Tab | What it shows |
|---|---|
| **Canvas** | The latest explanation diagram, if one has been drawn. Otherwise the context canvas: context statements grouped by source, with a warning marker on statements that need review. |
| **Sources** | Each source with file count, symbol count and index time. Refresh, relink and remove. Changed files are shown per source. Opens the local symbol and relationship explorer. |
| **Plans** | Plans attached to this project, with state and date. **Review findings** on a finished Plan starts the next Plan from its final output. |
| **Activity** | A log of what happened (project created, source indexed, context updated, diagram created), newest 200 kept. |

Below the canvas is the composer ("Ask about your project") with **Draw diagram**. While the agent works, progress lines stream in ("Investigating: read_source · path") and the run can be stopped.

### The explanation diagram

- The agent chooses one of five representations: **architecture** (structure), **workflow** (process), **sequence** (interactions over time), **dataflow** (how something moves and changes) or **lifecycle** (state changes).
- Shown as a rendered picture with pan and zoom. Click an element or a connection to see its citations, then open the exact lines.
- "Not in sources" lists anything the answer needs that the material does not contain.
- Export as interactive HTML, SVG, or JSON with the source references.

### Context statements

- Kinds: goal, component, decision, constraint, question, asset, milestone.
- Origin: **user** (you wrote or reviewed it), **extracted** (from the source structure) or **inferred** (a model suggestion you accepted).
- Edit, remove, add your own, or restore any of the last 20 versions.
- A statement whose cited files changed shows "needs review". Mark it reviewed ("I reviewed the updated sources") once the source has been refreshed; Vox re-reads the cited lines before accepting it.
- The header shows **Context needs review** when any statement needs review.

### Android

Open Projects from Plans or the profile menu. Android supports creating projects, **Ideate** (a chat that proposes context statements you accept or dismiss), editing context and notes, restoring earlier versions, archiving, starting attached Plans, attaching existing Plans and reviewing findings. The Sources tab explains that files live on the desktop. Android does not read local files or draw diagrams.

### Voice and assistant

The assistant can open the Projects page or a specific project on the desktop (`navigate` with `page=projects` and `projectId`), including from the phone. On desktop, the assistant context includes the open project's name, source count and up to 30 context statements. Android voice can open a project too.

## 5. Sources

| What you can add | Details | Status |
|---|---|---|
| **Folders** | One or more local folders. Respects `.gitignore`. | Built |
| **Individual files** | Up to five text or code files. | Built |
| **PDF and binary documents** | Need converting to text first. | Planned |

Accepted text formats: Rust, JavaScript, TypeScript, Python, C, C++, Go, Java, Kotlin, Swift, Ruby, PHP, C#, Dart, Markdown, plain text, TOML, JSON, YAML, CSV, SQL, HTML, CSS, shell, CMake, XML, Protocol Buffers, plus `Dockerfile`, `Makefile`, `CMakeLists.txt` and `.gitignore`.

Always skipped: symlinks, files over 1 MB, binary content, generated and dependency folders (`.git`, `node_modules`, `target`, `dist`, `build`, `.venv`, `venv`, `__pycache__`, `.next`, `vendor`, `.idea`), and likely secrets (`.env*`, `*.pem`, `*.key`, anything named `credentials`, `id_rsa`, `id_ed25519`, `secrets.json`).

Limits: 15,000 files and 200 MB per source, 30 sources per project. Limits reached and skipped files are reported in Sources.

### Indexing

The first import downloads the pinned **code-graph-mcp v0.169.0** engine from its GitHub release and checks its SHA-256 checksum. Each source is indexed into a private snapshot (embeddings off) that holds definitions ("symbols") and relationships between them. Indexing also creates one **extracted** context statement per top-level area of the source (up to 60), listing its files. These statements describe structure only; they say nothing about behaviour.

### Keeping sources fresh

While a project is open on desktop, Vox checks file hashes every 30 seconds and when you start a diagram. Changed sources are re-indexed before a diagram is drawn. A source moved or missing on this device must be relinked in Sources.

## 6. Explanations (Draw diagram)

### How it works

1. Vox checks source freshness and re-indexes changed sources.
2. It starts the **Gemini CLI v0.63.0** engine on a managed Node.js 22 runtime (both pinned, with integrity checks; nothing needs installing globally). Supported on Apple Silicon and Intel Macs.
3. The agent receives one generic prompt with the intent (explain the question truthfully and completely from these sources) and only Vox's read-only tools. It has no shell, file editing or web search.
4. It explores, reads, picks a representation, drafts the diagram, previews it, fixes it and submits it.
5. Vox checks the result and saves it to the project. A text-only answer counts as a failure; an empty canvas is never saved.

Runs stop after 10 minutes or 80 model turns. Leaving the project or pressing stop cancels the run and closes everything it started.

### Tools the agent can use

| Tool | Purpose |
|---|---|
| `list_sources`, `list_files` | What material exists. |
| `search_symbols`, `graph_calls` | Find named definitions and follow indexed relationships. |
| `search_text` | Find a literal text fragment in any file. |
| `read_source` | Read up to 200 lines and receive a citation ID. Only these IDs are accepted as evidence. |
| `diagram_capabilities`, `diagram_schema` | The five representations and the full schema for each. |
| `preview_diagram` | Validate and render a draft, with repair feedback. |
| `submit_diagram` | Validate, render and save. Citations attach to elements by ID and to connections by position. Gaps are optional. |

### What Vox checks on submit

- The diagram matches its schema (rendered by the bundled **Archify 3.0.1** renderers, offline, with no remote images or brand downloads).
- Every citation is one issued in this run, and the cited lines still match the current file.
- At most 40 elements, 80 connections and 20 gaps.

Vox does **not** decide what elements or connections mean, does not require a citation on every element, and does not restyle anything. Those choices belong to the agent.

### Model connection

- **Default (Vox):** the desktop opens a private local gateway with a one-time key. Core forwards only model requests to the configured Gemini model and keeps the provider key on the server. Each user can make up to 180 model calls per 10 minutes, with requests up to 2 MB.
- **Own key (optional):** a personal Gemini API key, stored in the macOS keychain per Vox account and passed only to the engine.

## 7. Projects and Plans

- A Plan can start inside a Project (**New plan**) or be attached later. A Plan can also stay standalone.
- When an attached Plan runs, every workflow step receives the project's context as source summaries: up to 80 statements that do not need review, 32,000 characters in total, with their file references. The Plan is told to cite statement IDs and keep uncertainty.
- **Review findings** takes a finished Plan's final output and starts the next Plan from it.
- Archiving a project hides it; attached Plans and local files are kept. Projects are only removed for good when the account is deleted, which unlinks their Plans.

## 8. How a project is stored

### Server (Postgres)

| Table | What it holds |
|---|---|
| `projects` | One row per project: owner, name (1–160 characters), `data` (JSON), `revision`, `archived`, timestamps. Row-level security limits each row to its owner. Deleted with the user. |
| `spaces.project_id` | Links a Plan to a project. Set to empty if the project is deleted. |

`data` holds the template, notes (up to 12,000 characters), context statements (up to 300, each with up to 20 citations and 20 earlier versions), sources (summaries only, up to 30), messages with diagrams (up to 100) and activity (up to 200). The whole `data` is capped at 1.5 MB. A diagram's typed form is capped at 100 KB.

### Device (desktop)

Under the signed-in account's private `~/.vox/projects/<user>/<project>/`: the source registry with file hashes, the index snapshots, and one folder per diagram run with its manifest, evidence and result. Folders are private (permissions 700, files 600). Source roots are never uploaded.

## 9. How the apps ask for data

All calls go through Core's authenticated HTTP API.

| Call | Purpose |
|---|---|
| `GET /v1/me/projects/count` | Count and a revision hash. If the revision has not changed, the cached list is reused. |
| `GET /v1/me/projects?before=` | 50 projects per page, newest first, without context, messages or activity. |
| `POST /v1/me/projects` | Create. |
| `GET /v1/me/projects/{id}` | Full project. |
| `PUT /v1/me/projects/{id}` | Save with the expected revision. A mismatch returns 409 so edits never overwrite each other. |
| `DELETE /v1/me/projects/{id}` | Archive. |
| `POST /v1/me/projects/{id}/chat` | Ideate (used by Android): a reply plus up to 6 proposed statements. |
| `POST /v1/me/projects/{id}/plans/{plan}/attach` | Attach a Plan. |
| `GET /v1/me/projects/{id}/agent`, `POST /v1/me/projects/{id}/agent/{action}` | Model status and the model gateway for diagram runs. |

Every change sends a `project_updated` live event so other open devices refresh.

Caching: the project list is kept in memory and on disk (desktop `~/.vox/cache`, Android account-scoped device cache) and refetched only when the count revision changes. Sign-out clears it.

## 10. Privacy, safety and control

- **Files stay local.** Indexes, snapshots and raw text live on the device. Core stores summaries, relative file references, context history, messages and activity.
- **Only what a question needs goes to the model.** During a diagram run, the lines the agent reads go to Gemini. For Ideate, only the excerpts sent with that message.
- **Sources are data, not instructions.** File contents never steer the agent. The project's own instruction files (such as `GEMINI.md`) are not loaded.
- **Read-only agent.** Deny-by-default tool policy; only Vox's read tools are allowed. Path containment, hash and line checks on every read.
- **Secrets skipped** at import (see section 5).
- **Owner isolation.** Row-level security and owner checks on every call. The model gateway only accepts requests for the signed-in owner's project and closes when the run ends.
- **Pinned engines.** Index engine and agent runtime are pinned versions with checksum or integrity checks.

## 11. Limits and things to know

- Only text formats are readable today. PDFs and other binary documents are skipped.
- The structural index understands code best. For documents, the agent relies on text search and reading.
- Diagram runs are desktop only, on macOS.
- Extracted context statements describe folders and files, not behaviour.
- The 30-second check marks every extracted statement of a source for review when any file in that source changes, so a busy repository shows **Context needs review** almost all the time.
- Diagrams saved before the typed diagram format existed are not displayed. Draw them again.
- Android uses Ideate (chat); desktop uses Draw diagram. They do not share one experience yet.
- The composer's placeholder text, "How does the voice pipeline work?", is specific to Vox's own code and should be generic.

## 12. Plan

### Built

- Projects list, create, import, archive, search, paging and cached list (desktop and Android).
- Sources: folders and up to five files, gitignore-aware scanning, limits, secret skipping, local index, refresh, relink, remove.
- Context statements with kinds, origins, citations, 20-version history, edit, restore and review.
- Drift detection and "needs review".
- Draw diagram with the generic explanation prompt, five representations, citations, export.
- Plans attached to projects, context fed to Plan steps, Review findings.
- Android parity for everything except local files and diagrams.

### Built, not verified

- The generic explanation prompt and the relaxed submit rules (changed 2026-10-14). A real diagram run still needs checking against a known flow, such as the vox-bridge call pipeline.
- Diagram export on a signed-in desktop.

### Next

1. Verify a real diagram run end to end and compare it with a known flow.
2. Make drift detection precise: mark a statement for review only when one of its own cited files changes.
3. Make the composer placeholder generic.
4. Accept PDFs and other documents by converting them to text locally.
5. Let a diagram answer save back as context statements, so it is paid for once and Plans can use it.
6. Bring Draw diagram and Ideate into one experience, and show diagrams on Android (read-only).
7. Finished Plans propose context updates that the user accepts in one click.
8. Show a cost estimate before the first large index or run, with a user cap.
9. Cheap per-module summaries cached by content hash, for large and multi-repo projects.
10. Regenerate the OpenAPI contract and generated types for the project and agent endpoints.

### Open

- Whether project templates (engineering, launch, 3D print) come back, or the agent shapes each project from its material alone.
- Whether Insights can be scoped to a project.
- Whether anyone will ever want file contents stored on the server (current answer: no).
- Whether to compare a second index engine (codebase-memory-mcp) after real use.
- Windows and Linux support for diagram runs.

## 13. Safe things to say on the marketing site

Backed by the code:

- "Bring your project. Ask how it works. Get a diagram that explains it."
- Works with code repositories and text documents: specs, notes, Markdown, data files.
- Every element of an explanation can point to the exact file and lines it came from.
- Your files stay on your computer. Only the lines needed for a question are sent to the model.
- Vox notices when your files change and asks you to review the context that depends on them.
- Your edits win, and every earlier version is kept.
- Plans start from your project's context and can feed what they learn back.
- Architecture, workflow, sequence, dataflow and lifecycle diagrams, chosen automatically.
- Export as HTML, SVG or JSON.
- Projects sync to Android; diagrams are drawn on the Mac desktop app.

Do not claim:

- PDF, Word, slides or image support, or any binary documents.
- Diagram runs on Windows, Linux or Android.
- That diagrams are always correct or complete. Validation checks structure and citations, not meaning.
- Live or automatic re-drawing of diagrams when files change.
- Specific accuracy, speed or cost numbers, user counts or testimonials. None are on file (see `vox-web/PRODUCT.md`: no invented social proof).

## 14. Where the code lives

- Core: `services/api/routes/projects.rs`, `routes/spaces.rs` (project link on Plans); `src/domain/projects.rs`, `src/storage/projects.rs`; `src/agents/project_ideation.rs` (Ideate); `src/agents/space_workflow.rs` (project context in Plans); `src/agents/tools/desktop.rs` (voice navigation); migration `20261011000001_projects.sql`.
- Desktop UI: `src/components/projects/` (`projects-view.tsx`, `project-detail.tsx`, `project-context.tsx`, `project-sources.tsx`, `code-explorer.tsx`, `context-editor.tsx`, `diagram-canvas.tsx`, `investigation-composer.tsx`, `plan-project-link.tsx`); `src/features/projects/` (`api.ts`, `types.ts`, `use-project.ts`, `context.ts`, `list-cache.ts`).
- Desktop native: `src-tauri/src/projects/` (`mod.rs`, `sources.rs`, `engine.rs`, `graph.rs`, `agent.rs`, `agent_tools.rs`, `agent_gateway.rs`, `agent_runtime.rs`, `diagram.rs`); renderer bundle `src-tauri/resources/archify-runtime.tar.gz`.
- Android: `app/src/main/java/in/voxagent/mobile/projects/` (`ProjectsApi`, `ProjectsModels`, `ProjectsViewModel`, `ProjectsScreen`), `control/AppCommandExecutor.kt`.
- Related docs: `product-vision.md` (section 5), `vox-desktop/docs/projects-agent.md`, `timeline.md`.
