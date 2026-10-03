# PlayStation activity capture

Connections verifies a PSN account using the community NPSSO token exchange and a provider profile read with `isMe=true`. The account identifier comes from the token returned directly by Sony and is confirmed by the authenticated profile request. NPSSO is never stored. Access and refresh tokens use AES-256-GCM and are bound to their connection and token type.

Core API and worker require the same 32-byte hex `VOX_CREDENTIAL_KEY`. Run Core's migrations before restarting either service. Independent Connections hosts apply `schema/playstation.sql` after their connector schema.

In Web's connected accounts panel, sign in on Sony, open Sony's session-cookie page in the same browser, and enter the `npsso` value into the protected form. Choose whether to capture gaming activity. Connect verifies the profile and game read before persisting credentials. It grants no agent access. Use Refresh to establish the first baseline immediately, or wait for the worker. Automatic capture runs once a day; failed requests back off and expired authorization requires reconnection.

The baseline stores cumulative totals. Later increases create `gaming` spans with `source=playstation`, schema binding and an inbound source event, committed atomically with checkpoints. The timeline timestamp is Sony's last-played timestamp, and the metadata stores observed duration and the interval between observations. No session start/end is claimed, and total lifetime playtime is never converted into a recent session. New titles are captured only when their first-played timestamp proves they began after the baseline. Counter decreases are held below the previous high-water total. Missing duration is not interpreted as zero.

Pause and resume reset the baseline to avoid importing activity from the paused period. Disconnect destroys stored tokens and revokes dependent agent grants; existing gaming spans remain. Worker-created spans signal API live subscribers through PostgreSQL notifications.

This is a community integration, not a Sony-issued Vox OAuth client. Sony's private endpoints and the mobile client exchange may change. Live account linking and refresh must be verified with a real account after deployment.

## Local cross-repository verification

Core's released dependency remains pinned to the published Connections revision. Before publishing both changes, build against the sibling checkout:

```sh
cargo check --config 'patch."https://github.com/vox-suite/vox-connections.git".vox-connections.path="../vox-connections"'
```

After publishing Connections, update Core's git dependency lockfile to that revision before building release images. Do not release Core with the previous shared-crate revision.
