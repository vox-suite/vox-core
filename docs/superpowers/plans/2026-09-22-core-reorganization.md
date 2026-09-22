# Vox Core Refactor, Reorganization, and Improvement Plan

> **For agentic workers:** Use `superpowers:executing-plans` task by task. Preserve existing changes. This is a planning deliverable; implementation, schema replacement, source publication, and deployment are separate actions.

**Goal:** Give API, Worker, UI, server agents, and desktop execution a shared set of authorized business operations, with simpler consumer identity and storage.

**Architecture:** Two independently runnable services in `services/api/` and `services/worker/`; eight shared responsibility modules under `src/`. Keep one Cargo package with two explicit binary paths initially. Use PostgreSQL for relational state and JSONB for dynamic records, with Redis as optional cache/presence acceleration.

**Tech stack:** Existing Rust 2024, Axum, Tokio, SQLx/PostgreSQL, reqwest, Redis, Rig and Jev integrations. Add OpenAPI generation and a standards-compliant JSON Schema validator as targeted dependencies after checking compatibility with the pinned Rust/toolchain versions.

**Spec:** User's folder/service requirements and preceding Vox architecture discussion. Companion plans: [database simplification](2026-09-22-core-database-simplification.md), [current API inventory](2026-09-22-core-api-inventory.md), and [Bridge reorganization](../../../../vox-bridge/docs/superpowers/plans/2026-09-22-bridge-reorganization.md).

## Global constraints and assumptions

- User-confirmed direction: Vox is a consumer product with direct sign-in and user-owned phone/WhatsApp/device connections. Retire the multi-company embedded platform after its callers and ownership mappings migrate; do not delete it silently during file moves.
- API and Worker have different parent folders and independent process lifecycles; Worker does not call API to run ordinary server-side business operations.
- Export/document every API endpoint, including authentication, failures, pagination, SSE events, and permissions. Export does not mean anonymously expose admin or internal endpoints.
- APIs and agent tools call the same application services. HTTP handlers and agent tools do not execute SQL directly.
- No database access or service-wide secrets on desktop/mobile. A desktop executor receives scoped job leases and returns validated results.
- No new implementation comments; use names, types, tests, and Markdown documentation.
- Preserve Bridge host trust, channel isolation, greetings, speculation, and stream contracts during the structural phase.
- Preserve existing modified `src/agents/prompts.rs`, `src/conversations/service.rs`, and `tests/fixtures/migration_checksums.sha384`.
- Do not change applied migrations/checksums to make tests pass. Database simplification is its own release track.

## Updated scope after user clarification

- User states VAD and speaker verification have been removed for now. No voiceprints, biometric models, enrollment, or speaker switching in this target. Retire dependent fields/calls; keep authentication and channel-possession proof. Earlier inspection notes are historical, not instructions to restore removed features.
- Implement the confirmed 21-table target. Remove spending_policies, operational_quotas, quota_reservations, status_events, connection_grants, service_credentials, auth_challenges, and service_nonces from target SQL. Credentials live in Google Secret Manager; provider verification and shared temporary flow/replay state replace SQL challenge/nonce storage. Keep exact-purchase approval and application-enforced connection capabilities.
- Initial app status uses polling; the status SSE route and durable cursor replay are deferred. Initial desktop jobs perform inference only; they cannot invoke connector operations.
- Replace target projects with collections(kind=project/trip/course/area); retain source project adapters during migration. Tasks and records each have one optional same-user collection_id. Implement collections APIs and tests in Task 3; no generic item or workspace permission system.
- Tasks 7–9 retain existing enforcement while callers migrate. Do not delete grants, quota state, or host authentication out from under a still-enabled feature; disable unsupported automation explicitly at cutover.

## Inspected baseline

Snapshot: HEAD `c099d94`, 2026-09-22. `cargo test --locked --no-run` passed: all test targets compile. Tests were not executed in this planning pass; ignored database/Redis tests need isolated services and some truncate tables. No live database inventory, row counts, deployment state, or real-provider behavior was verified.

