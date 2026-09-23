# Public integration discovery contract

## Host-facing read

`POST /v1/capabilities/discover` takes a fresh E06 signed host assertion in
`x-vox-host-*` headers and a JSON body:

```json
{
  "host_context": {
    "host_user_id": "host-user-42",
    "organization_external_key": null
  },
  "agent_external_key": "saathi",
  "region": "IN"
}
```

The optional `agent_external_key` narrows the catalog to capability categories
requested by an enabled, selected agent in the asserted deployment. Omit it for
the host's connect/catalog screen. The caller cannot supply a deployment ID;
Core derives it from the signed host credential. A missing or replayed assertion
returns `401`, an invalid host context or agent key returns `400`, and an agent
that is not selected in that deployment returns `404`.

`region` is an optional two-letter country code. Core returns capabilities
declared for that region or `global`. Without a region, only `global`
capabilities appear. The host supplies the region for presentation; Core does
not infer or verify the user's physical location. Execution must still enforce
the applicable policy and provider region. An empty or invalid region is `400`.

The response is an array of versioned, deployment-enabled declarations. For
example:

```json
[
  {
    "integration_external_key": "weather",
    "protocol": "direct",
    "display_name": "Weather",
    "declaration_version": 1,
    "capability": {
      "external_key": "search",
      "effect": "read",
      "access_needs": ["oauth"],
      "data_recipients": ["provider"],
      "regions": ["global"],
      "failure_modes": ["expired_access"],
      "optional_guarantees": {}
    },
    "declaration_is_claim": true
  }
]
```

Discovery is **not** authority. The response does not indicate that the user
has connected an account or granted this agent access. Hosts and agents must
query the signed context's effective grants and let Core enforce the exact
connection, grant, policy, approval, and execution checks. An empty response is
valid. Disabled integration versions are absent; historical records remain
stored and interpretable.

## Compatibility and rollback

The existing operator-only
`GET /v1/deployments/{external_key}/capabilities` remains available to trusted
deployment operations with the operator service token. Host applications must
use the signed-context route and must never receive that token. This change
adds an `integration_declaration_versions` table. The operator-only
`GET /v1/deployments/{external_key}/integrations/{integration_key}/versions`
returns each immutable snapshot with its version and creation time. Registering
the same version with a different declaration or an older version fails with
`400`; an exact retry is a no-op. A higher version disables the integration,
invalidates its currently authorized Core connections, and revokes their agent
grants. An operator must re-enable the declaration, and users must reconnect
and explicitly grant access again. This conservative rule also applies to
apparently cosmetic changes so permission growth can never inherit old consent.

The migration snapshots each currently installed version. Versions overwritten
before this migration cannot be reconstructed. Rollback must leave the new
history table intact even if the API code is reverted; clients must treat
discovery failure as unavailable and must not fall back to unscoped operator
discovery or broaden grants. The application should stay at the new schema
until the corresponding code can be restored, because older Core versions do
not preserve new declaration history.

The isolated PostgreSQL integration suite in `tests/integration_registry.rs`
checks enabled-only catalog results, deployment isolation, selected-agent and
region filtering, missing assertion, assertion replay, immutable version history,
secret-key rejection, and revocation of old connection/grant authority.
