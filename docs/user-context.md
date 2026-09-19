# Canonical user-context contract

The canonical user context is the isolation key for a host app's user. It is
defined by all of the following values:

- platform deployment;
- host app;
- optional host organization; and
- the host app's stable user identifier.

Matching email addresses, phone numbers, or host-user identifiers outside that
complete scope do not link contexts. Two host apps using `user-123` therefore
receive different context and internal user identifiers. Two organizations in
one host app also receive different contexts for the same host-user identifier.

## Interface

`IdentityService::resolve_context` accepts a `UserContextSubject` only after a
caller has authenticated the host assertion. It requires the deployment, host
app, and optional organization to be registered, normalizes surrounding
whitespace on the host-user identifier, rejects empty identifiers and values
over 512 UTF-8 bytes, and returns a stable
`ResolvedUserContext`. Concurrent first resolution is idempotent.

`IdentityService::authorize_context` checks that an internal user belongs to an
exact context. Missing contexts and cross-context user identifiers both fail
closed as `IdentityError::AccessDenied`; callers must not infer other users or
contexts from a denial.

This ticket does not expose context resolution directly over HTTP. Host
registration and authenticated host assertions are added by the host-trust
work. Until then, only already-trusted in-process callers may invoke the new
interface.

## Persistence constraints

The additive migration creates `platform_deployments`, `host_apps`,
`host_organizations`, and `user_contexts`.

- Host-app keys are unique inside one deployment.
- Organization keys are unique inside one host app.
- A context's host app must belong to its deployment.
- An optional organization must belong to the same deployment and host app.
- A host user is unique within the complete deployment/host/organization scope.
- Every context owns a distinct internal `users` row.

The schema deliberately does not backfill existing resources. Conversations,
schedules, tasks, and actions continue using their existing `user_id` ownership
until the resource-migration ticket assigns legacy users to canonical contexts.

## Legacy compatibility

`ChannelIdentity` and `IdentityService::resolve` remain unchanged during the
expansion window. Current phone, WhatsApp, and other channel callers therefore
continue to resolve through `user_identities`. Channel identity is not promoted
to a canonical cross-host identity, and the compatibility path will be removed
only after Core resources and Bridge have migrated.

## Verification and rollback

The isolated PostgreSQL suite verifies stability, cross-deployment,
cross-host-app, cross-organization, and cross-user separation; it also rejects
an organization used under the wrong host app. Existing API tests continue to
exercise the legacy channel payload.

Because the migration is additive, rollback means disabling the new resolution
path while retaining assigned context identifiers. Do not drop or remap
contexts: later migration can safely resume from the retained stable IDs.
