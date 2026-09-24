# Account/Identity Linking (box + desktop) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A user who calls the box, then signs up on vox-desktop with Google
and enters the same phone number, ends up as one account — their prior box
conversations/tasks move onto that account, and future calls from that
number are recognized by name without re-asking.

**Architecture:** Reuse the existing `users` / `auth_identities` /
`channel_identities` tables (no new tables). Add one merge-on-link endpoint
in vox-core, fix a one-line channel-tag bug in vox-bridge that currently
prevents phone identities from resolving correctly, capture the caller's
name at signup instead of only at first call, and add the missing
phone-entry step to vox-desktop. vox-web is untouched (see spec, step 1).

**Tech Stack:** Rust (axum, sqlx, Postgres) for vox-core and vox-bridge;
Rust (Tauri) + TypeScript/React for vox-desktop.

**Spec:** `docs/superpowers/specs/2026-09-25-account-identity-linking-design.md`
(in the vox-core repo)

## Global Constraints

- No tests: do not write any test code (no `#[test]` functions, no test
  files, no assertions-as-tests). If a build or an existing test fails
  because of a change in this plan, delete the failing test — do not
  rewrite it to pass around the change.
- No code comments: do not add comments (including doc comments `///`,
  `//!`, `/** */`) in any file touched by this plan. Existing comments in
  files you edit may stay; do not add new ones.
- Three separate git repositories are touched: `vox-core`, `vox-bridge`,
  `vox-desktop` (each at `/Users/rahul/Documents/vox/<repo>`, each with its
  own `.git`). Every commit step in this plan runs from inside the correct
  repo — check the `Files:` block of each task.
- vox-web is out of scope. Do not edit anything under `vox-web/`.
- No OTP/SMS phone verification — the phone number is trusted as entered.

## Review Focus

- A user who links a phone number that was **never** seen by the box before
  (no existing `channel_identities` row) should just get the number
  attached to their account — not accidentally hit the merge/tombstone path.
- A user who re-submits the **same** phone number they already linked
  (e.g. double-clicking submit, or re-opening the phone screen after a
  restart) must be a no-op, not a second merge attempt or an error.
- The merge must move rows from *every* table with a `user_id` FK to
  `users` — missing one silently strands data on the tombstoned account.
- `/v1/me`'s `has_phone` and `/v1/auth/exchange`'s `has_phone` must agree —
  a client that trusts one over the other should never see contradictory
  answers for the same account.
- The Twilio channel-tag fix must not change the value used for **outbound**
  calls (already `"phone"`) or WhatsApp (already `"whatsapp"`) — only the
  inbound Twilio voice path changes.

---

## Task 1: Capture the caller's real name at signup (vox-core)

**Files:**
- Modify: `/Users/rahul/Documents/vox/vox-core/services/api/identity_token.rs`
- Modify: `/Users/rahul/Documents/vox/vox-core/services/api/routes/auth.rs`
- Modify: `/Users/rahul/Documents/vox/vox-core/services/api/auth.rs`

**Interfaces:**
- Produces: `VerifiedIdentity.name: Option<String>` (new field), used by
  Task 2 and by the code in this task.

- [ ] **Step 1: Add `name` to `VerifiedIdentity` and `TokenClaims`, and a shared extraction helper**

In `services/api/identity_token.rs`, change the `VerifiedIdentity` struct:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
}
```

Change `TokenClaims` and add a metadata struct + helper function right
after it:

```rust
#[derive(Debug, Deserialize)]
struct TokenClaims {
    sub: String,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    user_metadata: Option<SupabaseUserMetadata>,
}