- One Rust library currently exports over 30 top-level modules; API/Worker entry points live together in `src/bin/`.
- `src/http/mod.rs` declares **49 method/path operations**; the attached inventory exports all of them.
- Ordinary task/project/record/schema CRUD is implemented primarily through agent tools, not a complete UI API.
- `src/agents/tools/*.rs` has 88 lines matching direct SQL/pool access. Shared application use cases must be extracted before exposing CRUD.
- `conversations/service.rs` is 1,323 lines; `agents/tools/records.rs` is 784 lines; identity and execution modules also combine domain, storage, and orchestration.
- `jobs` and `task_runs` both carry leases; webhook and audit delivery tables carry further lease/retry state.
- Worker claims up to ten jobs under a 30-second lease and executes the batch sequentially. Slow earlier jobs can consume later jobs' lease windows. The loop also uses a hardcoded attempt threshold; inspect stored limits during consolidation.
- API runs migrations during startup and awaits a greeting-name sync before binding the listener. These lifecycle choices should be separated from serving requests.
- Existing Google sign-in in `vox-web/src/lib/auth.ts` is a superuser/admin flow. It is not evidence of an implemented consumer Core login API.
- Migration source yields 54 application tables after historical drops, plus SQLx's migration table. `schema/*.sql` still includes old action representations, so there are competing schema descriptions.

## Target repository

```text
vox-core/
  Cargo.toml
  Cargo.lock
  services/
    api/
      main.rs
      mod.rs
      config.rs
      state.rs
      router.rs
      auth.rs
      openapi.rs
      routes/{identity,tasks,collections,records,schemas,conversations}.rs
      routes/{events,schedules,jobs,devices,connections,executions,status,admin}.rs
    worker/
      main.rs
      mod.rs
      config.rs
      runtime.rs
      health.rs
  src/
    lib.rs
    domain/           entities, IDs, value objects, pure invariants
    application/      use cases, Actor authorization, transaction boundaries
    agents/           reasoning, prompts, Jev, tool wrappers, model routing
    jobs/             schedule evaluation, leases, retries, execution routing
    devices/          registration, capability validation, device protocol
    connectors/       provider adapters, OAuth lifecycle, credential-store ports
    ingestion/        event normalization, classification, enrichment orchestration
    storage/          SQL repositories, migrations, cache implementations
  migrations/         one authoritative migration stream for the active release
  contracts/
    openapi.json
    device-protocol.md
  tests/
  docs/
```

Keep binary names stable while moving their paths:

```toml
[[bin]]
name = "vox-core-api"
path = "services/api/main.rs"

[[bin]]
name = "vox-core-worker"
path = "services/worker/main.rs"
```

Each binary declares its local modules and imports the shared `vox_core` library. Tests for service-local routers live in those modules or exercise the local binary through HTTP; do not reintroduce all API routes into the shared library just to preserve imports. Existing integration tests must migrate deliberately.

Dependencies: `services -> application/jobs/devices/ingestion -> domain + storage/connectors`; agents implement reasoning interfaces and call application operations through thin tool wrappers. Application code must not construct concrete agent runtimes, which would create a tools/application dependency cycle. Put a small reasoning interface next to its use case and inject its implementation at bootstrap. Keep SQLx confined to storage; domain has no Axum/SQLx/LLM imports. Use repository interfaces only where they create a useful test or implementation boundary.

## Current-to-target ownership

| Current modules | Target |
| --- | --- |
| `http`, API bin, API-specific config | `services/api` |
| Worker bin and process loop | `services/worker` |
| `identity`, `host_trust`, `identity_adapters` | `domain/identity`, `application/identity`, `storage/identity`; HTTP validation in API |
| `conversations`, `voiceprint`, `memory` | `application/conversations`, `domain/conversations`, `storage/conversations`, `storage/cache`; remove biometric speaker decisions per updated scope |
| `agents/tools/{tasks,projects,records,profile}` SQL | `application/{tasks,collections,records,schemas,preferences}` plus matching storage repositories |
| `agents`, `jev` | `agents`, including `agents/jev` |
| `jobs`, `db/jobs`, `workers/task_executor`, `durable_tasks`, `schedules` | `jobs` orchestration; `storage/jobs`; `application/tasks`; schedule value objects in domain |
| `events`, event planner dispatch | `ingestion`; planning implementation stays in agents |
| `connections`, `integration_registry`, `capability_grants` | `connectors` adapters/catalog; `application/connections` policy; storage repositories |
| `approvals`, `execution`, `execution_policy`, `outbound` | `application/actions` and `application/executions`; provider effect via connectors |
| `bridge_client`, web search, maps HTTP | `connectors/{bridge,search,maps}`; tool wrappers remain in agents |
| `audit`, `status`, summaries | application transactional event emission, storage append/query, jobs delivery/summarization |
| `agent_registry` | versioned configured catalog for consumer target; temporary compatibility adapter while routes retire |
| `sandbox`, `conformance` | retain meaningful acceptance fixtures under tests; optional sandbox provider under connectors |
| `db`, SQL scattered through modules | `storage` |

