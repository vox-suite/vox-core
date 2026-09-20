# Protocol-neutral integration registry

Core records integration declarations for either `mcp` or `direct` protocol.
Each capability declares its effect, access needs, data recipients, regions,
failure modes, and optional guarantees. These are presentation claims, not
enforcement facts: discovery never authorizes a connection, grant, approval, or
action.

Registrations begin disabled. Only operator-enabled integrations appear in
deployment discovery, and each result includes its declaration version,
protocol, and `declaration_is_claim: true`. A later authority contract must
validate connection, grants, policy, and outcome independently.
