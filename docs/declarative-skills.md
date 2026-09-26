# Declarative skills: current contract

A skill is versioned guidance and bounded JSON resources for an agent. It is not executable code and never carries a connection, credential, grant, or approval. A skill may *request* capability keys; Core reports which of those the selected agent actually has through its effective grants.

## Author locally

```sh
python3 tools/skill_author.py examples/skills/meeting-prep.json
python3 tools/skill_author.py next-version.json --compare examples/skills/meeting-prep.json
```

The package is one JSON object with `external_key`, `title`, `summary`, `instructions`, optional `requested_capabilities` string array, and optional `resources` object. The validator checks the same size and shape limits as Core and flags obvious credential patterns. It cannot prove instructions safe or find every secret; publishers must review the full diff before publication. No file paths, scripts, network references, or executable code are loaded from a package.

## Publish and install

Core exposes host-signed `POST /v1/skills/private` with `{"host_context": {"host_user_id": "...", "organization_external_key": null}, "skill": <package>}`. The caller must use a fresh registered-host assertion (see [host trust](host-trust.md)). Private skills are visible only in that user context. Operators may publish a curated package using `POST /v1/skills/curated` with the Core service token and `{"deployment_external_key":"...", "skill": <package>}`.

Host-signed `POST /v1/skills/list` lists visible packages and installed versions. `POST /v1/skills/{id}/versions/{version}` shows the full version for review. `POST /v1/skills/{id}/install` takes `{"host_context": ..., "version": 2}` and installs exactly the reviewed latest version; it returns `409` if a newer version appeared after review. Existing installations remain pinned until this call. `POST /v1/skills/{id}/disable` stops further loads. Other operations take a `{"host_context": ...}` body. `POST /v1/agents/{agent_key}/skills/{skill_id}/enable` takes `{"host_context": ..., "enabled": true|false}` and explicitly controls which selected agent may load the skill. No agent is enabled by installation alone. Only installed, enabled versions for that agent appear in `POST /v1/agents/{agent_key}/effective-skills`; `POST /v1/agents/{agent_key}/skills/{skill_id}/load` returns instructions and resources on demand. No skill instructions or resources appear in the lightweight discovery response.

The host must clearly distinguish *installed* from *connected account* and *agent grant*. Installation and agent skill enablement never permit a provider call. The current Web account page offers private creation, curated discovery, version review, install, per-agent enablement, and disable. Skill preview against tasks, an author publishing service, and automatic skill selection inside the production conversation model remain release work.
