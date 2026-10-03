# PlayStation activity capture implementation

Goal: Link a verified PSN account and automatically record observed gaming activity in Vox spans.

Architecture: Connections owns PSN transport and encrypted tokens. Core owns authenticated orchestration, snapshot checkpoints and span writes. Web provides linking, status, sync and pause controls. Existing reviewed flow is the design brief; implementation is authorized by the user's end-to-end request.

Constraints: No invented historical sessions, no passwords or NPSSO retained/logged, no automatic agent grants, no push or deployment. First observation is a baseline. Later increases are gaming activity over an observation window, with exact session times unknown. Disconnection destroys stored credentials and stops capture; existing spans remain.

- [x] Correct malformed timestamps and cumulative duration behavior. User requested no new tests or code comments; use existing checks only.
- [x] Implement verified NPSSO exchange, identity verification, bounded pagination and encrypted access/refresh token lifecycle in Connections. Add credential schema and fail-closed revocation.
- [x] Implement Core transactional checkpointing with stable account-scoped identities, span provenance and notifications, retry scheduling, manual sync and authenticated host routes. Wire background worker.
- [x] Implement authenticated Web proxy and connection controls using shared components. Test identity binding and secret exclusion from responses.
- [x] Run Connections and Core tests/checks against the changed shared crate, database integration where available, and Web tests/lint/build/browser checks. Document configuration and live verification limits.

Review focus: reconnect races; revoked credentials; unchanged snapshots; cumulative counter resets; missed/unknown provider timestamps; cross-context isolation; atomic checkpoint/span commit; retries and pagination.

Validation: Connections and Core compile and pass Clippy against the sibling Connections checkout; existing unit, web and consumer browser checks pass. All Core migrations applied to a disposable local PostgreSQL 17 database. Real PSN linking and refresh remain unverified; release requires publishing Connections and updating Core's dependency pin.
