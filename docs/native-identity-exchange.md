# Native provider identity exchange

`POST /v1/auth/exchange_token` verifies the provider token before mapping an
issuer/subject to a Vox account and minting an opaque Vox session. Supabase
consumer identity uses the same immutable session pin as Web's consumer adapter.

## Supabase authority

Configure `SUPABASE_URL` as the project root, for example
`https://project.supabase.co`. Core derives the exact issuer
`https://project.supabase.co/auth/v1`, JWKS path and fresh user endpoint from it.
Do not put `/auth/v1` in `SUPABASE_URL`: Supabase's self-hosted
[`API_EXTERNAL_URL` setting](https://supabase.com/changelog/47093-self-hosted-supabase-api-external-url-to-include-auth-v1)
has a different meaning and now includes that prefix.

ES256/RS256 signatures use this project's public JWKS. HS256 requires the
configured project `SUPABASE_JWT_SECRET`. All Supabase paths require:

- Valid signature and unexpired token, with any `nbf` already satisfied.
- Exact configured issuer; audience and role both `authenticated`;
  `is_anonymous: false`; valid UUID user and session IDs.
- A signed top-level `vox_identity` pin, version 1, whose user/session IDs match
  the verified token and whose identity/provider match the fresh user response.
- Exactly one current provider identity belonging to that same user, and a
  nonanonymous fresh user. Google requires an `oauth` authentication method;
  email requires `otp`, `magiclink` or `email/signup`. Additional `token_refresh`
  and `totp` methods are allowed; unrelated or malformed methods are denied.

A fresh `/auth/v1/user` request uses the presented token and
`SUPABASE_PUBLISHABLE_KEY` (or the existing `SUPABASE_ANON_KEY`). No service-role
key is required. The request fails closed on missing configuration, provider
failure, malformed/oversized response or redirect. It is bounded to five seconds
and one MiB. Display name/email may come from this fresh user but are not identity
or authorization proof. User-editable metadata cannot supply the pin.

The subject is `supabase:<provider>:<identity_id>`, matching Web's consumer account
identifier, rather than Supabase's merged user UUID. Replacement or multiple
identities fail closed for the existing pinned session. There is no email-match,
merged-user or unpinned-session recovery fallback. This prelaunch transition does
not migrate accounts previously identified by the merged UUID.

Use the exchange-issued opaque Vox token for subsequent native API requests.
The raw-provider JWT middleware expects a UUID subject and cannot consume this
immutable provider subject; bypassing exchange is unsupported. Web's signed
host-context path remains a separate public interface with its own context
isolation and access checks. Matching identifier syntax alone does not merge
accounts across host apps.

## Provider setup and release gate

The private pin table and custom access-token hook are defined in
[Web's provider migration](https://github.com/vox-suite/vox-web/blob/2aa14498dea4181d7fefb952698b81164121ff50/supabase/migrations/20261001174921_consumer_session_identity_pins.sql).
Apply it to the configured Supabase Auth project and register
`vox_auth.custom_access_token_hook` using the authorized provider setup workflow.
It runs before issuance, creates immutable pins only on allowed fresh sign-ins,
and cannot bootstrap an unpinned session on refresh. Require fresh sign-in after
configuration. This Core code change does not install/register the hook, provision
keys, modify provider accounts or claim live Google/OTP availability.

Before releasing native Supabase exchange, verify live hook configuration,
Google and OTP issuance, refresh continuity, identity replacement and same-email
link denial, provider errors and exchange-issued Vox-session access. Previously
issued access tokens are not guaranteed to become invalid immediately after
sign-out; Supabase owns token/session lifetime. The fresh user check establishes
current identity compatibility, not instantaneous revocation of every bearer token.
Already minted opaque Vox sessions retain Core's existing lifetime/revocation
semantics; no per-request provider-session linkage was added here.

Direct Google ID tokens retain their Google public-key signature check, allowed
Google issuer and exact `GOOGLE_OAUTH_CLIENT_ID` audience check. They do not use
the Supabase hook or merged Supabase user records.

## Verification

```sh
cargo test --bin vox-core-api identity_token -- --nocapture
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The tests use locally generated/signable keys and a mock fresh-user endpoint:
unsigned/forged/tampered pins; signed wrong issuer/audience/role/anonymous claims;
EC signatures; exact session binding; invalid authentication methods; replaced or
mixed identities; refresh continuity; and redirects. They do not contact a live
provider or certify deployed hook configuration.

Primary sources checked: [custom token hook contract](https://supabase.com/docs/guides/auth/auth-hooks/custom-access-token-hook),
[JWT verification](https://supabase.com/docs/guides/auth/jwts),
[fresh user verification](https://supabase.com/docs/reference/javascript/auth-getuser),
and the current [Supabase changelog](https://supabase.com/changelog).