## API export and product API design

Maintain the 49 existing operations during extraction. Add `contracts/openapi.json` generated from route DTOs and a controlled documentation endpoint. Generate typed TypeScript/Kotlin/Swift clients as needed from the same specification; do not write separate hand-maintained contracts per app. A route/spec coverage test must compare method/path sets and fail on undocumented additions or dangling definitions.

Partition routers and OpenAPI security schemes into user-session, trusted-Bridge/service, device-lease, and admin access. All can be documented in one artifact with tags and explicit security. Host registration, credential rotation, Redis, and audit administration must not acquire consumer permissions. OpenAPI documents HTTP streaming plus event schemas where streaming remains supported; `device-protocol.md` specifies heartbeat, lease, cancellation, reconnect, and message ordering.

### Proposed additions (not currently implemented)

| Capability | Proposed routes | Authorization/result |
| --- | --- | --- |
| Sign-in/session | `POST /v1/auth/exchange`, `/refresh`, `/logout`; `GET /v1/me` | Verify provider proof; issue/revoke own session; no raw user ID trusted |
| Channels | `GET /v1/me/channels`; `POST /v1/me/channels/link-challenges`; `POST /v1/me/channels/verify`; `DELETE /v1/me/channels/{id}` | Current user + possession proof; removal audit |
| Preferences | `GET/PATCH /v1/me/preferences` | User; optimistic version |
| Tasks | `GET/POST /v1/tasks`; `GET/PATCH/DELETE /v1/tasks/{id}`; `POST /v1/tasks/{id}/run` | User owns task; explicit run separate from CRUD |
| Collections | `GET/POST /v1/collections`; `GET/PATCH/DELETE /v1/collections/{id}` | Generalized projects/trips/areas; same-owner child references |
| Schemas | `GET/POST /v1/schemas`; `GET /v1/schemas/{id}`; `POST /v1/schemas/{id}/versions` | Read globals, author own immutable versions |
| Records | `GET/POST /v1/records`; `GET/PATCH/DELETE /v1/records/{id}` | Validate schema and owner; cursor/filter allowlist |
| Schedules | `GET/POST /v1/schedules`; `GET/PATCH/DELETE /v1/schedules/{id}` | Add session-based API without exposing legacy service token |
| Jobs | `GET /v1/jobs/{id}`; `POST /v1/jobs/{id}/cancel` | User-visible status, bounded diagnostics |
| Devices | `GET/POST /v1/devices`; `DELETE /v1/devices/{id}`; `POST /v1/devices/{id}/heartbeat` | User enrollment; device credential thereafter |
| Local work | `POST /v1/devices/{id}/jobs/claim`; `POST /v1/device-jobs/{id}/heartbeat`; `/result`; `/fail` | Device + scoped attempt/lease token; idempotent acceptance |
| Ingestion | `POST /v1/events/batch` | Device/user principal derived server-side; durable receipt |
| Connectors | `GET /v1/connectors`; `GET /v1/connections`; `POST /v1/connections/{provider}/authorize`; `GET /v1/connections/{provider}/callback`; `DELETE /v1/connections/{id}` | Server-issued OAuth state bound to user/session; callback proof |
| Actions/executions | `GET /v1/action-proposals/{id}`; `POST /v1/action-proposals/{id}/approve`; `GET /v1/executions/{id}` | Approval is tied to immutable details and user |
| App updates | Poll existing task/job/execution GET routes | Current resource status/version; no durable event-feed API |

