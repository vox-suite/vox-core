# User-flow remediation — 17 October 2026

This implements the [user-flow review](user-flow-audit-2026-10-17.md) across Core,
Bridge, Desktop, Android and Connections. Local automated checks and isolated
PostgreSQL tests are separate from live device/provider verification.

## Disposition

| Finding | Change |
|---|---|
| 1 | Android retains unknown nested project JSON during edits, including flow/diagram metadata; matching is stable across history rollover, citation reorder and deletions. |
| 2 | Corrected the original audit: the node edit succeeded before changes on PostgreSQL 17/SQLx. Simplified mixed bind construction and retained a graph/task versioning regression. |
| 3 | Bridge owns response task lifetimes and cancels them on failure/parent abort; shutdown awaits cleanup. |
| 4 | Completed inbound records retain scrubbed durable source identities, preventing replay from creating new processing work. |
| 5 | Batch insertion and processing-job creation now commit atomically. |
| 6 | Scheduled planner errors propagate; successful empty plans remain no-ops instead of becoming calls. |
| 7 | Missing Bridge rejects dispatch before conversation/job creation; configured invalid clients fail startup explicitly. |
| 8 | Request decisions, goal creation, final selection, replies and workflow completion use generation/state fences. Terminal transitions also stop work atomically. |
| 9 | Android blocks project navigation during mutation and disables conflicting controls. |
| 10 | Twilio playback writes have deadlines. |
| 11 | Failed STT reconnect calls consume the retry budget. |
| 12 | Desktop setup is cancellable; late starts/cleanup cannot overwrite a newer session. |
| 13 | Desktop validates the encoded JSON frame, including escaping, before enqueueing. |
| 14 | Gmail distinguishes permanent missing messages, records excluded identities and can advance history. |
| 15 | Gmail HTTP requests and complete per-connection delta processing have time budgets. |
| 16 | Gmail cursor finalization checks account, authorization and credential generations; stale work cannot report healthy completion. |
| 17 | General job claims return a generation; all lease mutations require matching generation and unexpired lease. Heartbeat failure cancels handlers; dropped handlers abort renewal. General workers exclude assigned-run job kinds. |
| 18 | Invalid/unavailable Plan routing produces a retry explanation without implicit research/mutation. |
| 19 | Invalid boolean/numeric configuration is rejected; uppercase false is honored; invalid configured Bridge/SMTP clients are no longer silently disabled. |
| 20 | Removed unused API/worker configuration wrappers and unreachable workflow-v2 handling. Kept public API removal candidates whose external callers cannot be ruled out. |

## Regression coverage

- Voice response ownership under initial STT failure and parent abort; transient
  reconnect failure; stalled Twilio writes; encoded-frame limits; desktop setup
  cancellation and stale startup responses.
- Android project round trips, clearing defaults, deleting/reordering entries,
  history rollover and citation metadata identity.
- Isolated PostgreSQL: batch queueing, processed-event replay, stale Gmail cursor
  generations, expired/reclaimed leases, assigned-job exclusion, router failure,
  stopped/terminal Plans, and node version/edge/task rewiring.
- Core configuration validation and handler cancellation/heartbeat cleanup.
- Missing outbound configuration creates no phantom conversation; planner failure
  and explicit no-op never dispatch a call.

Connections is published first and Core pins its fixed Git revision. No sibling
path dependency, vendored connector copy or unpublished commit is required.

## Verification results

| Check | Result |
|---|---|
| Core full `cargo test --locked --offline` | Passed: 42 library tests and 1 socket regression; database/benchmark cases remain ignored by default. |
| Core isolated PostgreSQL regressions | 13 passed: 9 library regressions, 2 API regressions, planner behavior, missing Bridge behavior. |
| Core strict Clippy | `cargo clippy --locked --offline --all-targets --all-features -- -D warnings` passed. |
| Bridge full test suite | 92 passed; pre-push formatting/Clippy, tests and release build passed. |
| Desktop | 21 Rust tests, 25 Node TypeScript tests and Chromium cancellation regression passed; production build passed; ESLint passed with 9 existing warnings. |
| Android | 23 unit tests and debug APK build passed. |
| Connections | 23 passed, 17 database tests ignored; one existing unused-variable warning in account tests. |

These are local checks on the final code. The PostgreSQL regressions used a new
isolated local container with synthetic data and full Core migrations. They do
not mean the entire ignored integration/benchmark suite was run.

## Limits

No production secrets/settings, live OAuth accounts, real phone calls, native
desktop audio or Android hardware were exercised. Retaining source identities
prevents future replay duplicates; previously deleted identities cannot be
reconstructed. Lease fencing and cancellation do not make arbitrary external
provider actions exactly-once; provider receipts and idempotency remain necessary.

The no-action behavior is deliberate: a model/provider error cannot authorize a
replacement external action. Benign documented defaults and generic cache-miss
greetings remain.
