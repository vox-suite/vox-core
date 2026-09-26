use serde::Serialize;
use serde_json::Value;

/// How much care a connected-app tool needs before the agent may run it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicy {
    /// Only reads data.
    Read,
    /// Changes something that is easy to undo (a cart, a draft, a task).
    Change,
    /// Spends money, contacts someone, or is hard to undo: the user must
    /// confirm in a later turn.
    Confirm,
}

impl ToolPolicy {
    pub fn needs_confirmation(self) -> bool {
        self == Self::Confirm
    }
}

/// Verbs whose effect is consequential: money moves, a booking or message is
/// made on the user's behalf, or data is destroyed.
const CONSEQUENTIAL: &[&str] = &[
    "place",
    "order",
    "checkout",
    "pay",
    "payment",
    "purchase",
    "buy",
    "book",
    "reserve",
    "cancel",
    "delete",
    "remove",
    "destroy",
    "erase",
    "send",
    "post",
    "publish",
    "submit",
    "confirm",
    "transfer",
    "refund",
    "share",
    "invite",
    "reply",
    "forward",
    "tip",
    "donate",
    "subscribe",
    "unsubscribe",
    "archive",
    "trash",
    "revoke",
];

/// Verbs that only read.
const READ: &[&str] = &[
    "get",
    "list",
    "search",
    "find",
    "fetch",
    "read",
    "view",
    "show",
    "lookup",
    "query",
    "check",
    "browse",
    "describe",
    "track",
    "status",
    "recommend",
    "suggest",
    "estimate",
    "quote",
    "preview",
    "count",
    "explore",
    "discover",
    "retrieve",
    "info",
    "details",
    "summarize",
    "summary",
    "availability",
    "available",
    "menu",
    "whoami",
    "me",
];

/// Split a tool name into lower-case words: `placeOrder`, `place_order`,
/// `place-order` and `place.order` all become `["place", "order"]`.
pub fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        current.push(c.to_ascii_lowercase());
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Words that mean money changes hands, even inside a cart or draft flow.
const SPEND: &[&str] = &[
    "place", "checkout", "pay", "payment", "purchase", "buy", "order",
];

/// Things that are cheap to change and change back.
const SCRATCH: &[&str] = &["cart", "basket", "bag", "draft", "wishlist"];

/// Classify an MCP tool. Explicit server annotations win; otherwise the verbs
/// in its name decide, and anything unrecognised needs confirmation.
pub fn classify(tool: &Value) -> ToolPolicy {
    let hint = |key: &str| {
        tool.pointer(&format!("/annotations/{key}"))
            .and_then(Value::as_bool)
    };
    if hint("readOnlyHint") == Some(true) {
        return ToolPolicy::Read;
    }
    if hint("destructiveHint") == Some(true) {
        return ToolPolicy::Confirm;
    }
    let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
    let words = words(name);
    let has = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
    let first = words.first().map(String::as_str).unwrap_or_default();

    // "get_order_status", "list_orders": the leading verb says it reads.
    if READ.contains(&first) {
        return ToolPolicy::Read;
    }
    // "add_to_cart", "remove_from_cart", "update_draft", but not "checkout_cart".
    if has(SCRATCH) && !has(SPEND) && first != "send" {
        return ToolPolicy::Change;
    }
    if has(CONSEQUENTIAL) {
        return ToolPolicy::Confirm;
    }
    // The server explicitly says it writes but is not destructive.
    if hint("readOnlyHint") == Some(false) && hint("destructiveHint") == Some(false) {
        return ToolPolicy::Change;
    }
    match first {
        "add" | "update" | "set" | "edit" | "create" | "save" | "move" | "rename" | "apply"
        | "select" | "clear" | "empty" | "complete" | "mark" | "draft" | "schedule" | "play"
        | "pause" | "resume" | "skip" | "queue" | "like" | "follow" | "start" | "stop"
        | "increase" | "decrease" | "change" => ToolPolicy::Change,
        _ => ToolPolicy::Confirm,
    }
}