Use a common error object `{code,message,request_id,details}`. Lists use bounded cursor pagination. Mutations that can be retried accept a user-scoped idempotency key and reject the same key with a different payload. PATCH uses `If-Match`/version and returns conflict instead of overwriting newer updates. DELETE is governed by entity lifecycle: an active execution is canceled/reconciled, not erased with a task row. Async work returns 202 with durable job ID. Synchronous CRUD must not call an LLM.

## Task 1: Lock baseline, contracts, and migration ownership

**Files:** existing tests, current API inventory, `docs/architecture.md`, `docs/contracts/core-bridge.md`.

- [ ] Record current worktree and preserve modified files. Run the compile check, then safe unit/contract suites; list ignored integration tests separately.
- [ ] Inventory environment variables by process and owner without printing values. Record Bridge's signed host assertion protocol and missing outbound result contract.
- [ ] Record the confirmed consumer direction and map affected platform callers before removing any platform feature. Retain legacy routes/storage during structural work.
- [ ] Compare migration-derived catalog with the supplied table list. Inspect actual target catalog/constraints only through an authorized read-only connection; no reset commands here.

```sh
cargo test --locked --no-run
cargo test --locked --lib
cargo test --locked --test process_boundaries --test migration_checksums
```

**Gate:** failures classified as pre-existing/new; no historical migrations rewritten.

## Task 2: Separate the two service roots

**Files:** `Cargo.toml`, `src/bin/*`, `src/http/*`, `services/api/*`, `services/worker/*`, `src/lib.rs`, `Dockerfile`, process/release tests.

- [ ] Move entry points to explicit Cargo binary paths above. Move HTTP handlers/state/router/config into API; worker startup/loop into Worker.
- [ ] Keep initial handlers and business calls unchanged. Adjust affected integration tests so both binaries remain independently testable.
- [ ] Split config so API startup does not require worker-only provider credentials and Worker does not need API bind/admin settings.
- [ ] Add graceful stop and real process health. Worker failures must produce failure exit/health signals rather than `let _ = worker.run(...)` swallowing the result.
- [ ] Plan migrations as one release/init command (not a third long-running service); API/Worker check schema compatibility. Move greeting refresh to Worker; API opening remains cache-only with a fallback. Listener startup must not await a full DB-to-cache scan.
- [ ] Update Docker COPY paths and deploy commands while preserving binary names. Test `cargo build --locked --bin vox-core-api --bin vox-core-worker` and process boundary tests.

**Gate:** API runs without Worker; Worker operates on shared storage without HTTP calls to API.

## Task 3: Extract shared task/project/preference operations

**Files:** `src/agents/tools/{tasks,projects,profile,dependencies}.rs`, new `domain`, `application`, `storage` modules; task/project API routes.

**Interfaces:** define `Actor { user_id, principal_id, principal_kind, grants }` from verified authentication; no public constructor from request JSON. Define `TaskService::{create,list,get,update,delete,request_run}` and CollectionService::{create,list,get,update,archive} plus preference operations using typed request/result structs shared by adapters.

- [ ] Extract existing SQL into repositories without changing tables. Move validation, authorization, and transactions into the application service.
- [ ] Make tool wrappers deserialize arguments, call the same service used by UI endpoints, and serialize results. No tool gets a raw DB pool.
- [ ] Test UI and tool operations against the same application fixture: create, list, update conflict, foreign-user access denial, foreign-user project link denial, delete lifecycle, and duplicate request key.
- [ ] Add task/project/preferences routes and documented DTOs. Derive owner from Actor, never a request `user_id`.
- [ ] Add a source-boundary check for SQL outside storage and gradually reduce its allowlist as remaining modules migrate.

**Gate:** tasks can be managed from UI without an agent and cannot bypass application authorization through a tool.

## Task 4: Extract schema/record operations and ingestion

**Files:** `agents/tools/records.rs`, `jev/schema_classifier.rs`, `events/*`, new `application/{schemas,records}`, `storage/{schemas,records,events}`, `ingestion/*`.

