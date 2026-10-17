# Vox user-flow audit — 17 October 2026

What this repo does: Vox accepts phone calls and desktop/Android voice sessions, connects external accounts, turns observations into Timeline entries and Pulse insights, and maintains Projects and planning Spaces. This review assumes concurrent users, retries, reconnects, and multiple backend processes.

Three parallel review agents covered voice, connectors/Timeline/Pulse, and Projects/Plans. The primary reviewer checked shared configuration, workers, scheduled calls, and consolidated evidence. Application code was not changed.

## Evidence and scope

- Reviewed the current local working trees, including substantial existing uncommitted changes. These are not claims about production deployments.
- HEAD snapshots: Core `425536f`, Bridge `ab804cd`, Desktop `945cf8e`, Android `4f48681`, Connections `ffd1920`.
- Core builds Connections at `a92136fc86cca60f5a00c31a5e3ced0865efd63a`, not the sibling checkout's HEAD. The reviewed Gmail provider file has no diff against that pinned revision.
- Deep source tracing covered the paths below; this was not a line-by-line audit of every package or every connector.
- “User plans” was interpreted as the Plans/Spaces planning workflow, including commit into planned spans. Subscription/billing plans were not identified in the inspected product paths.
- P1 means a correctness/data-loss issue to address first. P2 means a material failure under a particular interaction, outage, or concurrency condition. Findings are source-verified unless a runtime check is explicitly stated.

## Flow map

| User flow | Main path | Persistence and completion boundary |
|---|---|---|
| Phone call | Signed Twilio webhook → accepted-call state → signed stream → Bridge shared voice session → STT → Core → TTS/playback marks | Core conversation; Bridge transport and response tasks must both end |
| Desktop/Android voice | Authenticated Core bootstrap → single-use ticket → Bridge redemption → shared voice session | Core binds conversation; each client owns microphone, playback, and interruption |
| Add connector | Authenticated connector catalog → OAuth/state/PKCE or provider linking → encrypted credentials → scheduled sync | Connection authorization/generation and provider checkpoints |
| Timeline → insights | Provider observations or inbound event → ingestion/extraction → Timeline → SQL revision/invalidation → Pulse aggregation/cache | Stable event identity, evidence and committed revisions |
| Projects | Client JSON → authenticated CRUD/chat → validation → revision-checked save | Full project JSON is replaced on save; optimistic revision protects concurrent writes |
| Plans | Space/root/job → routing → research/synthesis tasks → final selection → explicit commit | Leases, generations, prerequisites and terminal-state checks |

## Must fix

### 1. P1 — Android saves erase desktop diagram metadata

**What this is:** Both clients save the same project JSON. Desktop stores diagram/flow information inside project messages.

