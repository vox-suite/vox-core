# Durable tasks and resumable runs

Core's existing context-owned `tasks` are the durable task record. Each new
platform task has one live `task_runs` record, which persists its checkpoint,
lease, and an explicit wait reason: clarification, connection, approval,
authentication, or reconciliation. A client connection has no lifecycle
authority: disconnecting it changes nothing.

The durable-task module owns start, read, wait, resume, cancel, expired-lease
recovery, and worker claim behavior. A fresh signed host-context assertion is
required for host-facing operations. Cancelling prevents future work and never
claims to undo an external effect that may already have occurred.
