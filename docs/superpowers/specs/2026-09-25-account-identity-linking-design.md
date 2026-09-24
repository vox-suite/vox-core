# Account/identity linking: box, desktop, web

Date: 2026-09-25
Repos touched: vox-core, vox-bridge, vox-web. vox-desktop is unaffected (its
auth path is already correct and is the pattern vox-web moves onto).

## Problem

Today the same person calling the box, signing into desktop, and signing
into web can end up as three unrelated `users` rows in vox-core, with no way
to reconcile them:

1. **Inbound phone/WhatsApp calls** resolve through `channel_identities`
   (`vox-core/src/identity/mod.rs::resolve`), minting a new `users` row on
   first contact if the number isn't known.
2. **vox-desktop** signs in via Supabase Google OAuth, then calls
   `POST /v1/auth/exchange` (`vox-core/services/api/routes/auth.rs`), which
   matches/creates a `users` row via `auth_identities(issuer, subject)`.
3. **vox-web** currently signs in via Supabase OAuth in the UI
   (`sign-in-form.tsx`) but resolves identity through a separate, disconnected
   `better-auth` + federated-Ed25519 "host app" path
   (`consumer-auth/auth-options.ts`, `core-host-client.ts`,
   `account-authority.ts`) that the actual sign-in button never triggers
   correctly — this looks like an abandoned mid-migration, not a deliberate
   design.

Additionally, inbound Twilio calls tag their channel identity as
`"twilio"` instead of `"phone"` (`vox-bridge/src/channels/twilio/stream.rs`),
which silently bypasses vox-core's existing phone↔WhatsApp cross-linking
logic and never matches the `"phone"` channel outbound calls use.

There is no endpoint for an authenticated user to attach their phone number
to their account, and the one generic linking primitive that exists
(`identity_adapters::link_identities`) is documented as deliberately
non-merging — it never moves conversations/tasks/history between linked
identities, so it does not solve "box remembers me."

The Redis cache the box's greeting uses
(`vox-core/src/memory/cache.rs::RedisContextCache`) is a pure read-through
mirror of Postgres (no TTL, rebuilt from Postgres every ~60s via
`src/memory/greetings.rs`) — it is not a source of truth and does not need
to be touched directly; it already reflects whatever Postgres says once the
identity graph is fixed.

The repo also carries three overlapping schema definitions: a stale
pre-rewrite draft (`vox-core/schema/01..21_*.sql`), a stale production dump
(`vox-core/supabase_schema.sql`) with table names (`user_identities`,
`client_devices`, `user_goals`, `outbound_calls`) that don't match what the
running Rust code actually queries, and the real live schema
(`vox-core/migrations/*.sql`, mirrored by `vox-core/schema/target_core.sql`).

## Scope decision

The multi-tenant "host app" platform layer (`platform_deployments`,
`host_apps`, `host_organizations`, federated Ed25519 proofs — for letting
third-party companies embed Vox under their own auth) is confirmed
speculative/leftover for this product today. This design does not build on
it and does not extend it. It is left in place in the schema (removing it
is a separate, riskier change) but vox-web's move off `better-auth` removes
its only current first-party dependent.

## Design

### 1. Unify vox-web onto the same auth path as vox-desktop

vox-web's sign-in already produces a Supabase session
(`sign-in-form.tsx` → `supabase.auth.signInWithOAuth`). Instead of routing
through `better-auth`/`core-host-client.ts`'s federated-proof path, call the
same endpoint vox-desktop already uses:

- `session.ts` (`currentConsumer()`) calls `POST /v1/auth/exchange` with the
  Supabase access token, exactly as `vox-desktop/src-tauri/src/auth.rs` does,
  and uses the returned `user_id` / session bearer instead of aliasing
  `supabase user.id` directly as `coreUserContextId`.
- Delete `consumer-auth/auth-options.ts`, `core-host-client.ts`,
  `account-authority.ts`, and the `better-auth` dependency and its route
  (`src/app/api/account/auth/[...all]/route.ts`) — dead once nothing calls
  them.

Result: desktop and web resolve the same Google account to the same
`users.id`, via the same existing endpoint. No new identity mechanism.

### 2. Capture the Google display name on account creation

In `/v1/auth/exchange` (`services/api/routes/auth.rs`), when inserting a new
`users` row, set `display_name` from the Google profile claim if the ID
token carries one and it isn't already set. This means a brand-new signup
already has a name before their first call, so the box can greet them by
name immediately.

### 3. `POST /v1/me/phone` — attach/merge a phone number

New authenticated endpoint (bearer session from step 1), body
`{ phone_number }`. Server normalizes the number, then:

- No existing `channel_identities` row for that number → insert one
  pointing at the caller's `user_id`.
- Row exists and already points at the caller's own `user_id` → no-op,
  success.
- Row exists pointing at a *different* `user_id` (the phone-only user
  created by a prior box call) → **merge**, in one transaction:
  - `UPDATE <table> SET user_id = $new WHERE user_id = $old` for every table
    with a `user_id` FK into `users` (the fixed list currently in
    `migrations/20260923000000_initial_core.sql` and
    `..._restore_platform_contract.sql`: `auth_identities`,
    `channel_identities`, `auth_sessions`, `conversations`, `collections`,
    `tasks`, `schedules`, `jobs`, `records`, `devices`, `connections`,
    `action_proposals`, `executions`, `inbound_events`, `audit_events`,
    `user_contexts`).
  - Set `users.status = 'disabled'` on the old row (tombstone; not deleted,
    to keep FK/audit history intact — nothing still points at it after the
    reassignment above, this is just a marker so it's never resolved again).

No OTP/SMS verification before linking — phone number is trusted as
entered. Tradeoff accepted: entering a number that already belongs to
someone else's linked account will silently pull that account's history
onto the entering user. Flag this in the phone-entry UI copy.

### 4. Fix the Twilio inbound channel tag

`vox-bridge/src/channels/twilio/stream.rs` builds `CallContext` with
`channel: "twilio".into()`. Change to `"phone"`. This is what makes
inbound-call identities match what `/v1/me/phone` (step 3) and outbound
calls already use, and is what makes vox-core's existing phone↔WhatsApp
cross-linking in `identity::resolve` actually engage for real calls.

With steps 3 and 4 in place, "the box stops asking for a name" requires no
further code: `cached_opening()`
(`vox-core/src/conversations/service.rs`) already looks up
`(channel, external_id) → name` and greets by name when found — it just
needs the identity graph underneath it to be correct.

### 5. Delete the stale schema files

Delete `vox-core/schema/01_extensions.sql` through `21_user_voiceprints.sql`
and `vox-core/supabase_schema.sql`. Keep `vox-core/schema/target_core.sql`
(mirrors the live `migrations/` schema). These stale files describe a
pre-rewrite schema (`user_identities`, `client_devices`, `user_goals`,
`user_insights`, `outbound_calls`, ...) that the running code does not
query, and actively mislead anyone reading the repo to understand the
identity system.

## Explicitly out of scope

- OTP/SMS phone verification.
- Any change to the `host_apps`/`platform_deployments`/federated-proof
  layer itself.
- Merging/UI changes in vox-desktop (already correct).
- Any new tables. Reuses `users`, `auth_identities`, `channel_identities`.

## Testing

None added. Any existing test that fails because of the `better-auth`
removal (vox-web), the schema file deletions, or the Twilio channel value
change is to be deleted, not fixed to pass around the change.

## Comments

No code comments to be added as part of this work.
