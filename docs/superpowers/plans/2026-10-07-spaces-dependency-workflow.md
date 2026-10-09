# Spaces Dependency Workflow Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans for inline execution. User authorized implementation in this chat.

**Goal:** Make a vision immediately become a live dependency graph of parallel agents with straight hierarchical connectors and bottom-center node-aware chat.
**Architecture:** Persist tasks beside nodes. Claim ready work with durable leases. Role-scoped workers produce structured outputs and validated follow-ups. Render rank-based columns and contextual chat.
**Tech Stack:** Rust, PostgreSQL/sqlx, rig/Gemini, React/TypeScript, React Flow.
**Spec:** ../specs/2026-10-07-spaces-dependency-workflow-design.md

## Global Constraints
- Preserve existing records, existing commit semantics, and unrelated working tree changes.
- Default per-space parallel cap three. Bound work by existing step/branch limits.
- App/blur/panel layering remains 10/20/30. External action authorization is unchanged.
- Persistence precedes published realtime events; dependencies must be same-Space and cycle-free.

## Review Focus
- Concurrent edge insertion cannot introduce cycles.
- Failed or cancelled inputs cannot unlock a join.
- New nodes and replies cannot reset canvas pan/zoom.
- Stale worker completion cannot overwrite current generation.
- Legacy graphs remain readable and are never replayed automatically.

## Tasks
- [ ] 1. Add dependency-layout tests, implement ordered columns, replace radial cards/orbits with workflow cards and straight edges; verify forks/joins/cycles.
- [ ] 2. Add durable task migration and transactional task operations: atomic creation, validated dependencies, claims, leases, completion, blocking, cancellation, retry; unit/integration checks.
- [ ] 3. Add role-specific worker and orchestrator; parallel ready tasks, structured follow-ups, bounded retries, generation guards, recovery; deterministic scheduler tests.
- [ ] 4. Save vision before architect, add workflow routes/events, context-aware chat, stop/retry, update client types; API authorization checks.
- [ ] 5. Replace side chat dock with bottom-center composer and expandable thread; preserve selection/details/commit and expose execution state.
- [ ] 6. Build/lint/format/test, database tests if local DB available, visual fixtures with clean console, final review and evidence report.

## Execution ledger
- Plan approved for inline execution by user instruction: “go ahead with the impl”.
- Ruling: additive task metadata uses node.data.execution to preserve existing API shapes and Android compatibility; durable task table owns scheduling truth.

## Implementation and local verification

Implemented desktop dependency columns with square cards and straight connectors, a bottom-center contextual composer with expandable messages, cycle warnings, and stop/retry controls. New spaces persist their vision and durable job before returning. Core now schedules scoped workers concurrently (three per Space, twelve per runtime), waits for prerequisites, records evidence and compact results, expands persisted outputs into follow-up DAG nodes, and accepts durable node-targeted chat requests. Generation, lease, version and commit guards protect cancellations and edits. Legacy Spaces retain the earlier runtime.

Verified locally with desktop production build, targeted ESLint, three dependency-layout tests, browser fixture inspection and selected-node/orchestrator input checks. Core library/API/worker compile and an isolated PostgreSQL integration test passed for parallel claims, joins, deduplication, cross-space rejection, cycles, cancellation, retry, expired leases, and stale results. The new migration was applied only to the disposable verification database. Provider calls and deployed API/client behavior have not been validated; deployment must apply `20261007000002_space_tasks.sql` before activating this runtime.