- [ ] Define `SchemaService::{create_version,get,list}` and `RecordService::{create,update,get,list,delete}`. Preserve explicit schema failures; user schemas outrank global defaults for name lookup.
- [ ] Replace ad hoc validation with a chosen JSON Schema draft implementation; immutable version reference on records, size/depth/time budgets, no remote `$ref` fetching. Test nested objects, arrays, required fields, unknown schema, and foreign-owner schema.
- [ ] Expose ordinary record/schema APIs. LLM-generated schema JSON is untrusted input, never executable DDL or SQL.
- [ ] Move device input normalization to ingestion. Insert event and processing job in one transaction; deduplicate by user/source/event ID and compare payload hashes.
- [ ] Commit derived records and event processed state transactionally; external model work occurs outside DB transactions and uses a claim/version fence on commit. Retry cannot create duplicate facts or plans.
- [ ] Add device batch receipt API with bounded payloads, timestamps/provenance, and per-event result semantics.

**Gate:** duplicate ingestion creates one logical record; direct API and agent validation behave identically.

## Task 5: Consolidate the execution scheduler

**Files:** `jobs`, `db/jobs`, `durable_tasks`, `schedules`, `workers/task_executor`, new `storage/jobs`, worker runtime.

- [ ] Keep user tasks distinct from execution jobs and schedules. Migrate overlapping leases to the database design's single jobs/job_attempts scheduler; preserve existing task status adapters during transition.
- [ ] Claim only available concurrency slots. Each lease carries a monotonically changing generation/attempt token; heartbeat/complete/fail must match it. Process completion after lease expiry cannot overwrite a newer result.
- [ ] Implement retry/backoff using persisted max attempts, not a hardcoded five. Bound concurrency by job type and provider budget. Do not hold DB locks during model/provider calls.
- [ ] Test two concurrent claimers, slow first job, heartbeat, expired lease, stale result, cancellation, restart, exhausted retries, and DST schedule boundaries.
- [ ] Use unique `(schedule_id, scheduled_for)` for recurring occurrences. A scheduler crash between enqueue and advancing next run cannot duplicate an occurrence.
- [ ] Keep execution attempts for external side effects separate from compute attempts. Unknown provider outcomes enter reconciliation rather than blind retry.

**Gate:** one scheduler owns leases, and lease loss cannot produce an accepted stale result.

## Task 6: Add devices and local inference dispatch

**Files:** `devices/*`, `jobs/routing.rs`, `application/devices`, `storage/devices`, API device routes and device protocol.

- [ ] Record user-owned device identity, public key/credential reference, capabilities/model versions, consent, last seen, and revocation. Presence is ephemeral; enrollment/revocation is durable.
- [ ] Worker chooses eligible local compute using capabilities, deadline, load, power preference, and privacy policy; API delivers/polls durable assignments. Connections stay outbound from desktop.
- [ ] Issue job/attempt-scoped short-lived capabilities. Start with summarize/classify/rank operations returning structured proposals; do not grant unrestricted tool access or connector tokens.
- [ ] Validate output schema and source versions server-side. Task reprioritization applies through TaskService; intervening edits cause recomputation/conflict, not overwrite.
- [ ] Add deadline-aware cloud fallback with one fenced acceptance point. Local-only policy prohibits cloud fallback. Late local results and repeated submissions are harmless.
- [ ] Test reconnect, sleeping desktop, revoked device, user mismatch, malicious result, timeout fallback, and both runners finishing simultaneously.

**Gate:** delegated work uses the same durable job model and application rules as server execution.

## Task 7: Extract connectors and preserve side-effect safeguards

**Files:** `connections`, `integration_registry`, `capability_grants`, `approvals`, `execution_policy`, `execution`, `outbound`, `bridge_client`, tools for external APIs.

- [ ] Separate versioned code/config connector declarations from per-user account connections; enforce allowed_capabilities on each connection without a connection_grants table. Each declaration states supported operations; OAuth availability does not prove ordering support.
- [ ] Extract credential custody behind a Google Secret Manager-backed secret-store interface. Database stores secret references; refresh/revoke is serialized per connection. Desktop never receives server connector credentials.
- [ ] Keep immutable proposals, explicit per-purchase approvals, executions, attempts, and reconciliation. Persistent spending policies and quota reservations are removed from this target. API and agents enter through the same ActionService/ExecutionService.
- [ ] Replace registry FKs with validated stable configuration keys only at database cutover; preserve historical definition/model versions on executions and audits.
- [ ] Define the Bridge outbound request/result authorization and callback contract jointly; current Bridge callback `/v1/actions/{id}/result` has no Core route. Implement compatibility or a coordinated replacement with idempotent terminal updates.
- [ ] Test revoked connection capabilities, changed proposal after approval, consumed approval replay, connection capability denial, ambiguous purchase/call timeout, duplicate callback, and mismatched callback provider account.

