# Recovery review

## Standards

The second audit found callback ingestion still running on paused OAuth relinks, personal inbound envelopes missed by reassociation, and a legacy parser signature mismatch. Corrective tests now check actual host ingestion/notification counts, real Takeout event and span ownership, and independent-host required-timestamp/parser imports. HMAC keys remain configured, email equality does not merge identities, and exact penalty approval remains mandatory for Expedia cancellation.

Timeline grant checks resolve account IDs and their actual connector declarations, including normalized Maps/YouTube sources. Account and grant lookups are batched; ID membership uses hash sets rather than repeated linear scans. Agent aggregate reads filter in SQL rather than hydrating all records. Cached six-chart loads at 100,000 records remain one query (local p95 0.89ms); cold loads use five queries and took 2,470.10ms. These fixture results do not establish production latency or global optimality.

## Spec

Internal chart, goal and timeline processors run under the user's owned Personal Assistant (`general`), while their processing names remain telemetry labels. Tests provision that real owned agent through the registry and grant through the public service; no automatic grants or inaccessible invented owned identities. Separate specialist grants do not authorize the assistant.

Relink and repeat import preserve disabled read/sync preferences; paused callbacks/imports skip history ingestion. Reassociation moves only account-bound, same-owner spans/events in the account transaction, preserving IDs and annotations; host failure rolls back account ownership. Legacy dated PlayStation APIs coexist with optional curated history without fabricated timestamps. Schema discovery and numerical query tools require verified context and effective account authority; global declarations remain discoverable.

Validation: 27 account lifecycle tests, 18 scoped Pulse/schema/grant tests, 12 ingestion tests, compatibility imports, unit tests and Clippy. Recovery inventory still covers the original five changes, subsequent Connections changes and corresponding Core history. Host UI grants/reassociation, exact-approval cancellation adaptation and real provider deployment checks remain explicit release gates.

## Totals

The renewed audit identified seven distinct issues across both axes: unreachable internal grant identities, normalized source grant mismatch, aggregate/schema authority gaps, preference resets/paused callback ingestion, incomplete history reassociation, PlayStation API compatibility, and quadratic ID filtering. All received concrete corrections and regression coverage. No unresolved concrete finding from the final targeted audit; release gates remain tracked separately.
