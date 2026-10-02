# Scoped specialist delegation

Ordinary chat keeps the Personal Assistant. An identical repeated delegation call within the same model turn reuses its task; a different second call is refused, so cloned tools cannot mint fresh parent budgets. The governed library exposes `specialists` (bounded owned assistant names and enabled permission summaries), `specialist_scope` (selected tool/account metadata) and `delegate`. Delegation creates a durable parent for a conversation request, or uses the existing assigned-run parent. A brief contains only requested work and explicit selected inputs, never an automatic transcript or memory export.

`delegate` accepts `specialist_agent_key`, a brief of at most 8192 UTF-8 bytes, `scope: {capabilities: [{connection_id, capability_external_key}]}` and optional `permission_id`. Core resolves references to immutable reviewed declaration fingerprints. Without a permission, the requested scope must be in both assistants' current grants and the parent's pinned authority. A permission may allow the parent to ask a specialist for explicitly scoped work available to that specialist. It never gives the parent those grants.

## Signed host consent

- `POST /v1/delegation-scopes` accepts `host_context`, `requester_agent_key`, `specialist_agent_key`. Returns capability references with `account_display_id`, `integration_name`, `tool_name`, and nonsensitive preference keys only.
- `POST /v1/delegation-permissions` accepts `host_context` and `permission` containing that assistant pair, capability references, optional `preference_keys`, and optional `parent_run_id`. A parent run makes permission once-only; omission remembers an immutable scope.
- `POST /v1/delegation-permissions/query` returns context-owned permissions including mode, state, used, fixed scope and selected preference pins.
- `POST /v1/delegation-permissions/{id}/revoke` revokes subsequent use.
- `POST /v1/durable-tasks/stop-all` cancels active assigned work in the authenticated context; `{cancelled, undo:false}` does not undo provider effects.

An ordinary-chat request lacking parent access first returns a task in clarification consent wait. Nonstream `POST /v1/conversations/respond` retains `text` and adds the exact server-owned `task`; the host binds once permission to this task's `run_id` and resumes with an explicit reply. The worker resolves and consumes that exact permission before invoking a model. Without matching consent it waits again. Streamed responses publish `event: task` with `{task: DurableTask}` after text or model error and before `[DONE]`, only for the exact governed captured handle re-resolved in the authenticated context. Dropping a stream does not cancel durable work; reconnect by that task ID. Existing text delta and error frames remain unchanged.

Only signed, authenticated hosts may create consent. Model tools cannot grant or expand it. Permission creation resolves an owned distinct assistant pair, limits 64 enabled permissions, 1–32 distinct capabilities, and at most eight selected nonsensitive preference keys. Shared selected preference values total at most 2048 bytes and are pinned by value digest. Changed, removed, sensitive or revoked preferences require renewed consent. The specialist receives only these selected advisory values; they confer no execution authority. Private memory tools are disabled for delegated runs. Skills are not delegated in this initial scope.

## Runtime boundaries

One delegation level and at most two children per parent are permitted. Parent and children share deadline, tool-call and attempt allowances; delegation never refreshes these limits. The parent persists `wait_reason: specialist` before the model can continue. Child completion durably returns one bounded work result with an untrusted-data marker; it cannot authorize a provider action. Results from a revoked permission are withheld. Budget exhaustion keeps a budget wait.

Every child invocation rechecks current grants, exact declaration fingerprints, parent availability and permission. Execution start and dispatch independently recheck delegated authority; permission shared locks serialize the final dispatch claim against revocation. Consequential calls still require the existing exact proposal and authenticated approval path. Resume does not dispatch them automatically. Stop cascades to children and expires undispatched proposals, while uncertain provider outcomes remain available for reconciliation.

Task query/status includes `parent_task_id` and `root_task_id` for attribution and reconnect. Database/mock regressions prove scope denial, once consumption, preference drift, child result return, revoked execution start/dispatch, ordinary-chat handoff and descendant cancellation. Live model selection quality, actual provider authorization, production deployment and UI smoke testing remain separate checks.
