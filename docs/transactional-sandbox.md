# Transactional conformance sandbox

`TransactionalSandbox` is test infrastructure. It is an in-memory, network-free
`ExecutionAdapter`; it is not registered in integration discovery, cannot receive
credentials, and is compiled only when the explicit `sandbox` Cargo feature is
enabled (or for Core unit tests). It is never a user-facing integration.

| Scenario | Dispatch outcome | Reconciliation outcome |
| --- | --- | --- |
| success | confirmed synthetic receipt | unchanged/unknown |
| rejection | failed | unchanged/unknown |
| price changed | `sandbox_fresh_proposal_required`; Core rejects the changed immutable price and requires a fresh proposal before dispatch | unchanged/unknown |
| provider authentication | awaiting provider authentication | unknown |
| timeout, crash-after-dispatch, unknown | unknown | unknown until provider evidence arrives |
| duplicate delivery | the same synthetic provider reference and receipt | unchanged/unknown |
| cancellation | cancelled with synthetic cancellation evidence | terminal |
| refund | confirmed original receipt; refund is separate evidence | unchanged/unknown |
| reconcile success | reconciling | confirmed synthetic receipt |
| reconcile failure | reconciling | failed |

Seeds are caller idempotency keys. Snapshotting and recreating a sandbox restores
its scripted behavior and provider-idempotency identity without an external side
effect. A refund is deliberately an ancillary provider observation; it never
rewrites the original execution outcome.

Run the focused deterministic checks with:

```sh
cargo test --locked --features sandbox sandbox::tests
```

Run the authenticated PostgreSQL acceptance checks with a disposable database:

```sh
TEST_DATABASE_URL=postgres://... cargo test --locked --features sandbox --test transactional_sandbox -- --ignored
```

The PostgreSQL tests use the same public Core services as hosts: authenticated
context resolution, connection/grant setup, proposal, approval, coordinator
start, dispatch, reconciliation, and durable status retrieval. Exact-price,
grant, connection, approval-expiry, and quota denials remain Core-policy checks;
the sandbox supplies only normalized synthetic provider observations.
