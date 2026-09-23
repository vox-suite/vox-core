# E05 resource context migration

This migration follows the platform schema restoration from PR #54. It is
an expansion over the consolidated 21-table consumer baseline for fresh
installs, not a deployment migration from the older multi-migration schema.
The 2026-09-19 E05 rehearsal covered the earlier schema with `actions` and
`scheduled_tasks` tables. The current baseline replaced those with
`action_proposals` / `executions` and `schedules`, so its old counts and
migration cannot establish acceptance for the current schema.

## Ownership transition

- Every consumer table with `user_id` now also has `user_context_id`.
  Existing registered contexts keep their IDs. Users without a registered
  context receive a stable context in the reserved legacy scope.
- The compatibility trigger derives a missing context from the unique
  `user_contexts.user_id` mapping. It rejects a user with no context and
  rejects a supplied context owned by another user through a composite FK.
- A signed host assertion cannot resolve the reserved legacy scope. Legacy
  channel APIs still use it internally during this transition; their eventual
  removal belongs to E11.
- Verified standalone sign-in assigns a context in a separate internal
  `vox.standalone.web` scope before writing authentication identities or
  sessions. Signed host assertions cannot resolve this internal scope either.
- Cross-resource owner constraints cover sessions, conversations, collections,
  tasks, schedules, jobs, records, proposals, approvals, executions, and
  inbound events. Existing mismatches stop the migration rather than being
  silently reassigned.
- Conversation uniqueness includes context, channel, and external ID.
  Identical host-supplied IDs can now belong to different contexts.
- User deletion still retains audit rows with both owner columns cleared.

## Rehearsal on isolated PostgreSQL

Run `cargo test --locked --test resource_context_migration -- --ignored`
with `TEST_DATABASE_URL` pointing at an isolated PostgreSQL database. The
test creates its own schemas. It verifies:

| Fixture | Before | After |
| --- | ---: | ---: |
| Users | 2 | 2 |
| Contexts | 1 | 2 |
| Conversations | 2 | 2 |
| Tasks | 1 | 1 |
| Schedules | 1 | 1 |
| Resource owner orphans | not applicable | 0 |

The same rehearsal proves that the existing canonical context ID is unchanged,
the legacy context ID differs from the user ID, a duplicate external
conversation ID is allowed across contexts, and a forged context is rejected.
A second fixture has a task linked to another user's collection; the
migration rejects it. The focused `user_context`, `resource_context`,
`migration_contract`, and outbound-call suites pass on isolated PostgreSQL.
The full non-ignored Core suite passes with local socket access.

## Remaining acceptance work

1. If an existing deployment needs this revision, build and rehearse a
   **data-preserving upgrade from the older migration chain** first. The
   current supported merge scope is fresh install only.
2. Before accepting an upgrade path, reconcile counts for all 17 affected tables, capture
   orphan and cross-owner reports, record migration duration, and sample
   ownership evidence. Stop writes and restore a schema-compatible release if
   validation fails; preserve migration provenance before retrying.
3. Verify Bridge and existing API compatibility, then close channel-only
   authority under E11. The reserved legacy scope is a migration record,
   not an authority grant.
4. Resolve unrelated ignored integration failures exposed by the consolidated
   consumer baseline before claiming E05 or downstream acceptance.

E05 acceptance remains open until the applicable compatibility and integration
evidence is attached. Merging this fresh-install correction does not close E05.
