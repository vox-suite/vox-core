# Durable assigned tasks

A signed host starts a task with a title, instruction and optional owned agent
key. Core resolves the Personal Assistant when omitted. The task and its run
persist independently of the browser or voice connection. Hosts query
`POST /v1/durable-tasks/query` with their authenticated `host_context`, an
optional UUID cursor and a limit of 1–50 (default 20). Results include state,
pinned actor/instruction version and structured work result. Internal tool checkpoints remain private.

## Execution and authority

The worker's dedicated assigned-run loop claims only jobs with an immutable
`assigned_task_runs` binding. The binding retains owned actor instructions,
model version, reviewed connection declarations and enabled skill digests.
Checkpoints cannot replace that authority. Current actor availability, service
access and agent grants still apply; new grants do not expand an existing run.

Every tool operation uses the governed capability library or that actor's
memory interface. Changes create exact action proposals. Proposal creation and
its approval wait commit together under the current run lease. The worker does
not dispatch a write on its own. Authenticated approval and existing execution
interfaces determine the action outcome. Resume refuses an unresolved approved
proposal and includes its terminal outcome in the next checkpoint. A clarification wait requires a bounded user reply on resume; that reply supplies task data without changing the pinned actor or authority.

Claims use lease generations, heartbeat and conditional state transitions.
Expired leases may be recovered; stale workers cannot commit results or create
run-bound proposals. Cancellation stops future work, retains evidence and does
not undo an external effect. Browser disconnects do not retry actions.

## Initial limits

Each context admits eight unfinished tasks and at most two leased assigned
runs. Runs have a 24-hour deadline, three attempts and 24 tool operations. Each
model attempt has six turns and a 90-second deadline. Exhaustion persists a
budget wait that cannot be resumed into a fresh allowance.

The shared capability library bounds loaded tools to eight, skills to three
and accumulated capability content to 12,000 UTF-8 bytes per attempt. This is a
conservative byte limit, not a measured 12,000-token allowance. Model responses
and checkpoints have separate size limits. These defaults are safety bounds,
not performance or scale guarantees.

## Release scope

The initial runner supports the deployment-approved Gemini configuration.
Unavailable actor/model configurations fail safely. Scoped specialist
permissions and child-run execution are not implemented in this slice;
lineage-bearing runs are rejected. Automatic proactive task creation and
scheduled external actions are outside the accepted product scope.

Generic span jobs without an assigned-run binding no longer invoke the old
model-only executor or report invented success. Explicit reminder delivery
remains on its separate schedule path. Obsolete autonomous spans are no longer enqueued by the schedule ticker. Already queued obsolete autonomous jobs fail
with `assigned_run_binding_required`; they do not become governed tasks merely
because their job kind matches.

## Verification

CI uses disposable PostgreSQL databases for context/wait boundaries and
assigned-run recovery, stale lease denial, immutable authority, persisted
query/results and cancellation. Mock runner evidence is not live model or
provider certification. Deploy API and worker from the same reviewed revision
and apply migrations before accepting new tasks. Keep connection encryption
keys unchanged. Live approval/read/write, crash recovery and host reconnect
remain release checks beyond database correctness.