#[derive(Debug, Deserialize)]
struct SupabaseUserMetadata {
    #[serde(default)]
    full_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

fn claims_name(claims: &TokenClaims) -> Option<String> {
    let direct = claims
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    direct.or_else(|| {
        claims.user_metadata.as_ref().and_then(|metadata| {
            metadata
                .full_name
                .as_deref()
                .or(metadata.name.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
    })
}
```

- [ ] **Step 2: Populate `name` at both construction sites**

In `identity_from_payload` (same file), change the returned struct:

```rust
Ok(VerifiedIdentity {
    issuer,
    subject: subject.to_string(),
    email: claims.email.filter(|value| !value.trim().is_empty()),
    name: claims_name(&claims),
})
```

In `verify_asymmetric_components` (same file), change the returned struct:

```rust
Ok(VerifiedIdentity {
    issuer: data
        .claims
        .iss
        .unwrap_or_else(|| expected_issuer.to_string()),
    subject: subject.to_string(),
    email: data.claims.email.filter(|value| !value.trim().is_empty()),
    name: claims_name(&data.claims),
})
```

- [ ] **Step 3: Use the name when creating a user in `exchange_token`**

In `services/api/routes/auth.rs`, inside `exchange_token`, change:

```rust
let display_name = identity.email.as_deref().unwrap_or("Vox User");
```

to:

```rust
let display_name = identity
    .name
    .as_deref()
    .or(identity.email.as_deref())
    .unwrap_or("Vox User");
```

- [ ] **Step 4: Use the name when creating a user via the bearer-JWT path**

In `services/api/auth.rs`, inside `extract_actor`, change:

```rust
let display_name = claims.email.as_deref().unwrap_or("Vox User");
```

to:

```rust
let display_name = claims
    .name
    .as_deref()
    .or(claims.email.as_deref())
    .unwrap_or("Vox User");
```

- [ ] **Step 5: Build and verify**

Run: `cd /Users/rahul/Documents/vox/vox-core && cargo build -p vox-core-api 2>&1 | tail -n 40`
(if the package name differs, run `cargo build --workspace 2>&1 | tail -n 40`
instead — check `Cargo.toml` for the actual package name first)
Expected: builds with no errors.

- [ ] **Step 6: Commit**

```bash
cd /Users/rahul/Documents/vox/vox-core
git add services/api/identity_token.rs services/api/routes/auth.rs services/api/auth.rs
git commit -m "Capture Google/Supabase display name at account creation"
```

---

## Task 2: Add `has_phone` to `/v1/me` and `/v1/auth/exchange` (vox-core)

**Files:**
- Modify: `/Users/rahul/Documents/vox/vox-core/services/api/routes/identity.rs`
- Modify: `/Users/rahul/Documents/vox/vox-core/services/api/routes/auth.rs`
- Modify: `/Users/rahul/Documents/vox/vox-core/services/api/router.rs`

**Interfaces:**
- Consumes: nothing new (uses existing `channel_identities` table).
- Produces: `GET /v1/me` response gains `has_phone: bool`.
  `POST /v1/auth/exchange` response (`AuthExchangeResponse`) gains
  `has_phone: bool`. Task 6 (vox-desktop) consumes both.

- [ ] **Step 1: Add `has_phone` to `GET /v1/me`**

Replace the full contents of
`services/api/routes/identity.rs` with:

```rust
use axum::{Extension, Json, extract::State, http::StatusCode, response::IntoResponse};
use sqlx::PgPool;
use vox_core::domain::identity::Actor;

pub async fn get_me(
    Extension(actor): Extension<Actor>,
    State(pool): State<PgPool>,
) -> Result<impl IntoResponse, StatusCode> {
    let has_phone = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(\
            SELECT 1 FROM channel_identities \
            WHERE user_id = $1 AND channel = 'phone' AND revoked_at IS NULL\
        )",
    )
    .bind(actor.user_id)
    .fetch_one(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(serde_json::json!({
        "user_id": actor.user_id,
        "principal_id": actor.principal_id,
        "grants": actor.grants,
        "has_phone": has_phone,
    })))
}
```

- [ ] **Step 2: Give the `/v1/me` route a pool state, in `router.rs`**

In `services/api/router.rs`, change:

```rust
let identity_routes = Router::new().route("/v1/me", get(get_me));
```

to:

```rust
let identity_routes = Router::new()
    .route("/v1/me", get(get_me))
    .with_state(state.pool.clone());
```

- [ ] **Step 3: Add `has_phone` to `AuthExchangeResponse`**

In `services/api/routes/auth.rs`, change the struct (keep the existing
`expires_at: chrono::DateTime<Utc>` type, only add the new field):

```rust
#[derive(Debug, Serialize)]
pub struct AuthExchangeResponse {
    pub token: String,
    pub user_id: Uuid,
    pub expires_at: chrono::DateTime<Utc>,
    pub has_phone: bool,
}
```

- [ ] **Step 4: Compute `has_phone` in `exchange_token` and include it in the response**

Still in `services/api/routes/auth.rs`, after the existing `has_context`
check block (the one that does
`SELECT EXISTS(SELECT 1 FROM user_contexts WHERE user_id = $1)`), add:

```rust
let has_phone = sqlx::query_scalar::<_, bool>(
    "SELECT EXISTS(\
        SELECT 1 FROM channel_identities \
        WHERE user_id = $1 AND channel = 'phone' AND revoked_at IS NULL\
    )",
)
.bind(user_id)
.fetch_one(&mut *tx)
.await
.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
```

Then change the final response construction:

```rust
Ok(Json(AuthExchangeResponse {
    token: raw_token,
    user_id,
    expires_at,
    has_phone,
}))
```

- [ ] **Step 5: Build and verify**

Run: `cd /Users/rahul/Documents/vox/vox-core && cargo build --workspace 2>&1 | tail -n 40`
Expected: builds with no errors.

- [ ] **Step 6: Commit**

```bash
cd /Users/rahul/Documents/vox/vox-core
git add services/api/routes/identity.rs services/api/routes/auth.rs services/api/router.rs
git commit -m "Add has_phone to /v1/me and /v1/auth/exchange"
```

---

## Task 3: `POST /v1/me/phone` — attach or merge a phone number (vox-core)

**Files:**
- Create: `/Users/rahul/Documents/vox/vox-core/services/api/routes/phone.rs`
- Modify: `/Users/rahul/Documents/vox/vox-core/services/api/routes/mod.rs`
- Modify: `/Users/rahul/Documents/vox/vox-core/services/api/router.rs`

**Interfaces:**
- Consumes: `Actor.user_id: Uuid` (from `vox_core::domain::identity::Actor`,
  already used by `get_me`).
- Produces: `POST /v1/me/phone`, body `{ "phone_number": string }`,
  response `{ "user_id": Uuid, "merged": bool }`. Task 6 (vox-desktop)
  calls this.

- [ ] **Step 1: Create the route file**

Create `services/api/routes/phone.rs`:

```rust
use axum::{Extension, Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;
use vox_core::domain::identity::Actor;

const MERGE_TABLES: &[&str] = &[
    "auth_identities",
    "channel_identities",
    "auth_sessions",
    "conversations",
    "collections",
    "tasks",
    "schedules",
    "jobs",
    "data_schemas",
    "records",
    "devices",
    "connections",
    "action_proposals",
    "action_approvals",
    "executions",
    "inbound_events",
    "audit_events",
    "user_contexts",
];

#[derive(Debug, Deserialize)]
pub struct LinkPhoneRequest {
    pub phone_number: String,
}

#[derive(Debug, Serialize)]
pub struct LinkPhoneResponse {
    pub user_id: Uuid,
    pub merged: bool,
}

pub async fn link_phone(
    Extension(actor): Extension<Actor>,
    State(pool): State<PgPool>,
    Json(payload): Json<LinkPhoneRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let normalized: String = payload
        .phone_number
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    if normalized.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let existing_owner = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM channel_identities \
         WHERE channel = 'phone' AND normalized_external_id = $1 AND revoked_at IS NULL",
    )
    .bind(&normalized)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let merged = match existing_owner {
        None => {
            sqlx::query(
                "INSERT INTO channel_identities (user_id, channel, normalized_external_id, verified_at) \
                 VALUES ($1, 'phone', $2, now())",
            )
            .bind(actor.user_id)
            .bind(&normalized)
            .execute(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            false
        }
        Some(owner) if owner == actor.user_id => false,
        Some(old_user) => {
            for table in MERGE_TABLES {
                let sql = format!("UPDATE {table} SET user_id = $1 WHERE user_id = $2");
                sqlx::query(&sql)
                    .bind(actor.user_id)
                    .bind(old_user)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
            sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
                .bind(old_user)
                .execute(&mut *tx)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            true
        }
    };

    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(LinkPhoneResponse {
        user_id: actor.user_id,
        merged,
    }))
}
```

- [ ] **Step 2: Register the module**

In `services/api/routes/mod.rs`, add (keep the list alphabetical, matching
the existing style):

```rust
pub mod phone;
```

- [ ] **Step 3: Wire the route into the protected router**

In `services/api/router.rs`, add the import — change:

```rust
        identity::get_me,
```

to:

```rust
        identity::get_me,
        phone::link_phone,
```

Then add a new router value near `let identity_routes = ...`:

```rust
    let phone_routes = Router::new()
        .route("/v1/me/phone", post(link_phone))
        .with_state(state.pool.clone());
```

Then add it to the merge chain — change:

```rust
    let protected_routes = task_routes
        .merge(collection_routes)
        .merge(record_routes)
        .merge(schema_routes)
        .merge(device_routes)
        .merge(device_socket_routes)
        .merge(event_routes)
        .merge(identity_routes)
```

to:

```rust
    let protected_routes = task_routes
        .merge(collection_routes)
        .merge(record_routes)
        .merge(schema_routes)
        .merge(device_routes)
        .merge(device_socket_routes)
        .merge(event_routes)
        .merge(identity_routes)
        .merge(phone_routes)
```

- [ ] **Step 4: Build and verify**

Run: `cd /Users/rahul/Documents/vox/vox-core && cargo build --workspace 2>&1 | tail -n 40`
Expected: builds with no errors.

- [ ] **Step 5: Commit**

```bash
cd /Users/rahul/Documents/vox/vox-core
git add services/api/routes/phone.rs services/api/routes/mod.rs services/api/router.rs
git commit -m "Add POST /v1/me/phone to attach or merge a phone identity"
```

---

## Task 4: Fix the Twilio inbound channel tag (vox-bridge)

**Files:**
- Modify: `/Users/rahul/Documents/vox/vox-bridge/src/channels/twilio/stream.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: inbound Twilio calls now resolve identity under vox-core's
  `channel = 'phone'`, the same value Task 3's endpoint and outbound calls
  already use — this is what makes a linked phone number actually be
  recognized on the next call.

- [ ] **Step 1: Change the channel tag**

In `src/channels/twilio/stream.rs`, inside `run_twilio_socket`, find:

```rust
    let context = CallContext {
        channel: "twilio".into(),
        external_identity: accepted_call.from,
```

Change `channel: "twilio".into(),` to `channel: "phone".into(),`.

- [ ] **Step 2: Build and verify**

Run: `cd /Users/rahul/Documents/vox/vox-bridge && cargo build --workspace 2>&1 | tail -n 40`
Expected: builds with no errors.

- [ ] **Step 3: Commit**

```bash
cd /Users/rahul/Documents/vox/vox-bridge
git add src/channels/twilio/stream.rs
git commit -m "Tag inbound Twilio calls as channel=phone, not twilio"
```

---

## Task 5: Delete the stale pre-rewrite schema files (vox-core)

**Files:**
- Delete: `/Users/rahul/Documents/vox/vox-core/schema/01_extensions.sql`
  through `21_user_voiceprints.sql` (all numbered files in `schema/`)
- Delete: `/Users/rahul/Documents/vox/vox-core/supabase_schema.sql`
- Keep: `/Users/rahul/Documents/vox/vox-core/schema/target_core.sql`

**Interfaces:** none — these files are not imported/read by any running
code path (verified: nothing under `src/` or `services/` reads from
`schema/` or `supabase_schema.sql` at runtime; they are static reference
SQL only).

- [ ] **Step 1: List the files before deleting, to confirm the exact set**

Run: `cd /Users/rahul/Documents/vox/vox-core && ls schema/*.sql`
Expected output includes `01_extensions.sql` through
`21_user_voiceprints.sql` and `target_core.sql`.

- [ ] **Step 2: Delete the stale files**

```bash
cd /Users/rahul/Documents/vox/vox-core
git rm schema/01_extensions.sql schema/02_users.sql schema/03_user_identities.sql \
  schema/04_user_profiles.sql schema/05_conversations.sql schema/06_messages.sql \
  schema/07_conversation_summaries.sql schema/08_projects.sql schema/09_tasks.sql \
  schema/10_user_goals.sql schema/11_user_records.sql schema/12_user_insights.sql \
  schema/13_client_devices.sql schema/14_events.sql schema/15_scheduled_tasks.sql \
  schema/16_jobs.sql schema/17_actions.sql schema/18_action_attempts.sql \
  schema/19_indexes.sql schema/20_data_schemas.sql schema/21_user_voiceprints.sql \
  supabase_schema.sql
```

If any filename in that list doesn't exist, drop it from the command and
re-run — use the `ls` output from Step 1 as the source of truth, not this
list.

- [ ] **Step 3: Verify nothing references the deleted files**

Run:
```bash
cd /Users/rahul/Documents/vox/vox-core
grep -rn "supabase_schema.sql\|schema/01_extensions\|schema/02_users" --include="*.rs" --include="*.toml" --include="*.md" . 2>/dev/null
```
Expected: no output (or only matches inside this plan/spec doc, which is
fine).

- [ ] **Step 4: Commit**

```bash
cd /Users/rahul/Documents/vox/vox-core
git commit -m "Delete stale pre-rewrite schema files superseded by migrations/"
```

---

## Task 6: vox-desktop (Rust) — has_phone plumbing and link_phone command

**Files:**
- Modify: `/Users/rahul/Documents/vox/vox-desktop/src-tauri/src/session_store.rs`
- Modify: `/Users/rahul/Documents/vox/vox-desktop/src-tauri/src/auth.rs`
- Modify: `/Users/rahul/Documents/vox/vox-desktop/src-tauri/src/lib.rs`

**Interfaces:**
- Consumes: `POST /v1/me/phone` and the `has_phone` field on
  `/v1/auth/exchange` and `/v1/me`, from Tasks 2 and 3.
- Produces: `AuthState.has_phone: bool` (Rust), Tauri command
  `link_phone(phone_number: String) -> Result<AuthState, String>`. Task 7
  (frontend) consumes both.

- [ ] **Step 1: Add `has_phone` to `StoredSession`**

In `src-tauri/src/session_store.rs`, change:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredSession {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: String,
    pub email: Option<String>,
    pub vox_token: String,
    pub expires_at: Option<String>,
}
```

to:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredSession {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: String,
    pub email: Option<String>,
    pub vox_token: String,
    pub expires_at: Option<String>,
    #[serde(default)]
    pub has_phone: bool,
}
```

(`#[serde(default)]` so a session saved before this change still loads.)

- [ ] **Step 2: Add `has_phone` to `AuthState`, `AuthExchangeResponse`, `CoreMeResponse`**

In `src-tauri/src/auth.rs`, change:

```rust
#[derive(Clone, Debug, Serialize)]
pub struct AuthState {
    pub signed_in: bool,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub bridge_url: String,
    pub api_url: String,
}
```

to:

```rust
#[derive(Clone, Debug, Serialize)]
pub struct AuthState {
    pub signed_in: bool,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub bridge_url: String,
    pub api_url: String,
    pub has_phone: bool,
}
```

Change:

```rust
#[derive(Deserialize)]
struct AuthExchangeResponse {
    token: String,
    user_id: Uuid,
    expires_at: String,
}
```

to:

```rust
#[derive(Deserialize)]
struct AuthExchangeResponse {
    token: String,
    user_id: Uuid,
    expires_at: String,
    has_phone: bool,
}
```

Change:

```rust
#[derive(Deserialize)]
struct CoreMeResponse {
    user_id: Uuid,
}
```

to:

```rust
#[derive(Deserialize)]
struct CoreMeResponse {
    user_id: Uuid,
    has_phone: bool,
}
```

- [ ] **Step 3: Populate `has_phone` in `AuthManager::state()`**

In `src-tauri/src/auth.rs`, change the `state()` method body:

```rust
    pub fn state(&self) -> AuthState {
        self.pull_session_from_store();
        let session = self.session.lock().ok().and_then(|guard| guard.clone());
        AuthState {
            signed_in: session.is_some(),
            user_id: session.as_ref().map(|s| s.user_id.clone()),
            email: session.as_ref().and_then(|s| s.email.clone()),
            bridge_url: self.config.bridge_url.clone(),
            api_url: self.config.api_url.clone(),
            has_phone: session.as_ref().map(|s| s.has_phone).unwrap_or(false),
        }
    }
```

- [ ] **Step 4: Carry `has_phone` through `establish_session` and `exchange_with_core`**

In `src-tauri/src/auth.rs`, inside `establish_session`, change:

```rust
        let session = StoredSession {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            user_id: exchange.user_id.to_string(),
            email: email.or(Some(user_id)),
            vox_token: exchange.token,
            expires_at: Some(exchange.expires_at),
        };
```

to:

```rust
        let session = StoredSession {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            user_id: exchange.user_id.to_string(),
            email: email.or(Some(user_id)),
            vox_token: exchange.token,
            expires_at: Some(exchange.expires_at),
            has_phone: exchange.has_phone,
        };
```

In `exchange_with_core`, the success path already returns the deserialized
`AuthExchangeResponse` as-is, so no change needed there. In the same
function's fallback path (after the `/v1/me` GET), change:

```rust
    Ok(AuthExchangeResponse {
        token: access_token.to_string(),
        user_id: me.user_id,
        expires_at: String::new(),
    })
```

to:

```rust
    Ok(AuthExchangeResponse {
        token: access_token.to_string(),
        user_id: me.user_id,
        expires_at: String::new(),
        has_phone: me.has_phone,
    })
```

- [ ] **Step 5: Add `AuthManager::link_phone` and the Tauri command**

In `src-tauri/src/auth.rs`, add near the other `#[tauri::command]`
functions (after `sign_out`):

```rust
#[derive(Serialize)]
struct LinkPhoneRequest<'a> {
    phone_number: &'a str,
}

#[derive(Deserialize)]
struct LinkPhoneResponse {
    #[allow(dead_code)]
    user_id: Uuid,
    #[allow(dead_code)]
    merged: bool,
}

impl AuthManager {
    pub async fn link_phone(&self, phone_number: String) -> Result<AuthState, String> {
        let session = self
            .current_session()
            .ok_or_else(|| "Not signed in".to_string())?;
        let url = format!("{}/v1/me/phone", self.config.api_url);
        let response = reqwest::Client::new()
            .post(&url)
            .header("authorization", format!("Bearer {}", session.vox_token))
            .json(&LinkPhoneRequest {
                phone_number: &phone_number,
            })
            .send()
            .await
            .map_err(|e| format!("Failed to reach Vox API: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("Could not save phone number ({status}): {text}"));
        }
        let _: LinkPhoneResponse = response
            .json()
            .await
            .map_err(|e| format!("Invalid phone link response: {e}"))?;

        let mut updated = session;
        updated.has_phone = true;
        save_session(&updated)?;
        if let Ok(mut guard) = self.session.lock() {
            *guard = Some(updated);
        }
        Ok(self.state())
    }
}

#[tauri::command]
pub async fn link_phone(
    phone_number: String,
    auth: State<'_, AuthManager>,
) -> Result<AuthState, String> {
    auth.link_phone(phone_number).await
}
```

This uses `AuthManager` (already `impl`-ed above in the file), `State`,
`Serialize`/`Deserialize`, `Uuid`, `reqwest` — all already imported at the
top of `auth.rs`.

- [ ] **Step 6: Register the command**

In `src-tauri/src/lib.rs`, inside `tauri::generate_handler![...]`, change:

```rust
            auth::get_auth_state,
            auth::sign_in_with_google,
            auth::sign_out,
```

to:

```rust
            auth::get_auth_state,
            auth::sign_in_with_google,
            auth::sign_out,
            auth::link_phone,
```

- [ ] **Step 7: Build and verify**

Run: `cd /Users/rahul/Documents/vox/vox-desktop/src-tauri && cargo build 2>&1 | tail -n 40`
Expected: builds with no errors.

- [ ] **Step 8: Commit**

```bash
cd /Users/rahul/Documents/vox/vox-desktop
git add src-tauri/src/session_store.rs src-tauri/src/auth.rs src-tauri/src/lib.rs
git commit -m "Add has_phone tracking and link_phone command"
```

---

## Task 7: vox-desktop (frontend) — phone-entry screen

**Files:**
- Modify: `/Users/rahul/Documents/vox/vox-desktop/src/lib/tauri.ts`
- Modify: `/Users/rahul/Documents/vox/vox-desktop/src/hooks/use-auth.ts`
- Create: `/Users/rahul/Documents/vox/vox-desktop/src/components/phone-entry-screen.tsx`
- Modify: `/Users/rahul/Documents/vox/vox-desktop/src/App.tsx`

**Interfaces:**
- Consumes: Tauri command `link_phone` and `AuthState.has_phone` from
  Task 6.
- Produces: `PhoneEntryScreen` component, `useAuth().linkPhone(phoneNumber)`.

- [ ] **Step 1: Add `has_phone` to the `AuthState` TS type and `linkPhone` to the `api` object**

In `src/lib/tauri.ts`, change:

```ts
export type AuthState = {
  signed_in: boolean;
  user_id: string | null;
  email: string | null;
  bridge_url: string;
  api_url: string;
};
```

to:

```ts
export type AuthState = {
  signed_in: boolean;
  user_id: string | null;
  email: string | null;
  bridge_url: string;
  api_url: string;
  has_phone: boolean;
};
```

In the `api` object, change:

```ts
export const api = {
  getAuthState: () => invoke<AuthState>("get_auth_state"),
  signInWithGoogle: () => invoke<AuthState>("sign_in_with_google"),
  signOut: () => invoke<AuthState>("sign_out"),
```

to:

```ts
export const api = {
  getAuthState: () => invoke<AuthState>("get_auth_state"),
  signInWithGoogle: () => invoke<AuthState>("sign_in_with_google"),
  signOut: () => invoke<AuthState>("sign_out"),
  linkPhone: (phoneNumber: string) =>
    invoke<AuthState>("link_phone", { phoneNumber }),
```

- [ ] **Step 2: Add `linkPhone` to `useAuth`**

In `src/hooks/use-auth.ts`, change `emptyAuth`:

```ts
const emptyAuth: AuthState = {
  signed_in: false,
  user_id: null,
  email: null,
  bridge_url: "",
  api_url: "",
};
```

to:

```ts
const emptyAuth: AuthState = {
  signed_in: false,
  user_id: null,
  email: null,
  bridge_url: "",
  api_url: "",
  has_phone: false,
};
```

Add a new function next to `signOut`, and return it from the hook:

```ts
  async function linkPhone(phoneNumber: string) {
    setAuthBusy(true);
    setAuthError("");
    try {
      const next = await api.linkPhone(phoneNumber);
      setAuth(next);
    } catch (err) {
      setAuthError(invokeErrorMessage(err));
    } finally {
      setAuthBusy(false);
    }
  }
```

Change the final return statement:

```ts
  return { auth, authBusy, authError, googleSignIn, signOut };
```

to:

```ts
  return { auth, authBusy, authError, googleSignIn, signOut, linkPhone };
```

- [ ] **Step 3: Create the phone-entry screen**

Create `src/components/phone-entry-screen.tsx`:

```tsx
import { useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { VoxLogo } from "@/components/vox-logo";

export function PhoneEntryScreen({
  busy,
  error,
  onSubmit,
}: {
  busy: boolean;
  error: string;
  onSubmit: (phoneNumber: string) => void;
}) {
  const [phoneNumber, setPhoneNumber] = useState("");

  return (
    <main
      className="relative flex h-full w-full flex-col overflow-hidden bg-void-black"
      tabIndex={0}
    >
      <div
        className="sign-in-glow pointer-events-none absolute -left-32 -top-36 h-[34rem] w-[34rem]"
        aria-hidden
      />
      <div
        className="sign-in-noise pointer-events-none absolute inset-0"
        aria-hidden
      />
      <div className="fixed inset-x-0 top-0 z-50 h-3" data-tauri-drag-region />
      <div
        className="relative z-10 flex flex-1 flex-col items-center justify-center gap-7 px-6"
        data-tauri-drag-region
      >
        <VoxLogo size={120} animated state={error ? "error" : "idle"} />
        <div className="flex w-full max-w-105 flex-col items-center text-center">
          <div className="mb-2 inline-flex items-center gap-2">
            <h1 className="text-[32px] font-normal leading-[1.15] text-pure-white">
              Your number
            </h1>
            <Badge
              variant="secondary"
              className="font-mono text-[10px] uppercase tracking-wider"
            >
              Desktop
            </Badge>
          </div>
          <p className="mb-7 max-w-80 text-sm text-ash">
            Add the phone number you call Vox from, so it recognizes you and
            your tasks show up here too. Anyone who later enters this number
            will inherit its history — only use a number that's yours.
          </p>
          <Card className="shadow-key w-full border-0 bg-transparent p-1.5">
            <form
              className="flex flex-col gap-3"
              onSubmit={(event) => {
                event.preventDefault();
                if (phoneNumber.trim()) onSubmit(phoneNumber.trim());
              }}
            >
              <Input
                type="tel"
                placeholder="+1 555 000 1234"
                value={phoneNumber}
                onChange={(event) => setPhoneNumber(event.target.value)}
                disabled={busy}
              />
              <Button
                type="submit"
                className="shadow-btn-lift h-11 w-full"
                disabled={busy || !phoneNumber.trim()}
              >
                {busy ? "Saving…" : "Continue"}
              </Button>
              {error ? (
                <p className="text-center text-xs text-coral-pulse">
                  {error}
                </p>
              ) : null}
            </form>
          </Card>
        </div>
      </div>
    </main>
  );
}
```

- [ ] **Step 4: Gate the app on `has_phone`**

In `src/App.tsx`, add the import:

```tsx
import { PhoneEntryScreen } from "@/components/phone-entry-screen";
```

Change:

```tsx
  const { auth: authState, authBusy, authError, googleSignIn } = auth;
```

to:

```tsx
  const { auth: authState, authBusy, authError, googleSignIn, linkPhone } = auth;
```

Change:

```tsx
  if (!authState.signed_in) {
    return (
      <SignInScreen
        busy={authBusy}
        error={authError}
        onSignIn={() => void handleGoogleSignIn()}
      />
    );
  }
```

to:

```tsx
  if (!authState.signed_in) {
    return (
      <SignInScreen
        busy={authBusy}
        error={authError}
        onSignIn={() => void handleGoogleSignIn()}
      />
    );
  }

  if (!authState.has_phone) {
    return (
      <PhoneEntryScreen
        busy={authBusy}
        error={authError}
        onSubmit={(phoneNumber) => void linkPhone(phoneNumber)}
      />
    );
  }
```

- [ ] **Step 5: Build and verify**

Run: `cd /Users/rahul/Documents/vox/vox-desktop && npm run build 2>&1 | tail -n 60`
Expected: builds with no type errors.

- [ ] **Step 6: Commit**

```bash
cd /Users/rahul/Documents/vox/vox-desktop
git add src/lib/tauri.ts src/hooks/use-auth.ts src/components/phone-entry-screen.tsx src/App.tsx
git commit -m "Add phone-entry step to desktop sign-in flow"
```
