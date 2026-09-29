# Spaces: agent-built ideation graphs

Date: 2026-09-30
Status: draft for review. Flow and architecture only; schema and API come after this is approved.
Working name: "Spaces" (name not final).

## Problem

Vox knows what happened (spans), how spans are grouped (collections), and
patterns in them (Pulse). It has nothing for the phase before: "I'm thinking
of doing X, and it will cost me time and money." Today the user researches
in the open web, then separately checks their own finances and calendar,
and reconciles by hand.

A Space is that ideation phase. The user states an intent; an agent gathers
the user's own data, researches options, compares them, and drafts a plan,
all shown as a live graph the user can steer by chat.

## Concepts

| Concept | Meaning |
|---|---|
| Space | Ideation of something that will take the user's time or resources. Not yet real. |
| Span | Something that took time or resources. |
| Collection | A label grouping spans (e.g. "Amuna trip"). |
| Pulse | Charts over spans. |

A Space is upstream of all three and owns none of them. Its nodes are not spans.
The only link is an explicit **commit** at the end (see Ending a Space).

## Example: weekend trip near Bangalore

1. User opens a Space and says: short trip, tomorrow or day after, with a friend, avoid crowds (long weekend).
2. Agent restates intent as the root node. It asks only what it cannot look up (preferences, who, hard constraints), never facts already in Vox (budget, past spend).
3. **Data fan-out.** A "pulling your data" node runs; branches appear: this month's spend vs usual, savings goals and headroom, past trips, calendar for the dates. Each shows its numbers and provenance.
4. Branches converge into a synthesis node ("what I know about you"): budget range, day trip vs overnight.
5. **Research fan-out.** Web/places/route research runs against those constraints. Each candidate (Gokarna, Munnar, Chennai...) is its own node: pros, cons, cost, travel time, crowd risk. Weak candidates are pruned with a stated reason.
6. A comparison node ranks survivors; a plan node drafts days, stay, rough costs.
7. User steers by chat ("make it a day trip", "what about Coorg?"). The graph is edited, not restarted: only nodes downstream of the change re-run.
8. User concludes. A decision node records the outcome.

## Architecture

### Per-Space agent spec, generic runtime
- One generic Space runtime. No hardcoded domain tools, no per-Space `agent_definitions` row (that table is an operator-managed, deployment-level catalog).
- On Space creation an **architect step** (one LLM call) reads the intent, the user's data catalog (`data_schemas` descriptions) and the capability catalog, and writes the Space's **agent spec**: mission, what data and research to look for, done-when criteria, effort limits.
- The runtime executes the spec in a loop and streams every step to the canvas. Chat messages update the spec and graph.

### Capabilities
- Discovered at runtime from the existing capability layer (integration capability declarations, connectors, MCP, skills, grants). Every capability the user has connected is available to every Space agent automatically; no per-Space approval.
- Side-effect actions (booking, paying, messaging) still go through the existing `action_approvals` flow and surface as a node awaiting the user's yes. Reads and research run freely. Ideation never spends or sends without approval.

### Platform primitives (domain-neutral, shared by every Space)
1. **Read the user's own data:** schema catalog plus constrained aggregate queries, reusing the Pulse `query_spec` builder. The agent proposes a spec, never SQL.
2. **Write graph:** add/update/remove nodes and edges. Nodes are free-form: title, body, kind label chosen by the agent, status, parents, provenance, structured data. No fixed node-type enum; the canvas renders whatever arrives (icon/color tokens as in `data_schemas`).
3. **Spawn child run:** parallel branches via the existing `jobs` queue; each child streams into its own branch.

### Finding relevant data
Embed the intent, match against `data_schemas.embedding` (already `vector(768)`) to select relevant categories, then run structured queries on them. No separate vector DB. Semantic search over spans themselves is deferred until a real need appears (e.g. "that beach place last year").

### Steering and staleness
Each node records which nodes it derived from. Editing or rejecting a node marks its descendants stale and re-runs only those. Upstream nodes are untouched.

### Ending a Space
Concluding records a decision node; the Space is then done as an idea. The UI offers an explicit **Commit** button (never automatic) that creates a collection, planned spans (with hierarchy) and reminders from the plan. Committing stores a link from the Space to the resulting collection, which later enables plan-vs-actual comparison (deferred).

## Client
React Flow (`@xyflow/react`) canvas with live-streaming nodes, running/done/stale states, per-node provenance, and a chat panel. Client (vox-desktop vs vox-web) to be decided; neither has React Flow today.

## Phases
1. Space runtime skeleton: architect step, graph-write primitive, canvas, chat; no data access.
2. Read-own-data primitive and data fan-out with provenance.
3. Research fan-out via connected capabilities; child runs; pruning and comparison.
4. Steering with staleness/partial re-run.
5. Commit to collection, planned spans, reminders.
6. Later: plan-vs-actual, semantic search over spans.

## Open questions
- Which client first.
- Final name.
- Effort limits per Space (tokens/steps/time) and how they surface to the user.
- Whether `spans.status = 'planned'` is acceptable for committed plans, or commit should create only the collection and reminders.
