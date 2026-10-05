# Connector platform recovery

Connections recovery: https://github.com/vox-suite/vox-connections/issues/29. Core recovery: https://github.com/vox-suite/vox-core/issues/121. Host UI release blocker: https://github.com/vox-suite/vox-web/issues/27.

Core depends on Connections corrective commit `0b4d1397a7af1aefa2519de92364f7f492c7c295`. The complete file/API inventory and detailed evidence live in Connections’ `docs/recovery-inventory.md`, `docs/recovery-inventory.json` and `docs/recovery-verification.md`. The inventory covers the original five removal/provider commits, all four subsequent Connections changes through `6f43efb`, and Core changes through `658387e`. Shared Git history is preserved.

Reusable skills, defaults, grant storage, protocol transports and provider types are delegated to Connections. Core retains authenticated contexts, policy, approval orchestration, scheduled sync, timeline ingestion, realtime delivery, WiZ, map tools, JWKS caching and the branded OAuth callback page. Known-user authentication skips signup only after matching the exact first-party context; other same-user host subjects do not qualify.

## Verification

All-target unit/HTTP contract tests, formatting and warning-free Clippy pass. Database checks run with PostgreSQL 18/pgvector and isolated fixtures: 11 ingestion tests, 3 migration tests, 6 ownership/memory/worker/delegation regressions plus the isolated ownership-transition test. The migration suite uses production SQLx ledgers and checksums. Library tests compile an independent host and exercise restored grants, setup consent, packages, HMAC auth, providers and schema drift.

Both old and curated upgrade paths preserve surviving connection/history IDs and credentials. The additive safeguard is dated before the destructive retirement migration so old platform deployments can preserve state without changing historical checksums; already-retired deployments cannot recover erased rows or regain revoked authority. Migration tests verify both paths and repeated sync without duplicate imported history. Stop writers and back up the database before the coordinated migration.

Curated operations require a verified context. Agent reads additionally require an explicit selected-agent grant and enabled account preference. Native payloads never accept caller-supplied ownership. An authenticated `/v1/me/connections/{id}/reassociate` endpoint associates only unscoped accounts owned by the authenticated user; it never grants agent access. Ambiguous accounts are excluded from scheduled sync and reads until explicitly reassociated.

## PlayStation decision

Retain provider first/last timestamps, game covers, cumulative counter deltas and 30-minute sync. Display first/last markers carrying an explicitly labelled observed range. Counter deltas may have separately labelled estimated placement near a recent provider last-played timestamp. Do not create evenly-spaced synthetic sessions from aggregate totals or delete first/last history: those newest upstream changes conflict with the approved range semantics and are superseded in this corrective branch.

## Release gates

Merge and make the Connections commit available before this consuming Core PR. Host UI work in https://github.com/vox-suite/vox-web/issues/27 must expose explicit grants and legacy account reassociation. Real linking, callback allowlisting and production provider validation remain separate deployment checks. Swiggy is default enabled when configured; Zomato OAuth remains opt-in. Local verification does not claim release readiness.
