# Vox Core

Vox Core owns conversations, agents, durable events, schedules, summaries, user context, jobs, and autonomous actions. It runs as two processes from one image:

- `vox-core-api` handles authenticated Bridge and client requests.
- `vox-core-worker` leases durable jobs, advances schedules, summarizes completed conversations, and dispatches actions.

PostgreSQL is authoritative. Set `DATABASE_URL` to the Supabase PostgreSQL connection string. Redis stores only minimal per-user records (`vox:user:{id}` JSON with name + channels, plus `vox:channel:{channel}:{external_id}` indexes) and runs with AOF persistence in Compose. Kafka is not required for this deployment because PostgreSQL provides the durable job queue and leases.

## Run the stack

Keep `vox-core` and `vox-bridge` as sibling directories. Copy `.env.example` to `.env`, replace every placeholder, then run:

```sh
docker compose up --build -d
```

Only Bridge port `3000` is published. Core API, Core Worker, and Redis remain on the private Compose network. Point the public reverse proxy at Bridge and configure Twilio with the public URLs documented in the Bridge repository.

## Phone greeting cache

The streaming inbound opening resolves the caller through Redis
`vox:channel:{channel}:{external_id}` → `vox:user:{user_id}` (minimal JSON with
`name` and `channels`). It has a 100 ms cache deadline and uses a generic
new-caller greeting on a miss, blank name, cache error, or timeout. PostgreSQL
and the LLM are not consulted before returning this greeting. The full LLM
context projection is always built from PostgreSQL and is not stored in Redis.

API startup (and the optional worker sync loop) replaces the complete minimal
user snapshot from PostgreSQL. Name and channel changes become visible after
the next write/sync. Deploy/restart Core API for this change.

`CORE_CACHED_GREETING_METRICS` measures the greeting path;
`CORE_OPENING_INITIALIZED` measures the background database work.

## Verify

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
VOX_ENV_FILE=.env.example docker compose --env-file .env.example config
```

Platform V1 authority and outcome semantics are executable without a database or
production provider:

```sh
cargo test --locked
```

The shared `vox-connections` crate runs the canonical conformance fixture with `cargo test --test host_boundary`. See [`docs/conformance.md`](docs/conformance.md) for the versioned fixture,
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

Authenticated host registration, signed context assertions, replay protection,
origin policy, and credential rotation are documented in
[`docs/host-trust.md`](docs/host-trust.md). Host credentials never enter agent
context or persistent Core records.

Replaceable federated and passwordless identity adapters, one-time proof-based
identity linking, and the non-merging context guarantee are documented in
[`docs/identity-adapters.md`](docs/identity-adapters.md).

The model-neutral agent catalog and its explicit no-authority guarantee are
documented in [`docs/agent-registry.md`](docs/agent-registry.md).

Protocol-neutral integration declarations and discovery semantics are documented
in [`docs/integration-registry.md`](docs/integration-registry.md).

Conversation, schedule, task, and action ownership migration and compatibility
rules are documented in
[`docs/resource-context-migration.md`](docs/resource-context-migration.md).

Database integration tests require an isolated PostgreSQL database and `TEST_DATABASE_URL`; run them serially with `--test-threads=1`.

## Connector boundary

Reusable integration declarations, provider transports, remote extension adapters, connected-app OAuth and MCP sessions, declarative skill packages, and conformance fixtures live in [vox-connections](https://github.com/vox-suite/vox-connections). Core owns host trust, identity, agent presentation, grants and approval enforcement, durable execution, and audit. It passes a minimal request context and database pool to the shared crate. The remaining `connected_apps`, `remote_extensions`, `skills`, and `conformance` modules here are host wiring or API re-exports.


## Deploy on Railway

Deploy this repo as two services, both with an empty root directory:

| Service | Dockerfile | Notes |
|---|---|---|
| `vox-core-api` | `Dockerfile` | Runs the migration baseline on boot. Binds `0.0.0.0:$PORT` (Railway injects `PORT`). |
| `vox-core-worker` | `Dockerfile.worker` (set `RAILWAY_DOCKERFILE_PATH=Dockerfile.worker`) | No port. Needs the same database, Redis and token variables as the API. |

Shared variables (set once as Railway shared variables):

- `DATABASE_URL`: the Supabase transaction pooler (port 6543, `?sslmode=require`). The statement cache is disabled for it.
- `REDIS_URL`
- `VOX_AUTH_TOKEN`: the same value on `vox-core-api`, `vox-core-worker` and `vox-bridge`.
- `VOX_BRIDGE_URL`: `http://vox-bridge.railway.internal:<bridge PORT>`
- `GEMINI_API_KEY`, `EXA_API_KEY`, `GOOGLE_MAPS_API_KEY`, `VOX_ADMIN_TOKEN`

Only the edge (`vox-edge`) has a public domain. It routes `/v1/*` to `vox-core-api`. See `vox-edge/ROUTING.md` for the path protocol.

`vox-connections` and `vox-shared` are fetched as public git dependencies, so no sibling checkout is needed.
