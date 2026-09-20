# Per-agent capability grants

An agent definition has no external authority by default. Its requested
capability categories are only a declaration of what it may ask a user to
grant. Core records a grant only after the exact canonical user context has a
selected, enabled agent; an enabled deployment integration declaring the
capability; and a currently authorized connection whose provider authorization
includes the same canonical key.

The canonical key is `<integration external key>.<capability external key>`;
for example, `calendar.read`. Host apps create grants through
`POST /v1/capability-grants` using a fresh signed host-context assertion and
can revoke one exact grant with `DELETE /v1/capability-grants`. They
query an agent's currently usable grants through
`POST /v1/agents/{external_key}/effective-capability-grants`, with that same
assertion and the host-context request in the body.

The grant module fails closed. It returns a grant only while all of these hold:
the agent is selected and enabled, the integration remains enabled (the
deployment's policy), the capability remains declared and requested, the grant
is enabled, and the connection is in its live authorized window. Disabling an
agent hides only that agent's grants. Revoking, expiring, or disconnecting a
connection hides every grant using that connection without deleting audit
history or weakening other connections.