**Gate:** simplifying tables/configuration does not remove approval, deduplication, or unknown-outcome reconciliation.

## Task 8: Simplify identities and cut over storage

**Files:** identity/host trust adapters, `application/identity`, `storage/identity`, API auth routes, schema/migration/tests specified in database plan.

- [ ] Implement the companion database plan in its own change track. Map authenticated subjects to `users.id`; separate channel possession, login authentication, and request authority.
- [ ] Keep Bridge's current protocol working with a compatibility resolver while migrating from user_context ownership; revoke retired credentials after deployment, not before.
- [ ] Register/check Google issuer, audience, expiry, nonce/flow binding and signature through a maintained OIDC/auth library. Use provider subject, not email, as account key. Web/mobile/desktop sign-in must use appropriate registered redirect/PKCE flows.
- [ ] Retain admin access as a distinct role/policy. Consumer sign-in cannot inherit the current superuser session mechanism.
- [ ] Execute mapping, constraint, concurrency, tenant-escape, and migration checks before retiring old tables/routes. No database reset is part of the code-folder move.

**Gate:** all requests have a verified Actor and one canonical data owner, without auto-linking accounts based on matching phone/email.

## Task 9: Export every endpoint and migrate consumers

**Files:** `services/api/openapi.rs`, `contracts/openapi.json`, `contracts/device-protocol.md`, route DTOs, `tests/api_contract.rs`, docs.

- [ ] Generate the OpenAPI artifact from actual request/response types and routes. Include all 49 baseline operations or explicit versioned retirements; add planned routes only when implemented.
- [ ] Define all security schemes and errors, streaming media types, SSE event names, idempotency keys, version headers, maximum list/batch sizes, and callback auth.
- [ ] Add a method/path coverage test and a deterministic export check that fails on uncommitted contract changes. Export through a CLI mode such as `vox-core-api --export-openapi contracts/openapi.json` that does not connect to DB/providers.
- [ ] Generate consumer SDKs; test Web/Desktop/Bridge compatibility. User-facing CRUD routes need normal session credentials, not a shared service token.
- [ ] Keep legacy POST reads available while GET replacements migrate. Publish deprecation mapping before removing platform registry endpoints.

**Gate:** each implemented endpoint has a contract and matching access-control tests; admin/internal endpoints remain protected.

## Task 10: Cleanup, observability, and release checks

**Files:** `README.md`, architecture/docs, packaging, deployment scripts, tests and retired modules.

- [ ] Split remaining conversation service into opening, turn orchestration, speculation and completion use cases. Preserve Redis-only greeting, read-only speculation revisions, and final-turn generation.
- [ ] Remove obsolete modules, SQL files, registry handlers, and dependencies only after consumers migrate. Generate schema documentation from migrations rather than maintaining parallel handwritten schemas.
- [ ] Use request/job/attempt correlation and stage timing without secrets, raw SMS, transcripts, or voiceprint logs. Separate API health from Worker progress and provider health.
- [ ] Validate all non-network tests, isolated PostgreSQL/Redis suites, two-worker concurrency tests, API contract coverage, release binaries, and deployment path changes.

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --release --locked --bin vox-core-api --bin vox-core-worker
```

Run ignored tests only in explicitly provisioned disposable databases, using the companion DB plan. Source publication, deployment, and authenticated/live workflow tests are separate gates. Stop old consumers/workers before incompatible schema cutover; retain the appropriate release artifact for the chosen migration mode.

## Review batches and completion

1. Service folders and baseline, with existing tables/contracts retained.
2. Shared use cases, ordinary CRUD, record/schema validation.
3. Durable jobs and local execution protocol.
4. Connector/action correctness and Bridge contract reconciliation.
5. Consumer identity and database cutover, with separate validation.
6. Complete API export, consumer migration, and retirement of compatibility code.

The structural refactor is complete when both processes use the eight shared modules with no duplicated business rules. Product improvements are complete only when ordinary UI actions, agent actions, local jobs, and connector effects pass their corresponding end-to-end tests. A lower table count is not itself a correctness criterion.
