# Spaces: vision-led dependency workflow

Status: proposed design for review. No runtime or UI implementation is included in this document.

## Outcome and scope

A submitted vision immediately opens a Space containing its vision node. The Space grows into a real execution graph: independent research workers run concurrently, further searches depend on earlier findings, and synthesis waits for all required inputs. The desktop canvas uses ordered left-to-right columns, compact dark cards, and straight connecting edges inspired by the supplied reference. Android rendering is outside this implementation scope; shared API additions must remain compatible with it.

Example: Vision → Web search A and User data B concurrently. Search A produces a follow-up search C. C and B become the dependencies of synthesis D. D can produce a plan or spawn another task. The graph represents execution and evidence, not decorative relationships.

## Current constraints

Creation currently waits for the architect before saving the Space. Runtime execution is a single agent loop protected by an in-process per-space lock. spawn_branch creates node/edge records without launching a worker. Node states are running/done/stale/rejected. Node data, provenance, derived_from, version, and graph edges already exist. The desktop uses a radial layout. These behaviors require implementation changes; prompt instructions alone cannot satisfy the workflow.

## Creation and orchestration

Persist the Space and its single vision node transactionally before invoking a model. Use the trimmed vision as an initial title until the architect supplies one. Return the Space immediately and select/open it in the desktop. Publish persisted events after commit. The vision node is completed input, never a fabricated research result.

Run the architect asynchronously to generate the initial task set from available schemas and intent. Initial tasks reference the vision node. A scheduler starts all ready tasks subject to a global semaphore and a per-space cap (default three), and existing configured step/branch limits. Expose the actual agent role on each task: web_search, user_data, synthesis, or plan. Do not automatically spawn irrelevant research types merely to fill columns.

Each worker receives its task brief, the vision, and the completed output snapshots of all prerequisites. A web worker uses web tools; a user-data worker uses authorized Core-owned queries; a synthesis worker consumes supplied evidence. Workers return structured findings, citations, and suggested next tasks. An orchestrator validates and persists follow-up tasks and all their dependency edges in one transaction. A follow-up is runnable only after its complete dependency set exists.

Use stable task deduplication keys derived from role, brief, dependency IDs, and dependency versions. Model output cannot bypass ownership, limits, cycle checks, or tool access policy. Existing commit behavior continues to create planned spans; booking/purchase/external writes remain separate authorized workflows.

## Durable task model

Add a space_tasks table keyed to an existing space node. Fields: space_id, node_id, role, brief, execution_status, attempt, max_attempts, dedupe_key, run_generation, lease_owner, lease_expires_at, input_versions JSON, output JSON, error, started_at, finished_at, created_at, updated_at. Unique node_id and (space_id, dedupe_key, run_generation).

Task status is separate from the legacy NodeState: queued, running, done, failed, blocked, cancelled. Add optional execution metadata to graph responses; older nodes with no task remain ordinary saved nodes. Keep existing NodeState values for older clients and expose richer state through optional task metadata. Legacy completed graphs must not be automatically executed again.

space_edges is the canonical prerequisite relation for executable nodes. derived_from is a compatibility mirror written in the same transaction, not a second scheduler input. A prerequisite must be in the same Space and owned by the same user. Reject self-edges, duplicate tasks, cycles, and dependency changes to a running task. Lock the Space while validating/inserting a dependency batch so concurrent inserts cannot each pass an obsolete cycle check.

## Scheduling, failures, and lifecycle

Claim queued ready tasks atomically in PostgreSQL, with row locks and leases; process-local locks are insufficient across replicas. Ready means every prerequisite is done, including the vision input. Claims carry the current run generation and prerequisite versions. Heartbeat active leases; recover expired leases on startup and periodically. Bound retries to two attempts for transient failures; validation/policy errors fail without retry. Do not repeat completed external reads unnecessarily on unrelated page reloads.

On completion, persist task output, evidence, node state, and follow-up proposals before publishing a realtime event. Conditional updates on the lease and run generation prevent late workers from overwriting cancelled or restarted runs. Failed prerequisites block descendants rather than allowing invented synthesis. Provide retry for a failed task and its blocked descendants and stop for the Space. Stopping cancels queued work, invalidates leases, and cancels active futures. Generation checks remain necessary when a provider request cannot be cancelled.

