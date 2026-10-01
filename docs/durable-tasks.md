# Durable tasks and resumable runs

Core's context-owned `spans` are the durable assigned-work record. Each new
platform task has one `execute_span` job, which persists its checkpoint, lease,
and an explicit wait reason: clarification, connection, approval,
authentication, or reconciliation. A client connection has no lifecycle
authority: disconnecting it changes nothing.

The durable-task module owns start, read, wait, resume, cancel, expired-lease
recovery, and worker claim behavior. A fresh signed host-context assertion is
required for host-facing operations. Cancelling prevents future work and never
claims to undo an external effect that may already have occurred.

## Worker boundary during the governed-run transition

Host-facing task reads and mutations require the exact owning user context as
well as the user ID. Production job claims exclude persisted wait reasons,
including expired running leases with a wait. Interactive durable tasks and
proposal tasks are also excluded from the legacy autonomous span executor,
even after a host resumes them or replaces their checkpoint. Resuming does not
constitute approval or authority to dispatch an external call.

These tasks remain queued until a governed durable dispatcher is implemented.
The existing explicit schedule worker and autonomous span jobs retain their
current behavior; this boundary does not certify their full action governance
or complete assigned-work recovery (W9). Exact connector execution continues
through the separately authenticated approval and execution interfaces.
