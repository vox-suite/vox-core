# Share to Action delegated access

Core implements the optional connector contract in `../share-to-action/docs/contract.md`.
Configure `SHARE_TO_ACTION_REDIRECT_URI` to the exact backend callback URL and
`GOOGLE_CLIENT_ID` to the Google web client accepted by existing Core ID-token verification.
The callback must use HTTPS (HTTP is accepted only for localhost/127.0.0.1 development).
`SHARE_TO_ACTION_CLIENT_ID` optionally overrides `share_to_action`.
Missing callback configuration disables authorization and token authentication.
Google must allow the Core consent page's origin for browser sign-in.

Browser authorization requires `code_challenge_method=S256`. The consent page uses
Google sign-in, exchanges its ID token at `/v1/auth/exchange`, lists existing collections
and Pulse boards, and lets the user select permissions. No collection or board is
selected initially. Selecting read resources requires both `span_from` and `span_to`,
with a maximum 365-day range. Collections authorize their own spans; selected Pulse
boards authorize their chart aggregates in the selected range. Empty read selections
return empty context, even when a time range is provided.

Authorization codes expire after five minutes. Access tokens expire after one hour;
refresh tokens expire after thirty days and rotate atomically on use. Grants expire
after ninety days or immediately on revocation. Core stores token/code hashes, never
plaintext tokens. Delegated tokens authenticate only the dedicated context, plan and
revoke routes; general consumer routes continue using existing session authentication.

Context responses have at most 30 collections, 300 spans, 10 boards, 20 charts per
board and 100 aggregate points per chart. Span membership IDs are filtered to selected
collections; parent IDs outside returned context are removed.

Plan requests validate selected collection ownership through Core's collection
application service and create spans through the Span application service inside the
idempotency transaction. The key is `(user_id, client_id, request_id)` so reconnecting
cannot blindly duplicate an existing request. Concurrent repeats return the same span
ID; a changed payload with the same request ID is rejected. Span data preserves the
standalone source ID and original URL. Plan creation is not autonomous execution.

Verification:

```sh
cargo test --offline
cargo check --offline --bins
# Use a dedicated disposable pgvector database; all Core migrations are applied.
INTEGRATION_TEST_DATABASE_URL=... cargo test --offline --lib integrations::tests::postgres -- --ignored
INTEGRATION_TEST_DATABASE_URL=... cargo test --offline --bin vox-core-api integration_boundary_tests -- --ignored
```

The database checks exercise foreign collection/board denial, time boundaries, Pulse
aggregation, empty selection, access expiry, concurrent one-time code consumption,
PKCE, refresh rotation, revocation, source provenance, payload mismatch and concurrent
plan idempotency. An HTTP middleware test confirms valid delegated credentials cannot
authenticate general `/v1/spans` routes. Actual Google browser consent and provider
connectivity require configured real credentials and have not been verified locally.