Changes to a vision or completed finding mark affected descendants stale. Rerunning starts a new generation with updated dependency versions. Removal during active execution is rejected; stop the run first. Graph edits and chat instructions route through the same validated scheduler operations.

A Space is planned only when its completion criterion is met and no runnable/running tasks remain. Exhausted limits or unresolved failures are visible on the graph and cannot be reported as successful completion. Preserve actual partial findings.

## Desktop design contract

Use the existing shared PageContainer/PageHeader/PageBody. Canvas fills the available body, with a faint dot grid, quiet navigation, and no radial rings or orbit decoration. Reuse the app palette and typography. Cards have fixed 260px widths, compact type/status headers, a short task brief, and a bounded findings preview. Expanded evidence/details appear on selection. Show queued/running/done/failed/blocked/cancelled distinctly without relying on color alone.

Vision occupies column zero. Each other node has rank = one plus the maximum rank of its prerequisites. Independent tasks share a column and stack vertically. Synthesis appears strictly to the right of its latest input, and has an incoming connection for every required input. Use deterministic row ordering with sibling grouping and crossing minimization; preserve existing nodes' order as new nodes arrive. Reserve height by card type so long output cannot overlap cards. Manual dragging cannot alter dependency rank.

Use React Flow straight edges with left target/right source handles and small arrowheads. No curved connectors. Keep edge endpoints on card boundaries, and keep connecting paths outside unrelated card bounds through row allocation. Selected nodes emphasize their immediate prerequisites and dependents. Cyclic legacy graphs display a visible compatibility warning with a deterministic fallback; do not freeze the renderer or silently schedule them.

Auto-fit once on first load, never on every update. New nodes appear without stealing focus or resetting pan/zoom. Retain zoom/fit controls, selection, the bottom-center chat composer, existing detail editing, and commit controls. Creating a Space opens it as soon as the persisted vision is available, before research finishes.

## API and compatibility

Preserve existing creation/graph/chat response shapes, adding optional task execution metadata and events. Add owner-authorized stop and task-retry routes. Validate writes at the repository/service boundary for both tool-originated and HTTP mutations. Generate updated API schemas and desktop types; verify Android accepts additive fields. Existing Spaces are readable; new workflows use an explicit agent_spec workflow_version to select the new scheduler.

## Validation and completion evidence

Unit tests: rank layout for forks, chains, joins, disconnected nodes, and cycles; readiness after all prerequisites; task bounds; deduplication; generation/version invalidation; retry classification.

Database integration tests: atomic vision creation; same-Space ownership; cycle rejection under concurrent mutations; task claims across two schedulers; expired-lease recovery; stop racing completion; atomic follow-up dependency insertion.

Deterministic fake workers: start web and user-data workers concurrently; delay user data; web creates a dependent second search; ensure synthesis starts only after second search and user data complete. Test failure, restart, and bounded fan-out.

Desktop fixture: initial vision alone; concurrent tasks; follow-up chain; two-input synthesis; failures; long text; large graph. Check actual straight SVG paths, rank ordering, no card overlap, readable 1280px/1920px layouts, preserved pan/zoom, keyboard selection, and clean console. Production build, lint, Rust formatting/checks, and relevant tests must pass.

Local fixtures and tests do not prove deployed provider execution. Final reporting must distinguish implementation, local checks, migrations, deployment, and live data validation. Deployments and live runs are subsequent concrete steps, not claims made from unit tests.

## Implementation boundaries

Core: domain task types; migration; SpaceRepository transactional graph/task operations; asynchronous creation; architect structured task output; role-specific workers; durable scheduler; stop/retry API; realtime events and schemas.

Desktop: dependency-layout helper and tests; workflow card component; straight edge configuration; creation selection; additive execution states and retry/stop actions; browser fixtures.

Avoid unrelated edits currently present in desktop and Android working trees. Preserve older Space records and current commit semantics.

## Approved chat refinement

A bottom-center floating composer replaces the side chat dock. Selecting a node targets its agent and names it above the input; otherwise messages target the Space orchestrator. A compact expandable thread shows replies. Requests for new work spawn connected tasks; corrections invalidate descendants. Node context is sent as a validated node_id, never inferred only from message text.
