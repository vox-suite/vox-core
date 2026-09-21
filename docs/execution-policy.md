# Spending policies and operational quotas

An action proposal can disclose an `execution` object with an exact provider,
model, account reference, connection, quoted total in minor units, and ISO-4217
currency. Before Core mediates an execution attempt, it compares the supplied
object to the approved proposal exactly. A changed price, provider, model,
account, or connection fails closed with `fresh proposal required`; Core never
chooses a substitute.

Host apps set user-context spending ceilings through authenticated host trust.
A ceiling is an upper bound for a capability and currency, optionally narrowed
to one provider. Matching ceilings compose to the smallest maximum. A policy
does not create a proposal, approve one, or consume an approval.

This first policy surface intentionally provides deterministic **per-action**
ceilings only. Period budgets need completed/unknown outcome reconciliation and
therefore belong to the authoritative execution and audit work that follows;
Core must pause rather than infer a period-total result before that exists.

Operational quotas are scoped to the complete provider/model/account/connection
identity. Evaluation atomically reserves the requested attempt's exact quota
slot. If it is exhausted, work pauses with `quota exhausted`; no alternative
provider, model, account, or connection is selected. A successful policy result
is still only a constraint check. The authoritative execution layer must later
consume the independently authenticated user approval before external work.

Every evaluated identity mismatch, policy denial, quota denial, and success
stores an immutable decision record with the matched policy versions. This is
evidence for the execution and audit layers, not permission for them to act.
