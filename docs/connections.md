# Connected Apps

Core authenticates native clients and schedules connections. Vox Connections owns account linking, encrypted credentials, OAuth state and PKCE, token rotation, preferences, provider reads and checkpoints. Core implements transactional timeline ingestion and forwards committed PostgreSQL notifications as `span_updated` client events.

Authenticated POST APIs: `/v1/me/connectors/list`, `/v1/me/connections/list`, `/start`, `/setup/{id}/status`, `/setup/{id}/cancel`, `/{id}/preferences`, `/{id}/refresh`, `/{id}/disconnect`. Start takes `connector_id`, `consent: true` and, for PlayStation, `npsso`. OAuth opens the configured Core Google callback in the system browser. Setup IDs persist in the native UI and recover by authenticated polling when the app resumes. Never send a client callback URI.

Configure a 32-byte hexadecimal `VOX_CREDENTIAL_KEY`, Google OAuth credentials and `VOX_CORE_API_URL`. Missing encryption configuration disables connections; invalid keys fail startup. Existing unconsented connections require reconnecting. Timeline and assistant reads can be paused independently. Disconnect removes credentials but retains spans. Calendar title/time/status are managed by Google; notes and collections remain local.

Retired connector runtime and credential tables are removed through forward migrations. Historical connection and grant relations are named `retired_*` to preserve action evidence and its foreign keys. They confer no new access. Skills, scoped identity and approval contracts now belong to Core.

Required release gates: isolated database and provider-fixture regressions, native builds, Google and PlayStation linking and sync with real accounts on desktop and Android, resume/reconnect, independent preference enforcement, assistant reads and live timeline updates. Local tests, release publication, deployment and live provider validation are separate evidence.
