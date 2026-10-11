# Vox: Product Vision and Design Record

This document collects the vision, design decisions and open work for Vox as discussed so far. It covers the four main surfaces (Timeline, Pulse/Insights, Spaces/Plans, Projects), the platform around them, and the rules we follow when building. Sections marked **Built** exist in code today. Sections marked **Planned** are agreed direction but not yet built. Sections marked **Open** still need a decision.

---

## 1. What Vox is

Vox is a personal agent that lives on your devices (desktop, Android), knows your data, and helps you think and act. It has three jobs:

1. **Remember** what happens in your life and work (Timeline).
2. **Understand** your data over time (Pulse, to be renamed Insights).
3. **Plan** what to do next with an agent that does the research for you (Spaces, to be renamed Plans), grounded in long-lived context (Projects).

Principles that run through everything:

- **The user does not manage structure.** They write vague prompts. The agent decides what to do. No "merge this" or "extend from here" choices.
- **Local-first for personal files.** Files stay on the device. Only the excerpts a step needs go to a model.
- **Trust is visible.** Every claim says where it came from, and the product distinguishes what was read from what was inferred.
- **Fast by default.** Count first, load details lazily, cache per unit of data, and update only what changed.
- **Same product on every device.** Desktop and Android expose the same features. Where one lags, it is a known gap, not a design difference.
- **Calm, clean UI.** No glow effects on the Plans builder. Headers attach to the top like every other page. Pages share the same visual language.

---

## 2. Timeline

### 2.1 Purpose
A single chronological record of everything that happened to the user: connected-service events, device activity, locations, and agent actions. It is the raw material that Pulse charts and Plans reason over.

### 2.2 How it works (Built)
- Events are stored in Postgres per user, grouped by day. Each day carries a **server-side revision number** that changes whenever an event is added or edited on that day.
- The app asks for data **per day**. If its cached copy of a day has the same revision, nothing is downloaded.
- A **range picker** selects the period. An empty state explains when a range has no events.
- A **location card** shows where the user was for location events. Map controls were removed to keep it quiet.
- Detailed storage and API behaviour are in `timeline.md`.

### 2.3 Caching (Built)
- **Desktop:** a per-day store in memory, persisted to the device cache directory (`~/.vox/cache`) so a restart does not refetch history. Days are invalidated by revision.
- When a new event arrives, the server bumps that day's revision and sends a live update (`vox-live-update`). The app refetches only that day.
- Count-first loading shows totals immediately, then fills details.

### 2.4 Known limits
- Android has not yet received the per-day cache and count-first loading.
- A full local replica of all history was discussed and rejected for now. The cache holds what the user has viewed, not everything.

---

## 3. Pulse (to be renamed Insights)

### 3.1 Purpose
Charts over the user's own data across time: play time, steps, spend, anything the connected services report. The goal is that a user can ask "how am I doing on X?" and see an honest chart.

### 3.2 What exists (Built)
- **Per-chart results.** Each chart is computed and cached on its own, so one slow chart does not block the page.
- **Multi-series charts** in three modes, on Core, desktop and Android with the same features.
- **Compare** against a prior period (`compare_with`), returning a `PulseSeriesResult`.
- **Global time range.** One range control drives every chart, shared with Timeline.
- **Landing page** with line art, a dither/pixel treatment and subtle red pulse animation. Design decisions made along the way: original red, no bottom grid plane, no glow, metrics shown as real values (for example "34 hrs" play time), tertiary header buttons, and yellow/green call-to-action colours.
- **Count-first loading** so the page shows how many charts exist before data arrives.
- Pulse discovery and verification are documented in `pulse-discovery-verification.md`.

### 3.3 Rename (Planned)
"Pulse" is vague. The user-facing name becomes **Insights**. API paths and internal names can stay until a later cleanup.

### 3.4 Open
- Android parity for the newest caching work.
- Whether Insights should accept a Project as a lens (for example "show me only what relates to this project").

---

## 4. Spaces (to be renamed Plans)

### 4.1 Purpose
A Plan is where the user thinks out loud with an agent. They state a vision. The agent researches the web, looks at the user's own data, synthesizes findings, and proposes a plan. The result is a graph of nodes on a canvas.

### 4.2 The workflow engine (Built)
- A Plan is a **DAG of nodes and edges**. Work happens as **tasks** with four roles: `web_search`, `user_data`, `synthesis`, `plan`.
- A run starts from the goal node, spawns web research and user-context tasks, synthesizes them, and auto-proposes a plan after synthesis.
- Tasks run with **concurrency of 3** per plan, with leases, heartbeats and **automatic retry** (one extra attempt) before a task shows as failed.
- A **generation counter** invalidates stale work when the graph changes underneath it.
- **Task failures show a real reason** (provider error, invalid output, empty result, timeout) instead of a generic message. The error clears when a retry starts.
- Live progress streams to the canvas through the user event hub.
- When a run finishes with a completed plan, the plan is marked **planned**, and a `space_plan_ready` event is sent.
- **Plan-ready voice announcement.** The desktop speaks when a plan is ready, with a confirmation prompt component. Verified in a harness; not yet verified in the real app.
- Limits were raised to **60 tasks and 8 children per node** per generation (from 20 and 5).

