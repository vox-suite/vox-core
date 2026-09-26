/**
 * Amazon.in shopper: HTTP client for the local Playwright helper
 * (`services/amazon-shopper`) and the server-side guard that only lets an
 * order be placed after the caller heard the checkout total and answered.
 *
 * Demo path, enabled only when `AMAZON_SHOPPER_URL` is set. The helper drives
 * a single visible Chrome window, so it serves one shopper at a time.
 */
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

// vox-bridge gives a whole agent turn 30 s, so one browser step must finish
// well inside that.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const SESSION_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, thiserror::Error)]
pub enum ShopperError {
    #[error("the Amazon browser helper is not reachable; ask the user to start it")]
    Unreachable,
    #[error("the Amazon browser helper took too long to respond")]
    Timeout,
    #[error("the Amazon browser helper rejected Vox's token")]
    Unauthorized,
    #[error("the Amazon browser helper returned an invalid response")]
    InvalidResponse,
}

/// Client for the helper's JSON API. Every handled outcome (including
/// `needs_human`, `cod_unavailable`, `dry_run`) comes back as `Ok` with a
/// `status` field so the agent can explain it; only transport failures are
/// errors.
#[derive(Clone)]
pub struct ShopperClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl ShopperClient {
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Result<Self, ShopperError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| ShopperError::Unreachable)?;
        Ok(Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            token: token.filter(|t| !t.trim().is_empty()),
        })
    }

    pub async fn search(&self, query: &str) -> Result<Value, ShopperError> {
        self.post("/search", json!({ "query": query })).await
    }

    pub async fn open_product(&self, asin: &str) -> Result<Value, ShopperError> {
        self.post("/product", json!({ "asin": asin })).await
    }

    pub async fn select_options(
        &self,
        options: &HashMap<String, String>,
    ) -> Result<Value, ShopperError> {
        self.post("/select", json!({ "options": options })).await
    }

    pub async fn checkout(&self, quantity: u32) -> Result<Value, ShopperError> {
        self.post("/checkout", json!({ "quantity": quantity }))
            .await
    }

    pub async fn place_order(&self) -> Result<Value, ShopperError> {
        self.post("/place", json!({})).await
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, ShopperError> {
        let mut request = self
            .http
            .post(format!("{}{}", self.base_url, path))
            .json(&body);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|err| {
            if err.is_timeout() {
                ShopperError::Timeout
            } else {
                ShopperError::Unreachable
            }
        })?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ShopperError::Unauthorized);
        }
        // The helper answers handled failures with a JSON body too, whatever
        // the HTTP status, so the body decides.
        response.json::<Value>().await.map_err(|err| {
            if err.is_timeout() {
                ShopperError::Timeout
            } else {
                ShopperError::InvalidResponse
            }
        })
    }
}

/// The `status` a helper response reports, or `""` when it has none.
pub fn response_status(response: &Value) -> &str {
    response.get("status").and_then(Value::as_str).unwrap_or("")
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlaceDecision {
    /// Checkout was prepared in an earlier turn: place the order.
    Allowed,
    /// No checkout is ready (never run, expired, or already used).
    NoCheckout,
    /// Checkout was prepared in this same turn: the caller has not answered.
    SameTurn,
}

#[derive(Clone)]
struct Session {
    last_active: Instant,
    checkout_turn: Option<Uuid>,
    /// Where the purchase stands, shown to the agent each turn: it only
    /// remembers what was said, not earlier tool results.
    note: Option<String>,
}

impl Session {
    fn new() -> Self {
        Self {
            last_active: Instant::now(),
            checkout_turn: None,
            note: None,
        }
    }
}

/// Per-user shopping state shared across agent turns: keeps short replies
/// ("teal, 256", "yes") routed to the shopping tools, and enforces that an
/// order is only placed from a turn after the one that read the total.
#[derive(Clone)]
pub struct ShopperSessions {
    sessions: Arc<Mutex<HashMap<Uuid, Session>>>,
    ttl: Duration,
}

impl Default for ShopperSessions {
    fn default() -> Self {
        Self::with_ttl(SESSION_TTL)
    }
}

impl ShopperSessions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            sessions: Arc::default(),
            ttl,
        }
    }

    /// Whether the user used a shopping tool recently.
    pub fn is_active(&self, user_id: Uuid) -> bool {
        self.live(|sessions| sessions.contains_key(&user_id))
    }

    /// Records shopping activity. Browsing (search, product, options) also
    /// invalidates any prepared checkout, since the cart no longer matches.
    pub fn touch(&self, user_id: Uuid) {
        self.update(user_id, |session| session.checkout_turn = None);
    }

    /// Marks checkout as prepared in `turn`. If it was already prepared in an
    /// earlier turn (and not browsed away from since), that earlier turn is
    /// kept: the caller has heard the total already.
    pub fn record_checkout(&self, user_id: Uuid, turn: Uuid) {
        self.update(user_id, |session| {
            session.checkout_turn.get_or_insert(turn);
        });
    }

    /// Keeps the session alive without changing where the purchase stands.
    pub fn keep_alive(&self, user_id: Uuid) {
        self.update(user_id, |_| {});
    }

    pub fn set_note(&self, user_id: Uuid, note: impl Into<String>) {
        let note = note.into();
        self.update(user_id, |session| session.note = Some(note));
    }

    /// The latest note on where the purchase stands, if a session is active.
    pub fn note(&self, user_id: Uuid) -> Option<String> {
        self.live(|sessions| sessions.get(&user_id).and_then(|s| s.note.clone()))
    }

    /// Decides whether `turn` may place the order. `Allowed` consumes the
    /// checkout, so a retry needs a fresh checkout and confirmation.
    pub fn authorize_place(&self, user_id: Uuid, turn: Uuid) -> PlaceDecision {
        self.live(|sessions| match sessions.get_mut(&user_id) {
            Some(session) => match session.checkout_turn {
                Some(checkout_turn) if checkout_turn == turn => PlaceDecision::SameTurn,
                Some(_) => {
                    session.checkout_turn = None;
                    session.last_active = Instant::now();
                    PlaceDecision::Allowed
                }
                None => PlaceDecision::NoCheckout,
            },
            None => PlaceDecision::NoCheckout,
        })
    }

    pub fn end(&self, user_id: Uuid) {
        self.live(|sessions| {
            sessions.remove(&user_id);
        });
    }

    fn update(&self, user_id: Uuid, f: impl FnOnce(&mut Session)) {
        self.live(|sessions| {
            let session = sessions.entry(user_id).or_insert_with(Session::new);
            session.last_active = Instant::now();
            f(session);
        });
    }

    fn live<T>(&self, f: impl FnOnce(&mut HashMap<Uuid, Session>) -> T) -> T {
        let mut sessions = self.sessions.lock().expect("shopper session lock poisoned");
        let now = Instant::now();
        sessions.retain(|_, s| now.duration_since(s.last_active) < self.ttl);
        f(&mut sessions)
    }
}
