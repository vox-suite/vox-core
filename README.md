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

Database integration tests require an isolated PostgreSQL database and `TEST_DATABASE_URL`; run them serially with `--test-threads=1`.