### 4.3 The builder UI (Built)
- Redesigned with a WebGL shader background (canvas created per mount so it survives React StrictMode), animated flow edges coloured by node role, and role accents (research teal, user context amber, synthesis, plan pink, goal blue).
- **No glow effects.** The header is a normal page header attached to the top, with progress (done of total).
- Nodes render crisp: no backdrop blur, fixed zoom, rounded positions.
- A chat composer at the bottom talks to the agent. The selected node is only a hint.
- Minimap, controls, empty "architecting your space" state.

### 4.4 Branching and versions (Built, new)
The design goal: the user never chooses structure. They type something vague and the agent decides.

**Immutable edits.** Editing a node's title or body creates a **new node** that replaces the old one. The old version is kept and hidden. Links, task and downstream edges move to the new node. The selected node's panel shows "Earlier versions" with a "Use this" button, and the node shows a `v2` badge.

**Agent orchestrator.** Each user message is classified by the agent into one action:

| Action | Meaning |
|---|---|
| refine | small change to a node's content |
| extend | build further on an existing plan with new research (a new vision node under it) |
| branch | explore an alternative from an earlier node while keeping existing work |
| new root | a separate, independent idea in the same plan |
| combine | merge several plans into one |
| finalize | the user picked or approved a plan |
| answer | a question needing no change |

The agent replies in plain language, with no node or graph jargon. If classification fails or times out it falls back to a plain follow-up on the selected node.

**Final plan.** Plan nodes carry a final marker. The latest completed plan is marked final automatically, or whichever plan the agent finalizes. **Commit saves only the final plan and step nodes**, not every plan.

**Layout.** Each tree sits in its own rows, merge nodes sit to the right of their parents, and nodes do not overlap.

### 4.5 Commit (Built)
Committing turns the done plan and step nodes into spans in a new collection, and the plan becomes committed. Committed and dropped plans cannot be edited.

### 4.6 Planned changes
- **Standalone plans.** A plan does not need a project. Some plans are throwaway (a trip that never happened).
- **Abandoned state.** A plan can be marked abandoned: kept, hidden, and never fed into any context.
- **Attach to project.** The new-plan screen has an optional "Attach to project" dropdown, empty by default. A plan can be attached later.
- **Voice input.** A mic button in the vision box using the webview's built-in speech recognition, if the macOS webview supports it. Server transcription is out of scope for now.
- **Rename** Spaces to **Plans** everywhere in the desktop app.

### 4.7 Known gaps
- The new SQL (node replacement, final marker, orchestration) has not run against a real database.
- Duplicate vision nodes are possible if the server crashes mid-request and retries.
- Android lacks the plan announcer, count-first and cache work, and the branching features.
- OpenAPI contract and generated types were edited by hand, not regenerated.

---

## 5. Projects

### 5.1 Purpose
Plans end. Real work does not. A **Project** is a long-lived container for context that many plans draw on: an engineering project, a launch, a marketing campaign, a side project, office work, a 3D-printing build. Each iteration adds back to the project, so the next plan starts already informed.

### 5.2 Why a separate entity from Plans
- A Plan has one goal and finishes. Project context lasts months and serves many plans.
- Plan nodes are generated task output. Context nodes are sources of truth (architecture, data flow, decisions) that come from files and user edits and need to stay current.
- Keeping context inside one plan would force later plans to copy it or lose it.

### 5.3 Shape (Planned)
- **Project:** name, template, notes, owner.
- **Sources:** files and folders on the user's device. **Up to 5 individual files**, or a folder scanned respecting `.gitignore`.
- **Context graph:** nodes such as goal, component, decision, constraint, open question, asset, milestone. Each cites the source files and line ranges it came from.
- **Plans inside a project:** the agent receives the context graph as input. A finished plan can propose context updates, which the user approves in one click. Approved updates create a new version of the node, using the same replace-and-keep-history behaviour as plan nodes.
- **Templates** only set the starting structure: engineering (architecture, data flow), launch (audience, channels, timeline), 3D print (model, material, settings). The model is general.

### 5.4 Local-first rules
- Files, indexes and raw chunks stay on the device. The desktop app owns folder access.
- The server stores only context nodes: short summaries and file references. That lets Android see the project without having the files.
- Only the specific excerpts a given step needs go to the model, like web results do today. A per-project indicator shows what is shared.

