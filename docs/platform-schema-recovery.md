# Platform schema recovery after the consumer baseline rewrite

Core `main` replaced its migration chain with
`20260923000000_initial_core.sql`. That fresh baseline creates 21 consumer
tables but omits the deployment, host-context, agent, integration, connection,
and grant tables still used by the Platform V1 services. This change adds the
20 omitted tables from the accepted migrations at
`6f4d2636deeb608a9c00f15fbe355b9ab63d7b0b` as an additive migration.
The consumer baseline is left byte-for-byte intact.

The migration is only a first repair. Current `HostTrustService` creates host
credentials in process memory and does not persist deployment or host rows, so
agent and integration registry calls still return `NotFound`. Current
`ConnectionService` and `CapabilityGrantService` use the consumer `connections`
table rather than the context-scoped `external_connections` and
`agent_capability_grants` tables. Context ownership on conversations, tasks,
and other consumer resources is also missing. Those behaviors need their own
contract repair and security tests before this branch can be merged.

An existing deployment with the prior migration chain is **not** upgraded by
this additive migration alone. The consolidated baseline has a new version
and overlapping table names. Do not run it against a deployed database until
a tested data-preserving upgrade path and rollback procedure are published.

Verification on a fresh isolated PostgreSQL database:

- `cargo test --locked --test migration_contract -- --ignored --test-threads=1`
  passes and finds 41 application tables (21 consumer and 20 platform).
- `cargo test --locked --test migration_checksums` passes.
- `cargo test --locked --test agent_registry -- --ignored --test-threads=1`
  still fails because host registration does not persist its deployment.

These failures require revalidation of the E04/E05/E06/E08/E09/E13/E16
producer contracts. A compiling binary is not evidence that these contracts
work with the schema it installs.
