# Vox Core

Vox Core owns conversations, agents, durable events, schedules, summaries, user context, jobs, and autonomous actions. It runs as two processes from one image:

- `vox-core-api` handles authenticated Bridge and client requests.
- `vox-core-worker` leases durable jobs, advances schedules, summarizes completed conversations, and dispatches actions.

PostgreSQL is authoritative. Set `DATABASE_URL` to the Supabase PostgreSQL connection string. Redis stores only rebuildable user-context projections and runs with AOF persistence in Compose. Kafka is not required for this deployment because PostgreSQL provides the durable job queue and leases.

## Run the stack

Keep `vox-core` and `vox-bridge` as sibling directories. Copy `.env.example` to `.env`, replace every placeholder, then run:

```sh
docker compose up --build -d
```

Only Bridge port `3000` is published. Core API, Core Worker, and Redis remain on the private Compose network. Point the public reverse proxy at Bridge and configure Twilio with the public URLs documented in the Bridge repository.

## Verify

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
VOX_ENV_FILE=.env.example docker compose --env-file .env.example config
```

Platform V1 authority and outcome semantics are executable without a database or
production provider:

```sh
cargo test --locked --test platform_conformance
```

See [`docs/conformance.md`](docs/conformance.md) for the versioned fixture,
adapter contract, stable error vocabulary, and semantic coverage map.

Provider integrations must follow the dated capability decisions and production
enablement gates in
[`docs/provider-feasibility.md`](docs/provider-feasibility.md). The record
selects the first connected-read and consequential-write routes, distinguishes
verified provider behavior from inference, and requires unsupported actions to
remain labelled handoffs.

The canonical deployment, host-app, optional organization, and host-user
isolation contract is documented in
[`docs/user-context.md`](docs/user-context.md). Existing channel identity remains
available during the migration window.

Conversation, schedule, task, and action ownership migration and compatibility
rules are documented in
[`docs/resource-context-migration.md`](docs/resource-context-migration.md).

Database integration tests require an isolated PostgreSQL database and `TEST_DATABASE_URL`; run them serially with `--test-threads=1`.

## Read-only Redis administration

`GET /v1/admin/redis` is enabled when `VOX_ADMIN_TOKEN` is set. This is a dedicated admin credential, separate from `VOX_CORE_SERVICE_TOKEN`. Requests without it are denied, including when admin is unconfigured. The browser never connects to this endpoint directly: Vox Web checks the Google session and exact superuser allowlist before forwarding a request from its server.

Query parameters:

- `match`: Redis glob pattern, default `vox:*`, maximum 256 bytes.
- `cursor`: unsigned 64-bit SCAN cursor represented as a string; default `0`.
- `key`: inspect one UTF-8 key, maximum 1,024 bytes, without control characters. If provided, returns an atomic bounded value preview and metadata instead of a key listing.

Redis 7+ is required for `EVAL_RO`. The existing Redis 8 service is compatible. SCAN uses a count hint of 100. Connections are reused and reconnect; admin reads have a four-second deadline and at most eight concurrent requests per API instance. String previews stop at 64 KiB. Collection string data has a total 64 KiB budget and 2 KiB per-value limit. No write methods or arbitrary commands are exposed.

Keep Redis private. Configure the HTTPS reverse proxy to route only this exact path to the Core API and omit query strings from access logs. Deployment instructions live in the sibling `vox-web/docs/deployment.md`.

```sh
cargo test --test admin_redis
TEST_REDIS_URL=redis://127.0.0.1:16379 cargo test --test admin_redis -- --include-ignored
```

The second command must target an isolated Redis instance. The test creates uniquely prefixed fixture keys and deletes those keys afterward.
