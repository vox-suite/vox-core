# Voice turns and speculative lookups implementation plan

## Approved design
Implement the nine changes approved in conversation: partial speech Jev read-only lookups, final Gemini response using relevant results with one delayed acknowledgement, continuation collection, supersession, safe verification recovery, stable call identity, real speaker inference and quality gates, lower overhead, accurate metrics.

## Constraints
Keep Core intelligence/tools and Bridge provider/audio boundaries. No source comments added. No deployment, push, or production data cleanup. Existing caller identity authorization remains; biometric evidence is advisory unless a compatible enrolled model gives sufficient evidence. Model calibration cannot be asserted without real labeled recordings.

## Task 1: Core identity and speculative lookups
Owner: Core implementer, all vox-core files except this plan.
- Add failing regression tests for incomplete phone collection, no automatic profile creation, stable conversation after speaker switch, compatible voice model quality gates.
- Keep conversation owner immutable; store active speaker separately and resolve on later turns. Preserve requested question across verification.
- Add POST /v1/conversations/speculate using identity, external_conversation_id, text, turn_id, revision. It plans and starts read-only work, returning status without waiting for tools; deduplicate and bound work. No user message persistence or Gemini during speculation.
- Add optional turn_id and revision to respond request. Final processing verifies owner, revision and final intent before using speculative data, then waits for required work before Gemini. Cache by owner/call/turn and validated tool/arguments, expire and clean up.
- SSE emits a lookup_pending event before waiting on outstanding tools, separate from text delta. Final response can proceed normally on speculation failure.
- Log identity/history/verification/tool and model timing separately, including early intercepted paths. Minimize redundant database calls where safe.
- Tests: core cargo test, format, clippy; mock Jev/tools to prove speculative execution is read-only, deduplicated, scope-safe, stale-safe and waits before final model execution.

## Task 2: Bridge turn scheduler and protocol
Owner: root, vox-bridge.
- Emit STT partial transcripts; debounce partial updates and send speculate requests asynchronously with turn_id/revision.
- Add final transcript settling, combine continuations, supersede obsolete pending generations; do not queue stale fragments.
- Distinguish thinking from playback; cancel obsolete requests, ignore stale completion/marks, cleanup partial work on stop.
- Track actual last voiced audio timestamp. Separate substantive audio/filler/TTS/request timings.
- Play at most one acknowledgement when Core signals lookup_pending and result is not ready promptly.
- Regression tests for partial forwarding, turn continuation/correction, cancellation, pending tools acknowledgement, genuine speech-end timing and playback marks.

## Task 3: Real speaker inference and fast extraction
Owner: root, vox-bridge speaker module and dependencies.
- Replace mislabeled statistical voiceprint with actual optional ONNX model inference; never emit old synthetic features as biometric evidence.
- Require at least one second of accepted speech, finite nonzero output, truthful model identity and duration metadata. Clear insufficient current evidence rather than reusing old samples.
- Snapshot audio under short lock and extract outside it; use FFT and reusable feature computation.
- Document model contract/runtime setup and calibration requirements. Validate extraction using deterministic ONNX fixture where feasible.

## Task 4: Integration and review
- Verify HTTP payload and SSE compatibility across repositories.
- Run all repo checks and required database tests with local disposable database if available.
- Review final diff for wrong-turn results, ownership leakage, unbounded tasks, timer races and false biometric claims; fix findings.
- Report actual validation and operational configuration still needed.
