# Host-app trust

Platform V1 treats a host app as a distinct principal from the person using the
host app. A host may assert a stable local user identifier only after it proves
control of a registered host credential. A request body, email address, phone
number, or matching host-user identifier is not evidence of that control.

## Deep module interface

`HostTrustService` is the one module callers use to manage this relationship:

- `register_host_app` creates or updates a deployment/host registration and
  issues a signing credential;
- `rotate_credential` creates an overlapping replacement without changing the
  deployment or host-app identity;
- `revoke_credential` disables one credential immediately; and
- `resolve_authenticated_context` verifies one signed assertion and maps it to
  the credential's canonical user context.

The module owns credential verification, short assertion lifetime, in-process
replay prevention, audience validation, browser-origin policy, organization
lookup, and context resolution. HTTP handlers do not accept deployment IDs,
host-app IDs, organization IDs, or user-context IDs from an untrusted payload.

## Operator workflow

The platform operator calls `POST /v1/host-apps` with the platform bootstrap
credential (`VOX_AUTH_TOKEN`). The response includes the host-app ID,
audience, credential ID, and raw signing secret. The secret is returned only
in that response; Core stores only its SHA-256 verifier. The operator places
the secret in the host app's server-side secret manager. Credential-bearing
types intentionally do not implement debug formatting, and Core never logs
request headers or registration responses. A browser must never receive this
secret; browser-origin policy is an additional check, not a substitute for
server-side credential custody.

Use `POST /v1/host-apps/{host_app_id}/credentials` to issue a replacement.
Both the old and new credentials work during a deliberate rollout overlap.
After the host has moved, call `DELETE /v1/host-app-credentials/{credential_id}`
with the bootstrap credential to revoke the old one. Credential rotation never
changes the host app's ID, its deployment, or a user's existing context.

`allowed_origins` is an exact allowlist of normalized HTTP(S) origins. Browser
requests bearing an `Origin` header must match it. Server-to-server hosts may
omit `Origin`; they still require a valid signed assertion.

## Signed context exchange

The host calls `POST /v1/host/context` with a body containing only:

```json
{
  "host_user_id": "stable-host-user-id",
  "organization_external_key": "optional-host-local-org-key"
}
```

It supplies these headers over TLS:

- `X-Vox-Host-Credential`: credential UUID
- `X-Vox-Host-Secret`: the operator-issued secret
- `X-Vox-Host-Audience`: `vox-host:{deployment-key}:{host-app-key}`
- `X-Vox-Host-Timestamp`: Unix seconds
- `X-Vox-Host-Nonce`: fresh UUID for this request
- `X-Vox-Host-Signature`: lowercase hexadecimal HMAC-SHA-256 signature

The signature covers the protocol version, credential ID, audience, timestamp,
nonce, host user ID, and organization key using length-prefixed fields. It is
therefore unambiguous even when identifiers contain punctuation. Assertions
expire after five minutes (with at most 60 seconds of future clock skew) and a
nonce can succeed only once within a single API process. Core keeps only
credential ID, nonce, and expiry in memory for that window; it never stores the
assertion payload or secret.

The authenticated credential decides deployment and host app. If an
organization key is supplied, it is resolved only inside that authenticated
scope. The result returns a `user_context_id`; Core creates the stable context
on first use. A host cannot move an assertion to another deployment, host app,
organization, or user context by changing JSON fields or headers. In
particular, changing a signed `host_user_id` without the host credential is
rejected instead of creating a context for an attacker-selected identifier.

## Failure and recovery behavior

Malformed, forged, wrong-audience, expired, replayed, revoked, unknown-host,
and origin-policy failures return the same HTTP `401` response to avoid
credential or tenant enumeration. Invalid host-user input returns `400` only
after a valid host assertion has been supplied. A host retries a rejected or
expired assertion with a new timestamp and nonce; it must never reuse a nonce.

If a credential is suspected compromised, revoke it immediately. Existing user
contexts and durable resources remain intact. Registering a replacement and
issuing fresh assertions restores the host without remapping domain identity.

## Compatibility and limits

Existing channel callers remain on the temporary legacy path documented in
[`user-context.md`](user-context.md) and
[`resource-context-migration.md`](resource-context-migration.md). They are not
implicitly upgraded by this protocol. Bridge migration is the next consumer of
this accepted host-trust interface.

This protocol authenticates the host application's assertion. Replaceable
identity adapters and proof-based identity links build on that assertion as a
separate contract; they never infer a link from host identity alone. Service
access and action approvals remain separate Core contracts.

## Verification evidence

On 2026-09-20, the migration was rehearsed against PostgreSQL 17 containing
an existing deployment, host app, and canonical user context. The upgrade
preserved the host and context, assigned the empty origin policy to the
pre-existing host, and created no credentials or nonce records implicitly.

The isolated integration suite covers authenticated registration, signed
context resolution, tampered host-user data, forged secrets, wrong audience,
expired assertions, concurrent replay, origin mismatch, cross-host
organization injection, credential overlap, revocation, and preservation of
the resolved context through rotation. The existing canonical-context and
resource-ownership suites pass against the same migrated schema.
