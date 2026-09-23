# Platform schema recovery after the consumer baseline rewrite

Core `main` replaced its migration chain with
`20260923000000_initial_core.sql`. That fresh baseline creates 21 consumer
tables but omits the deployment, host-context, agent, integration, connection,
and grant tables still used by the Platform V1 services. This change adds the
20 omitted tables from the accepted migrations at
`6f4d2636deeb608a9c00f15fbe355b9ab63d7b0b` as an additive migration.
The consumer baseline is left byte-for-byte intact.

The current `main` also keeps host credentials and replay nonces in process
memory and resolves user contexts through `auth_identities` without an
organization key. This branch restores the accepted database-backed host
credential hashes, durable replay nonces, registered deployment/host scopes,
and context records. Raw host secrets are returned once at registration and
are not stored in the database. The Google Secret Manager credential snapshot
path is removed from the API startup; deployments relying on that snapshot
need a tested migration before this branch is merged.

`ConnectionService` and `CapabilityGrantService` still use the consumer
`connections` table rather than the context-scoped `external_connections` and
`agent_capability_grants` tables. Context ownership on conversations, tasks,
and other consumer resources is also missing. Those behaviors need contract
repair and security tests before this branch can be merged.

An existing deployment with the prior migration chain is **not** upgraded by
this additive migration alone. The consolidated baseline has a new version
and overlapping table names. Do not run it against a deployed database until
a tested data-preserving upgrade path and rollback procedure are published.

Verification on a fresh isolated PostgreSQL database:

- `cargo test --locked --test migration_contract -- --ignored --test-threads=1`
  passes and finds 41 application tables (21 consumer and 20 platform).
- `cargo test --locked --test migration_checksums` passes.
- The isolated `user_context`, `host_trust`, `agent_registry`, and
  `integration_registry` suites pass. The host trust suite includes a
  cross-instance replay and revocation check.
- The isolated `capability_grants` suite still fails because a grant is not
  recorded or evaluated per agent.

The remaining failures require revalidation of E05/E07/E09/E13/E16 and their
consumers. A compiling binary is not evidence that these contracts work with
the schema it installs.