### 5.5 Building context from huge, multi-repo projects cost-effectively
Layered, cheapest first:

1. **Free and deterministic (local):** the file tree, manifests (`package.json`, `Cargo.toml`, docker-compose), import graphs, route and API definitions, git history. Parsed with tree-sitter. No model calls, edges tagged "from code".
2. **Cheap model, summaries only:** summarize each module from its manifest, README and public interfaces, not function bodies. Cached by content hash, so only changed files cost money later.
3. **Expensive model, on demand:** deep reads happen only when a plan needs one ("how does auth flow between these two repos?"). The answer is saved back as a context node, so it is paid for once.
4. **Tiers:** the canvas shows repos and relationships first, then modules, then details loaded when opened. Never hundreds of nodes at once.
5. **Incremental re-index:** a watcher re-summarizes only changed files and marks dependent nodes outdated instead of regenerating them.
6. **Cost guard:** show an estimate before the first large index and let the user cap it.

### 5.6 Open-source research
We looked for projects that build code context from a repository using Gemini. Findings (from GitHub, not verified by running them):

| Project | License | Notes |
|---|---|---|
| Graphify | Apache-2.0 | Tree-sitter local parsing, edges tagged `EXTRACTED` or `INFERRED`, LLM only for docs. Pushes a paid cloud product; its popularity figures are unverified. |
| Code-Graph-RAG | MIT | Tree-sitter multi-language monorepo graph, incremental indexing. Requires Memgraph, too heavy for a desktop app. |
| PocketFlow Codebase-to-Tutorial | MIT | Gemini by default. Finds main abstractions, then relationships, then writes explanations. Closest to what we need. |
| DeepWiki-Open | MIT | Wiki generator with RAG, built around hosted repositories. Not a fit for local files. |

**Decision:** do not embed them (they are Python; Vox is Tauri and Rust). Copy the approach: local tree-sitter structure, then the PocketFlow-style pipeline through the rig/Gemini client already in the backend. An installed `graphify` CLI could be an optional sidecar for advanced users later.

### 5.7 Keeping context correct
- **Citations:** every claim links to files and line ranges. A node without a source is labelled an assumption.
- **Trust tags:** "verified from code" versus "inferred", visible in the UI.
- **Drift detection:** when a cited file changes, the node becomes "possibly outdated" and is refreshed or flagged. Stale context is a bigger risk than wrong context.
- **Confirmation:** a short list of "is this right?" prompts for load-bearing nodes only.
- **Plans must cite context** and flag a node when code contradicts it.
- **User edits win** over the model and are marked as such.

---

## 6. Navigation and naming

- Sidebar: **Plans** and **Projects** as sub-items, plus Timeline and **Insights**.
- New plan: vision box, mic button, optional "Attach to project" dropdown (empty by default).
- Project view tabs: **Context** (canvas), **Plans**, **Sources**, **Activity**.
- A plan started outside any project is standalone. Attaching later is allowed. Abandoned plans are hidden.
- Renames: **Spaces to Plans**, **Pulse to Insights**. API paths stay for now.

---

## 7. Platform and engineering notes

- **Apps:** `vox-desktop` (Tauri, React, React Flow, xterm terminal), `vox-android` (Kotlin), `vox-core` (Rust, axum, sqlx, utoipa, Gemini through rig, Exa web search).
- **Performance architecture:** count first, per-day Timeline cache with server revisions, per-chart Pulse results, Spaces summary lists, device persistence in `~/.vox/cache`.
- **Live updates:** `UserEventHub` events reach the desktop as `vox-live-update`. Space events include graph updated, node updated, node stream, message created, plan ready.
- **Conventions:** skip test files and code comments unless asked. Keep plans short and bullet-pointed. Pre-push hook runs clippy with warnings denied, cargo test and a release build.
- **Repos:** `vox-core-merge` and `vox-intro` were removed or merged earlier. The share-to-action branch was removed.

---

## 8. Build order from here

1. Renames (Plans, Insights), standalone plans, `abandoned` state, and the project dropdown on new plan.
2. Projects and the Sources tab, with the local structural skeleton for files and folders.
3. Gemini summaries with citations and outdated detection.
4. Plans reading project context, with context update proposals.
5. Mic input in the vision box (conditional on webview support).
6. Android parity for caching, announcer and branching.
7. Regenerate OpenAPI contracts and types, verify the new SQL on a real database.

## 9. Open questions

- Final names beyond Plans and Insights, including whether the project canvas tab is called Context.
- Whether Insights can be scoped by project.
- Whether any user will want file content uploaded to the server (current answer: no).
- Whether the macOS webview supports speech recognition well enough to ship the mic.
