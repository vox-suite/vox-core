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

Host registration and signed assertions expose this context through the
host-trust boundary. Conversation, schedule, and event handlers resolve it once
and pass a `ResourceOwner` into their services.

## Persistence constraints

The additive migration creates `platform_deployments`, `host_apps`,
`host_organizations`, and `user_contexts`.

- Host-app keys are unique inside one deployment.
- Organization keys are unique inside one host app.
- A context's host app must belong to its deployment.
- An optional organization must belong to the same deployment and host app.
- A host user is unique within the complete deployment/host/organization scope.
- Every context owns a distinct internal `users` row.

Every active user-owned runtime row has non-null context ownership. Contact
points are context-scoped delivery metadata and cannot resolve a context.

## No implicit identity authority

There is no channel-identity compatibility path. Matching phone numbers,
WhatsApp senders, names, or voices never create, link, or select a context.
Proof-based identity adapters are the only cross-context linking mechanism.

## Verification and rollback

The isolated PostgreSQL suite verifies stability, cross-deployment,
cross-host-app, cross-organization, and cross-user separation; it also rejects
an organization used under the wrong host app. Existing API tests continue to
exercise the legacy channel payload.

Because this is a pre-launch contraction, legacy databases are rebuilt. Do not
drop or remap canonical contexts in an active canonical database.
