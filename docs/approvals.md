# Exact proposals and authenticated approvals

Consequential work is first represented as an immutable proposal. A proposal
binds one canonical user context, selected agent, granted capability, exact
structured details, a SHA-256 details hash, and expiry. A material revision
creates a new proposal and supersedes the old proposal; approval never carries
forward.

Only a fresh signed host-context assertion for the owning user can approve a
proposal. Core records the authenticating host app and proposal hash, then
permits exactly one later execution attempt to consume the approval. Agent
claims, stale proposals, other user contexts, forged host assertions, and
replayed approval consumption fail closed.
