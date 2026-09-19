# Resource ownership migration

Platform V1 resources are owned by the canonical user context introduced in
[`user-context.md`](user-context.md). Conversation, schedule, task, and action
rows now carry both `user_context_id` and the temporary compatibility key
`user_id`.

## Ownership interface

`ResourceOwner` is the small interface passed to resource modules. It contains
the canonical context ID and its internal user ID. `IdentityService` produces an
owner in two ways:

- `ResolvedUserContext::owner` for authenticated host-app contexts; and
- `resolve_legacy_owner` for channel callers during the migration window.

The reserved `vox.legacy.deployment` / `vox.legacy.channel-host` scope gives
every pre-existing internal user one stable context. Matching channel values do
not become cross-host identity.

## Migration behavior

Migration `20260919000001_resource_user_context_ownership.sql`:

1. creates the reserved legacy scope idempotently;
2. preserves users that already have canonical contexts;
3. assigns every remaining user one legacy context;
4. adds `user_context_id` to conversations, schedules, tasks, and actions;
5. backfills each resource through its existing `user_id`;
6. validates composite `(user_context_id, user_id)` foreign keys;
7. validates that task/project and action/source/target relationships share the
   same owner;
8. replaces global conversation identity with context-scoped identity; and
9. aborts if any backfilled resource remains orphaned.

The columns remain nullable for the temporary rolling-compatibility window.
New code always dual-writes both identifiers. Reads accept a null context only
when the legacy `user_id` matches the current context's user, then mutation paths
opportunistically fill the context. A different context never falls through
that compatibility clause because one internal user belongs to exactly one
context.

Schedule updates now include the caller's existing `ChannelIdentity`, matching
schedule creation. A caller that supplies another user's identity receives
`not found`; existence is not disclosed.

## Isolation rules

- Conversation identity is `(user_context_id, channel, external_id)`.
- User-driven schedule reads and updates require the matching owner.
- Task create/list/get/update tools carry `ResourceOwner` and scope every query.
- Actions created by conversations, schedules, events, tasks, device commands,
  and outbound-call tools inherit the initiating owner.
- Internal workers may load a resource by durable job ID, but derived resources
  retain the loaded resource's context.
- Provider action callbacks remain authenticated machine callbacks bound to the
  action ID and provider call ID; they are not user-context assertions.

## Verification evidence

On 2026-09-19 the migration was rehearsed against a fresh PostgreSQL 17 database
containing three pre-existing users, two conversations, one schedule, one task,
one action, and one already-canonical context. The result contained three
contexts, preserved the existing canonical context, retained every resource,
and reported zero conversation, schedule, task, or action orphans.

Automated PostgreSQL tests additionally prove:

- equal external conversation IDs in separate contexts do not collide;
- one context cannot complete another context's conversation;
- one context cannot update another context's schedule;
- task list/get/update operations cannot observe or mutate another context;
- a task cannot be attached to another owner's project;
- action ownership rejects context/user mismatches; and
- an action cannot reference another owner's schedule, task, event, or device; and
- legacy conversation, schedule, event, voice-switching, action dispatch, and
  action callback flows retain behavior.

## Rollback and contraction

Rollback deploys the last schema-compatible application while retaining all
assigned context IDs and dual-written columns. Do not delete contexts or remap
resource ownership. If a migration integrity check fails, halt writes, restore
the last application version, inspect orphan counts, correct the source user
mapping, and resume the additive migration.

The later legacy-contraction ticket may make context columns non-null and remove
the null-context fallback only after Core and Bridge compatibility evidence is
accepted.
