# Platform semantic conformance

`vox_core::conformance` is the executable, provider-neutral contract for Platform V1 authority and outcome semantics. It exists before the production identity, registry, grant, approval, and integration implementations so every later adapter can be measured against one vocabulary instead of defining its own.

The current fixture version is `1`. The canonical fixture is bundled at `src/conformance/fixtures/v1.json`; changing an accepted meaning requires a new fixture version. Additive scenarios that only make an existing meaning more explicit may remain within the current version after review.

## Run the suite

```sh
cargo test --locked --test platform_conformance
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
```

The integration test runs the same fixtures through both the in-process reference adapter and `JsonBoundary`, which serializes every command and result. The boundary is deliberately transport-neutral: HTTP, MCP, queues, database-backed services, and FFI implementations can all translate their public behavior into `Command` and `SemanticResult` without importing the reference state machine.

To test an implementation, implement `PlatformAdapter` and pass a factory to `run_suite`. Every scenario receives a fresh adapter instance, so success cannot depend on scenario order or leaked state.

## Stable vocabulary

The suite treats these error categories as public semantic outcomes: isolation violation, unavailable capability, missing grant, missing approval, approval mismatch, expired approval, disabled extension, conflict, not found, and invalid request. Adapters may expose richer internal errors, but their public boundary must map them to these categories.

An extension being installed, enabled by an operator, visible in discovery, granted to an agent, approved for an exact action, and successfully executed are separate facts. None implies another.

An action is `succeeded` only with non-empty provider evidence, `failed` only with an explicit provider error, and `unknown` when the provider result cannot be established. Retrying the same idempotency key returns the originally recorded action and outcome; it never upgrades an unknown result based on a later caller claim.

## Coverage map

| Scenario | Contract evidence |
| --- | --- |
| `extension-governance-and-grants` | Disabled extensions are not discoverable or grantable; grants are explicit and revocable. |
| `user-context-isolation` | Grants, proposals, approvals, action outcomes, and event streams remain scoped to one user context. |
| `exact-approval-binding` | Approval is required, bound to the exact input and agent, and expires before execution. |
| `truthful-and-idempotent-outcomes` | Unknown remains unknown, terminal claims require evidence, and retries do not create or rewrite actions. |

The test suite also runs deliberately non-conforming adapters. It must fail when an adapter ignores context isolation, accepts a mismatched approval, or invents a successful result from an unknown provider observation. Those tests prove the harness itself is capable of catching the three high-risk failures called out by the Platform V1 acceptance criteria.

## Compatibility rule

Consumers pin a fixture version. An implementation may support more than one version during migration. Never rewrite an already accepted fixture in a way that changes whether the same observable behavior passes; introduce the next version, migrate consumers, and retire the older version only after its downstream users have moved.
