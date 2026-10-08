# Connector platform recovery

Connections recovery: https://github.com/vox-suite/vox-connections/issues/29. Core recovery: https://github.com/vox-suite/vox-core/issues/121. Host UI release blocker: https://github.com/vox-suite/vox-web/issues/27.

Core depends on Connections corrective commit `f15b89c82dd5b60fa7f3ad399a2652ac31c92e63`. The complete file/API inventory and detailed evidence live in Connections’ `docs/recovery-inventory.md`, `docs/recovery-inventory.json` and `docs/recovery-verification.md`. The inventory covers the original five removal/provider commits, all seven subsequent Connections changes through main `99458ac` plus Maps `f61edd9`, and Core changes through `ba13964`. Shared Git history is preserved.

Reusable skills, defaults, grant storage, protocol transports and provider types are delegated to Connections. Core retains authenticated contexts, policy, approval orchestration, scheduled sync, timeline ingestion, realtime delivery, WiZ, map tools, JWKS caching and the branded OAuth callback page. Known-user authentication skips signup only after matching the exact first-party context; other same-user host subjects do not qualify.

## Verification

All-target unit/HTTP contract tests, formatting and warning-free Clippy pass. Database checks run with PostgreSQL 18/pgvector and isolated fixtures: 11 ingestion tests, 6 migration tests, 6 ownership/memory/worker/delegation regressions plus the isolated ownership-transition test. The migration suite uses production SQLx ledgers and checksums. Library tests compile an independent host and exercise restored grants, setup consent, packages, HMAC auth, providers and schema drift.

Both old and curated upgrade paths preserve surviving connection/history IDs and credentials. The additive safeguard is dated before the destructive retirement migration so old platform deployments can preserve state without changing historical checksums; already-retired deployments cannot recover erased rows or regain revoked authority. Migration tests verify both paths and repeated sync without duplicate imported history. Stop writers and back up the database before the coordinated migration.

Curated operations require a verified context. Agent reads additionally require an explicit selected-agent grant and enabled account preference. Native payloads never accept caller-supplied ownership. An authenticated `/v1/me/connections/{id}/reassociate` endpoint associates only unscoped accounts owned by the authenticated user; it never grants agent access. Ambiguous accounts are excluded from scheduled sync and reads until explicitly reassociated.

## PlayStation decision

Retain provider first/last timestamps, game covers, cumulative counter deltas and 30-minute sync. Display first/last markers carrying an explicitly labelled observed range. Counter deltas may have separately labelled estimated placement near a recent provider last-played timestamp. Do not create evenly-spaced synthetic sessions from aggregate totals or delete first/last history: those newest upstream changes conflict with the approved range semantics and are superseded in this corrective branch.

## Release gates

Merge and make the Connections commit available before this consuming Core PR. Host UI work in https://github.com/vox-suite/vox-web/issues/27 must expose explicit grants and legacy account reassociation. Real linking, callback allowlisting and production provider validation remain separate deployment checks. Swiggy is default enabled when configured; Zomato OAuth remains opt-in. Local verification does not claim release readiness.

## Latest upstream reconciliation

Keep immediate PlayStation first sync, sync failure logs and consented Maps import; retain new native finance records, goal previews and collection-scoped day pagination. Matching email alone cannot join identities. Explicit proven identity linking keeps host contexts separate and retains credential encryption provenance through token rotation. Maps upgrades preserve span/event IDs and annotations and repeated import remains idempotent.

Native span, legacy chart and Pulse reads isolate the exact native context and honor account preferences; generic external connection history remains owner-readable. Agent timeline/chart/goal access requires its own effective grants; chart and goal generation rechecks them after model completion. Catalogue authorization uses one account query plus one shared effective-grants query per context per pass. Composite spending remains owner-only until execution can bind contributing connections. The legacy Expedia cancellation interface fails closed because it cannot bind an approval to the exact cancellation penalty.

New goals preserve context ownership through an additive migration. Ambiguous existing goals and entries survive without inferred authority; explicit reassociation is a release gate tracked in https://github.com/vox-suite/vox-core/issues/121 alongside https://github.com/vox-suite/vox-web/issues/27.

Performance fixture with PostgreSQL18/pgvector, six charts and 100,000 records: cold5 queries/2,275.82ms, cached1 query/p95 0.82ms, forced refresh5 queries/2,140.63ms. At 1,000 records, cold30.36ms and cached p95 1.16ms. These local results do not guarantee production latency.
