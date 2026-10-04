# Connections candidate verification

Candidate: 2026-10-04. Connections library commit `61073aa1007870f573d35ebb5285dfe9ee5d98c1` is published on `vox-suite/vox-connections`, branch `codex/complete-connections-20261004`. Core release manifest, lockfile and CI checkout pin this revision. A clean locked check resolves the published Git dependency without a sibling override.

## Local evidence

- Connections: Rust formatting and Clippy pass; 9 unit/provider fixture tests pass. 11 database lifecycle tests pass on disposable PostgreSQL 17. Coverage includes key/AAD behavior, consent, ownership, OAuth replay/denial/expiry/scope reduction, rotation, lease recovery, pause/disconnect during provider reads, outstanding Google reconnect cancellation, and disconnect during verified Sony linking.
- Calendar provider fixtures fetch 1,650 events through pagination, reject failed partial pages, retain recurring-instance identity and cancellation tombstones, and preserve all-day dates through daylight-saving boundaries.
- Core: all Rust targets check; all-target tests pass (17 unit/contract tests). Clippy with all targets/features passes. Five isolated ingestion tests pass: atomic rollback, version changes, local annotation preservation, account isolation, provider-owned field protection including assistant storage writes, retained calendar reconnection deduplication, unknown gaming-session times, duplicate gaming sync and commit-only PostgreSQL `span_updated` notifications.
- Preserved Core database regressions pass individually with appropriate isolation: agent provisioning, ownership transition/action evidence, assistant memory, durable task context, assigned-worker recovery, cancellation/dispatch and specialist delegation/preference revocation. Historical connector-only test fixtures are retired.
- Shared UI: typecheck and WebView production build pass. Lint has no errors and retains one existing theme-component warning. Chrome fixture verification passes consent, validated host browser port, persisted pending setup, reload/focus recovery, independent read/timeline preferences, manual refresh and disconnect at a mobile viewport.
- Desktop: frontend build/lint and Tauri native release build (`--no-bundle`) pass. Existing development-auth constants produce warnings. Desktop now owns its UI sources and has no separate UI package dependency. The current independent frontend build and lint pass.
- Android: debug APK builds with the shared WebView assets and system-browser bridge. App resume explicitly triggers pending-setup polling. Android owns its WebView sources under `web-ui`; Gradle builds and packages them from this same repository with a committed npm lockfile. It has no sibling UI checkout or release-download dependency. Gradle reports existing deprecation warnings.
- Public website: lint and production build pass; only public landing/robots/sitemap routes are built. The consumer application/auth routes were already absent from its current base revision.
- Deployment: full deployment regression suite passes. The standalone Connections service, MCP settings/scripts, obsolete sandbox and consumer-site health callback are retired. Protected Core-only connection configuration preserves the existing key. No manual deployment was triggered; hosted automatic deployment state has not been verified.

## Review and decisions

A fresh source review identified four lifecycle defects. Reproducing tests failed before repairs for disconnect/reconnect cancellation, preference changes during refresh rotation, and same-account calendar duplicates; all pass after repairs. Transient provider errors now preserve retryable authorization. Decreased Sony counters retain their high-water value and the wider observation window.

Historical connector/grant records are retained as `retired_*` relations to preserve Core action evidence and foreign keys; runtime credentials, packages, onboarding and invocation paths are removed. Retired grants do not authorize the curated connection service. Destroying historical audit relations would also destroy unrelated evidence, so this is intentionally a forward retirement.

## Mandatory remaining release gates

1. Publication and locked dependency resolution are verified. The standalone UI repository is retired; desktop and Android own their UI sources.
2. Deploy the coordinated Core/native candidate with forward migrations and protected Core configuration. Validate production health and worker scheduling separately from source checks.
3. Authorize real Google Calendar and PlayStation accounts on desktop and Android; verify linking, resume/reconnect, token refresh, independent preferences, bounded assistant reads and live timeline updates. Fixture tests and native builds are not live-provider or device acceptance evidence.

Steam, Valorant, Amazon shopper data and Zomato tracking are outside this release. Existing map, voice and visual behavior is preserved.
