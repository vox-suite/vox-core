# Conversation tools

The selected agent and authenticated user context are supplied by Core. Models never select the owning user.

`library` searches enabled guidance, loads a pinned skill, discovers owned specialists and delegates bounded work. It cannot onboard MCP servers, load connector tools or invoke arbitrary provider actions. Skill guidance does not confer account authority.

`read_connected_app` reads Google Calendar or PlayStation through the shared Connections service. Each invocation checks current account authorization and `assistant_read`; results include observation freshness, completeness and bounds. Gaming counters are lifetime observations with unknown session times.

Scoped assistant-memory tools and the existing map/visit tools remain registered on every conversation channel. See [the channel contract test](../src/agents/conversation.rs) and [Connected Apps](connections.md).