**Problem:** [ProjectsModels.kt:48](https://github.com/vox-suite/vox-android/blob/4f48681/app/src/main/java/in/voxagent/mobile/projects/ProjectsModels.kt#L48) omits `flow` and `diagram_id`; the Android decoder ignores unknown fields. [ProjectsApi.kt:42](https://github.com/vox-suite/vox-android/blob/4f48681/app/src/main/java/in/voxagent/mobile/projects/ProjectsApi.kt#L42) serializes this reduced model, and [projects.rs:87](https://github.com/vox-suite/vox-core/blob/425536f/src/storage/projects.rs#L87) replaces the stored JSON. Opening a desktop-created project and saving settings/context or proposal changes on Android loses those fields even with a valid revision.

**Fix:** Complete the shared serialization contract, preserve unknown fields, or use scoped mutation endpoints. A desktop-shaped fixture must survive an Android decode/edit/encode round trip.

**If skipped:** Ordinary cross-device edits permanently remove diagram associations and flow metadata.

### 2. Corrected after reproduction — Plan node SQL bind lists are fragile

**What this is:** Editing a node creates a version and rewires dependent tasks/edges transactionally.

**Original assessment:** The mixed SQL loop binds three parameters even for a two-placeholder statement. The review initially claimed this would always return 500. **That claim was incorrect:** the regression test passed on the original code against isolated PostgreSQL 17 and the current SQLx version. This is a maintainability/hardening issue, not a reproduced failure.

**Fix:** Give each statement its correct bind list. Add a database regression covering successful title/body edit, current version, edges and dependent tasks.

**If skipped:** Heterogeneous SQL bind construction remains harder to review and safely extend. The successful original-code regression is retained to check versioning and rewiring during simplification.

### 3. P1 — Voice response tasks can outlive the failed call

**What this is:** Bridge starts a response task while speech recognition connects.

**Problem:** [session/mod.rs:139](https://github.com/vox-suite/vox-bridge/blob/ab804cd/src/voice/session/mod.rs#L139) spawns the response; initial STT failure returns at line 164, and reconnect failure returns at line 352. The response is aborted only on normal loop exit at line 438. Dropping a Tokio task handle detaches its task; aborting the parent session has the same ownership gap.

**Fix:** Use an abort-on-drop response owner and explicitly await cancellation during normal shutdown. Test initial connection failure, reconnect failure and parent cancellation with a pending fake agent.

**If skipped:** Core generation, tool processing or synthesis may continue after the user’s call has failed or ended.

### 4. P1 — Successful event processing removes retry deduplication

**What this is:** Ingestion uses a stable external event ID to avoid accepting the same delivery twice.

**Problem:** [handler.rs:511](https://github.com/vox-suite/vox-core/blob/425536f/src/events/handler.rs#L511) deletes the inbound row after processing. A later retry can insert the same external ID again through [service.rs:81](https://github.com/vox-suite/vox-core/blob/425536f/src/events/service.rs#L81), but Timeline deduplication uses the newly generated inbound UUID at [handler.rs:461](https://github.com/vox-suite/vox-core/blob/425536f/src/events/handler.rs#L461).

**Fix:** Retain a minimal processed-identity tombstone or derive downstream deduplication from stable source/external identity. Test ingest → process → replay → exactly one Timeline entry.

**If skipped:** Delayed retries duplicate nonfinancial Timeline entries and inflate derived insight counts. Some financial normalization paths provide separate mitigation; this is not a claim that every financial event duplicates.

### 5. P1 — Batch event ingestion acknowledges work that is never queued

**What this is:** `/v1/events/batch` accepts user/device observations.

**Problem:** [events.rs:68](https://github.com/vox-suite/vox-core/blob/425536f/services/api/routes/events.rs#L68) inserts inbound rows, then commits and reports success without scheduling processing. The normal service inserts `process_event` jobs atomically; the failed-event sweeper only recovers events already marked failed. No enqueue trigger was found. No current desktop/Android call site was found for this endpoint, so the impact is specifically callers that use it.

**Fix:** Reuse the normal ingestion service or enqueue processing transactionally for newly inserted rows. Test HTTP ingestion followed by worker processing.

**If skipped:** The API reports inserted events that never reach Timeline or Pulse.

### 6. P1 — Planner failure and an explicit no-action decision both become a phone call

**What this is:** Scheduled tasks ask the planner whether an outbound call is useful.

**Problem:** [handler.rs:123](https://github.com/vox-suite/vox-core/blob/425536f/src/schedules/handler.rs#L123) converts planner errors into an empty action list. [handler.rs:172](https://github.com/vox-suite/vox-core/blob/425536f/src/schedules/handler.rs#L172) turns that empty list into a reminder call. The planner contract explicitly says `actions: []` means no useful action. Both provider failure and a valid no-action decision can therefore trigger the same external action when the earlier optional Jev gate does not skip processing.

**Fix:** Keep `Err`, successful no-op, and explicitly configured reminder delivery separate. Retry provider failures; honor a valid no-op. Test all three outcomes.

**If skipped:** Users receive calls the planner rejected or never successfully evaluated.

### 7. P1 — Missing Bridge configuration can complete a reminder without placing a call

**What this is:** The worker always installs an `OutboundCallService`, whose internal Bridge client is optional.

**Problem:** [runtime.rs:207](https://github.com/vox-suite/vox-core/blob/425536f/services/worker/runtime.rs#L207) silently drops Bridge client construction failure. [outbound/mod.rs:170](https://github.com/vox-suite/vox-core/blob/425536f/src/outbound/mod.rs#L170) then returns success without a provider call ID. The schedule handler treats this as dispatched and marks the related span done at [handler.rs:203](https://github.com/vox-suite/vox-core/blob/425536f/src/schedules/handler.rs#L203).

**Fix:** Return an explicit unavailable/blocked outcome when dispatch is not configured. Mark dispatch only after provider acceptance and distinguish dispatch acceptance from call completion.

**If skipped:** A missing or invalid `VOX_BRIDGE_URL` silently loses scheduled delivery while showing progress/completion.

## Should fix

### 8. P2 — Stopped Plans can still change final selection and publish replies

**What this is:** Stop increments the workflow generation and marks pending requests processed.

**Problem:** A router request already awaiting the LLM at [space_workflow.rs:319](https://github.com/vox-suite/vox-core/blob/425536f/src/agents/space_workflow.rs#L319) continues afterward. The direct `finalize` writes at line 375 and assistant-message insert at line 409 do not recheck generation, processed status or terminal Space state. Task spawning has a generation guard, but these writes bypass it.

**Fix:** Apply each decision in a transaction that locks/rechecks the Space and request. Fence assistant replies too. Test stop/commit while routing is paused, then release the router and assert no writes.

**If skipped:** A successful stop can be followed by stale selections or assistant activity. Later recalculation may overwrite some selection changes; post-stop writes are still possible.

### 9. P2 — Android project responses can undo navigation

**What this is:** Project chat/save runs asynchronously while the back button remains available.

**Problem:** [ProjectsViewModel.kt:107](https://github.com/vox-suite/vox-android/blob/4f48681/app/src/main/java/in/voxagent/mobile/projects/ProjectsViewModel.kt#L107) changes selection, but reload returns early while busy at line 67. A pending chat then unconditionally restores its old project at line 305. Back or selection of another project can be overwritten by the old response.

**Fix:** Fence completions using the captured selection/generation and reload the destination afterward, or consistently disable navigation during mutation. Test with a deferred response.

**If skipped:** The user returns unexpectedly to an old project or sees a response under the wrong current selection.

### 10. P2 — Twilio writes can block interruption and teardown indefinitely

**What this is:** Twilio transport processes incoming controls and outgoing playback in one select loop.

**Problem:** [stream.rs:203](https://github.com/vox-suite/vox-bridge/blob/ab804cd/src/channels/twilio/stream.rs#L203), line 213 and line 224 await socket sends without a deadline. A non-reading peer suspends the selected branch, preventing the loop from handling stop, completion and cleanup. Desktop’s equivalent writes already have a deadline.

**Fix:** Bound each send and close operation; terminate transport on timeout. Test a sink that remains pending.

**If skipped:** A stalled connection can retain session resources and prevent timely interruption/cleanup.

### 11. P2 — A failed STT reconnect bypasses the retry budget

**What this is:** Bridge advertises three reconnection attempts after an established STT stream ends.

**Problem:** [session/mod.rs:352](https://github.com/vox-suite/vox-bridge/blob/ab804cd/src/voice/session/mod.rs#L352) propagates the first failed reconnect immediately. The retry count only covers successfully connected streams that subsequently close.

**Fix:** Count failed connection attempts inside the retry loop and make connection/backoff cancellable. Test stream close → connect error → successful reconnect → transcript.

**If skipped:** A brief provider outage unnecessarily ends a call. The existing reconnect test passes because every fake `connect()` succeeds; it does not cover this case.

### 12. P2 — Desktop cannot cancel voice setup promptly

**What this is:** Voice setup bootstraps Core, connects Bridge, handshakes and opens audio before becoming active.

**Problem:** [sidebar-voice-controls.tsx:254](https://github.com/vox-suite/vox-desktop/blob/945cf8e/src/components/shell/sidebar-voice-controls.tsx#L254) disables End while connecting. The native implementation first checks `stop_rx` at [bridge_voice/mod.rs:146](https://github.com/vox-suite/vox-desktop/blob/945cf8e/src-tauri/src/bridge_voice/mod.rs#L146), after microphone startup and active/ready notifications.

**Fix:** Enable cancellation during connecting and select cancellation around the entire setup future, including a check before audio opens. Test cancellation during delayed bootstrap/handshake.

**If skipped:** Setup can open the microphone and announce active after cancellation was requested.

### 13. P2 — Desktop accepts text that exceeds Bridge’s encoded frame limit

**What this is:** Desktop sends typed messages during a voice session.

**Problem:** [bridge_voice/mod.rs:76](https://github.com/vox-suite/vox-desktop/blob/945cf8e/src-tauri/src/bridge_voice/mod.rs#L76) validates raw text up to 8192 bytes, then adds JSON framing at line 168. Bridge limits the complete control/frame to 8192 bytes. An accepted 8192-byte ASCII message encodes to 8217 bytes; escaping crosses the limit earlier. The UI has already accepted the local enqueue.

**Fix:** Serialize once and validate the encoded byte length before enqueueing. Android already checks encoded bytes in `VoiceSession.kt:156`; share contract fixtures.

**If skipped:** A valid-looking typed message can disconnect the voice session.

### 14. P2 — A deleted Gmail message prevents cursor advancement

**What this is:** Gmail history IDs are fetched individually before the history cursor advances.

**Problem:** [gmail.rs:504](https://github.com/vox-suite/vox-connections/blob/ffd1920/src/providers/gmail.rs#L504) treats message 404 as generic failure. [gmail_sync.rs:215](https://github.com/vox-suite/vox-core/blob/425536f/src/gmail_sync.rs#L215) retains the entire cursor on any message error. A permanently deleted message keeps the window failing and replaying until history recovery replaces it.

**Fix:** Distinguish permanent not-found from transient failure, record a skipped identity, and advance over permanent misses. Test deleted ID plus valid ID in one history window.

**If skipped:** Repeated work and unhealthy sync persist; newer messages may still ingest, so this is not a claim that all subsequent email is blocked.

### 15. P2 — One hanging Gmail request stalls the shared Gmail worker

**What this is:** Pending Gmail and reconciliation work run serially in one worker loop.

**Problem:** [gmail.rs:102](https://github.com/vox-suite/vox-connections/blob/ffd1920/src/providers/gmail.rs#L102) uses a client without configured request deadlines. [runtime.rs:72](https://github.com/vox-suite/vox-core/blob/425536f/services/worker/runtime.rs#L72) awaits work sequentially. Delta processing retains an advisory-lock transaction across network/extraction work, consuming a pool slot.

**Fix:** Configure connect/read/request and per-connection budgets; process independent connections with bounded concurrency. Test a provider endpoint that never completes a response and ensure another account progresses.

**If skipped:** One account/provider request can stall Gmail processing for other users and occupy scarce database capacity.

### 16. P2 — Old Gmail work can overwrite a reconnected account’s cursor

**What this is:** Individual message commits check authorization/credential generations.

**Problem:** Final cursor persistence at [gmail_sync.rs:229](https://github.com/vox-suite/vox-core/blob/425536f/src/gmail_sync.rs#L229) checks only connection/user ID and numeric cursor ordering. Reconnect between the last message commit and finalization can merge an old mailbox cursor into current metadata. The update’s affected-row count is also ignored before marking sync healthy.

**Fix:** Require captured account and authorization/credential generations at finalization; treat zero affected rows as stale work. Test using a barrier immediately before finalization.

**If skipped:** Reconnected accounts can inherit an inappropriate cursor and misleading healthy status.

### 17. P2 — General job workers continue after losing their lease

**What this is:** The general queue heartbeats jobs while handlers execute; another worker can reclaim expired work.

**Problem:** [workers/mod.rs:114](https://github.com/vox-suite/vox-core/blob/425536f/src/workers/mod.rs#L114) only breaks the heartbeat task when lease extension fails; it does not cancel the handler. [db/jobs.rs:75](https://github.com/vox-suite/vox-core/blob/425536f/src/db/jobs.rs#L75) does not issue a generation fence, and completion/renewal check owner but not current expiry. After a pause or database failure lets another worker reclaim a job, the original handler can still perform side effects before its final completion fails.

**Fix:** Propagate lease loss into handler cancellation and fence transactional mutations by generation/valid lease. Reuse the stronger invariants already implemented for assigned runs in `durable_tasks/runs.rs`, rather than introducing another queue abstraction.

**If skipped:** Expired and reclaimed jobs can execute concurrently. Cancellation alone does not guarantee exactly-once external delivery; stable operation identities are also needed.

### 18. P2 — Plan routing failure silently starts mutation/research

**What this is:** A router distinguishes a chat answer from refinement, branching and research.

**Problem:** [space_workflow.rs:508](https://github.com/vox-suite/vox-core/blob/425536f/src/agents/space_workflow.rs#L508) defaults to `refine` or `extend` when the client is unavailable, times out or returns invalid output. A question that should have been answered without workspace changes can therefore spawn new tasks after a router failure.

**Fix:** Return a retriable unavailable/needs-clarification state that preserves the workspace. Only start mutation after a valid routed decision or an explicitly typed operation.

**If skipped:** Provider failure changes the requested behavior and consumes research budget unexpectedly.

### 19. P2 — Invalid environment values silently change runtime behavior

**What this is:** Core centrally loads settings but does not consistently validate configured values.

**Problem:** [config.rs:98](https://github.com/vox-suite/vox-core/blob/425536f/src/config.rs#L98) recognizes only lowercase `false` or `0`; `VOX_EVENT_AGENT_ENABLED=FALSE` unexpectedly enables the event agent. Malformed pool numbers silently become defaults at line 81, while syntactically valid zero values are accepted. Optional client-construction failures are further reduced to `None` in the worker.

**Fix:** Distinguish absent settings from malformed present values, parse booleans explicitly, validate numeric ranges and report capability availability. Keep documented defaults for genuinely absent optional settings.

**If skipped:** Operator mistakes are hidden, including unintended autonomous processing. Do not remove benign defaults indiscriminately.

### 20. P2 — Dead branches and duplicate configuration obscure the actual implementation

**What this is:** Legacy compatibility code remains beside active workflow and configuration paths.

**Problem:** [spaces.rs:728](https://github.com/vox-suite/vox-core/blob/425536f/services/api/routes/spaces.rs#L728) repeats workflow-v2 handling, but every v2 path already returns at line 706. [api/config.rs:8](https://github.com/vox-suite/vox-core/blob/425536f/services/api/config.rs#L8) and [worker/config.rs:8](https://github.com/vox-suite/vox-core/blob/425536f/services/worker/config.rs#L8) define unused wrappers with `allow(dead_code)` while both entrypoints use `Config` directly. Repository-wide symbol searches found only wrapper declarations/implementations. `MemoryService::find_user_by_name` also has no in-workspace callers, but it is public library API and needs an external-consumer check before removal.

**Fix:** Delete the unreachable block and unused internal wrappers/module declarations. Treat the public helper as a removal candidate, not proven globally dead code.

**If skipped:** Future fixes can target logic that never runs; suppressed warnings keep obsolete interfaces in circulation.

## Fallback inventory

| Layer | Current behavior | Assessment |
|---|---|---|
| Core required configuration | Missing database/service/Gemini/Exa values fail loading | Appropriate fail-fast boundary; malformed values need stronger validation |
| Pool/Redis/voice defaults | Pool 10, acquire timeout 5s, Redis service hostname, predefined voice/model | Document defaults and validate deployment capability; defaults alone are not defects |
| Event agent flag | Everything except exact `false`/`0` enables | Finding 19 |
| Bridge outbound | Missing/failed client construction becomes no provider dispatch | Finding 7 |
| Scheduled planner | Error and no-action both collapse to empty → reminder call | Finding 6 |
| Plan request router | Timeout/unavailable/invalid output → refine/extend | Finding 18 |
| Linked Project context for Plans | Missing/archived Project supplies empty context | Make loss of grounding visible; do not imply original project evidence was used |
| Project ideation | Provider/timeout/invalid response returns error; original project remains | Good boundary; no alternate-model fallback identified |
| Project model override | Invalid override falls back to configured Gemini model | Report effective model in diagnostics; avoid silently hiding operator mistakes |
| Connector encryption | Missing key disables/unavailable; malformed configured key rejected | Fail-closed behavior is tested |
| Calendar OAuth refresh | Missing client values become empty strings and fail provider authorization | Prefer explicit configuration failure before making provider request |
| Gmail history recovery | Invalid history → reconciliation baseline | Intentional recovery; permanent message errors still poison cursor (finding 14) |
| Gmail extraction | Invalid/unsupported facts yield review/unavailable paths rather than invented events | Preserve this evidence boundary |
| Batch event input | Missing external ID → UUID; missing time → now | Makes retries/time unstable; require stable source identity for retriable clients |
| No event triager | `jev_not_configured` nonretryable state | Explicit degradation, but missing capabilities should be observable |
| Phone cached greeting | Generic greeting on cache miss/error/deadline, without waiting for full DB/LLM context | Useful latency fallback; not proof of personalized context readiness |
| Voice completeness | No Jev → deterministic settling | Intentional behavior change; test both modes |
| Voice failure speech | Spoken apology uses the same synthesis provider | Cannot recover from that provider being unavailable |
| STT/TTS failover | No alternate-provider failover identified | Product availability decision, not automatically a bug; fix retry/cancellation first |
| Desktop/Android transport | Audio/transport failure ends session and asks for retry | Make retry identity and cleanup deterministic |

## Rust and engineering priorities

1. **Preserve error meaning.** Replace error-to-success conversions at external-action boundaries with typed outcomes: no-op, unavailable, retryable, accepted, completed and unknown. Findings 6, 7 and 18 are concrete examples.
2. **Give spawned tasks an owner.** Use scoped cancellation/abort-on-drop for voice responses and heartbeats. Parent cancellation must not detach work that still has authority to mutate state.
3. **Use one lease contract.** Assigned runs already use generation and expiry checks; general jobs and direct Plan writes should meet the same invariants. Share small repository primitives where useful, without merging unrelated execution models wholesale.
4. **Use checked SQL for fixed queries.** Apply `query!`/`query_as!` with maintained offline metadata where practical, or integration tests against a disposable database. The node-edit SQL was simplified and regression-tested; its original guaranteed-failure claim was disproven.
5. **Share cross-client wire fixtures.** Project JSON must round-trip on Android and desktop; voice limits must apply to encoded frames. Generate models where feasible and preserve extension fields where whole-document replacement remains.
6. **Make ingestion identity durable.** Store stable source identifiers independently of disposable payloads. Separate permanent provider absence from transient errors and fence final cursor updates.
7. **Bound I/O and pool usage.** Reuse deadline-configured HTTP clients. Avoid holding database transactions across provider/LLM work when a short lease or separately fenced transaction can enforce ownership.
8. **Remove duplicate policy implementations.** The batch route bypasses the event service, desktop and Android validate text differently, and the node route contains unreachable v2 logic. These already produce behavior differences; consolidate those specific contracts.
9. **Expose degraded capabilities.** Startup should show effective model/provider availability and disabled features without logging secrets. Count fallback paths, lease losses, skipped permanent IDs and stale-write rejections.

The shortest safe cleanup removes the two configuration wrappers (about 38 lines total) and unreachable v2 block (12 lines), plus their module declarations. No dependency-removal recommendation is supported by this review. Public exported functions were not classified as globally dead solely because local references were absent.

## Verification performed

| Check | Result | Limit |
|---|---|---|
| Core `cargo test --locked --offline --lib` | **36 passed, 30 ignored** | Ignored tests need isolated PostgreSQL/pgvector; they were not exercised |
| Core `cargo test --locked --offline --bin vox-core-api` | **0 passed, 1 ignored** | Confirms compilation of this target, not database route correctness |
| Bridge `cargo test --locked --offline --test stt_reconnect_test` | **1 passed** | Covers repeatedly closed connected streams; does not cover failed reconnect calls |
| Source checks | Entry points, callers, migrations, bind lists, task lifetime, serialization and dependency pin reviewed | Static evidence; findings were not all reproduced at runtime |

An initial filtered library command matched zero tests; it is not counted as a regression check. The full library suite above was run afterward.

## Fix sequence and acceptance

1. Protect existing data and external actions: findings **1–7**. Require Android project round-trip fixtures, DB node-edit test, event replay/batch tests, and schedule no-op/failure/unconfigured-dispatch tests.
2. Enforce stop/navigation/lease ownership: findings **8–13 and 17**. Use paused futures/fake sinks, barriers and reclaimed leases rather than arbitrary sleeps.
3. Make connector progress recoverable: findings **14–16**. Test deleted messages, hung provider, reconnection during final cursor persistence and next-account progress.
4. Tighten configuration and simplify code: findings **18–20**, then run targeted Clippy and the broader integration matrix after fixes.
5. Validate real flows separately: authenticated desktop microphone/playback/interrupt/end; Android hardware and focus-loss/sign-out; inbound/outbound phone call; connector authorization and reconnect; Timeline/Pulse updates; cross-client Project editing; Plan stop/edit/commit.

Verdict: fix cross-client data loss, detached voice tasks, ingestion identity, and call-dispatch fallbacks first; simplify fragile Plan editing. The system has useful revision, authorization and generation protections, but several paths bypass those protections.

Lean: approximately 50 lines plus module declarations removable from verified internal dead/duplicate paths; 0 dependencies proven removable.

Not checked: production deployment/environment values, live OAuth/provider accounts, real phone calls, native desktop audio, Android hardware/build, all connectors individually, every package/file, benchmark/load behavior, complete Clippy output, or disposable-database integration tests. No broad claim of security, absence of races, or production readiness follows from this audit.
