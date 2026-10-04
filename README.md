# Vox Core

Vox Core owns conversations, agents, durable events, schedules, summaries, user context, jobs, and autonomous actions. It runs as two processes from one image:

- `vox-core-api` handles authenticated Bridge and client requests.
- `vox-core-worker` leases durable jobs, advances schedules, summarizes completed conversations, and dispatches actions.

PostgreSQL is authoritative. Set `DATABASE_URL` to the Supabase PostgreSQL connection string. Redis stores only minimal per-user records (`vox:user:{id}` JSON with name + channels, plus `vox:channel:{channel}:{external_id}` indexes) and runs with AOF persistence in Compose. Kafka is not required for this deployment because PostgreSQL provides the durable job queue and leases.

## Run the stack

Keep `vox-core` and `vox-bridge` as sibling directories. Copy `../vox-edge/.env.example` (the single reference for every variable core and bridge read) to `.env`, replace every placeholder, then run:

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
VOX_ENV_FILE=../vox-edge/.env.example docker compose --env-file ../vox-edge/.env.example config
```

Platform V1 authority and outcome semantics are executable without a database or
production provider:

```sh
cargo test --locked
```

This release supports Google Calendar and PlayStation connections. Steam, Valorant, Amazon shopper data and Zomato tracking remain outside this release. The older [provider research](docs/provider-feasibility.md) is historical and does not describe shipped functionality.

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

Conversation, schedule, task, and action ownership migration and compatibility
rules are documented in
[`docs/resource-context-migration.md`](docs/resource-context-migration.md).

Database integration tests require an isolated PostgreSQL database and `TEST_DATABASE_URL`; run them serially with `--test-threads=1`.

## Connected Apps

Google Calendar and PlayStation linking use the authenticated `/v1/me/connectors` and `/v1/me/connections` API shared by desktop and Android. Connections owns encrypted tokens, provider reads and checkpoints; Core owns scheduling and transactional timeline ingestion. See [the connection contract](docs/connections.md).

## Deploy on Railway

Deploy this repo as two services, both with an empty root directory:

| Service | Dockerfile | Notes |
|---|---|---|
| `vox-core-api` | `Dockerfile` | Runs the migration baseline on boot. Binds `0.0.0.0:$PORT` (Railway injects `PORT`). |
| `vox-core-worker` | `Dockerfile.worker` (set `RAILWAY_DOCKERFILE_PATH=Dockerfile.worker`) | No port. Needs the same database, Redis and token variables as the API. |

Shared variables (set once as Railway shared variables):

- `DATABASE_URL`: the Supabase **direct** connection (`db.<ref>.supabase.co:5432`). Do not use the transaction pooler: sqlx uses named prepared statements, which collide there. The session pooler works but caps you at 15 clients. The direct host is IPv6-only, so run `railway outbound-network ipv6 enable --service <name>` for both `vox-core-api` and `vox-core-worker` and apply the staged change.
- `REDIS_URL`
- `VOX_CREDENTIAL_KEY`: the same 64-character hexadecimal encryption key on API and Worker for linked provider credentials.
- `VOX_AUTH_TOKEN`: the same value on `vox-core-api`, `vox-core-worker` and `vox-bridge`.
- `VOX_BRIDGE_URL`: `http://vox-bridge.railway.internal:<bridge PORT>`
- `GEMINI_API_KEY`, `EXA_API_KEY`, `GOOGLE_MAPS_API_KEY`, `VOX_ADMIN_TOKEN`

Only the edge (`vox-edge`) has a public domain. It routes `/v1/*` to `vox-core-api`. See `vox-edge/ROUTING.md` for the path protocol.

`vox-connections` and `vox-shared` are fetched as public git dependencies, so no sibling checkout is needed.

### Agent memory isolation

Model memory and `get_agent_memory` / `update_agent_memory` are scoped to the authenticated host user context and the selected owned agent. Agent facts are bounded to 8 KiB; the projection is bounded to 16 KiB and includes only that agent’s summaries. Current actor/template availability is checked on each read and mutation; no user-wide projection cache can survive archive. Summarization retains facts in the originating conversation and does not publish them to the global identity profile. Schedules without an assigned agent receive no agent memory. A voice identity change cannot silently replace the authenticated host context.

Signed hosts can inspect projected memory, clear it, or toggle retention through `/v1/agents/{agent_key}/memory`. Clear removes facts and starts a new summary retention window; conversation and action history remain intact. Disabled retention blocks fact writes and omits retained summaries; re-enabling never resurfaces summaries from the disabled period. Identical retention-setting retries preserve new facts.

This implements memory isolation and controls, not complete W7: explicit saved-preference sharing, scoped delegation and the remaining native-tool consolidation still require delivery. Identity name/channel routing remains separate from model memory.
