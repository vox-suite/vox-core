//! Chooses which connected apps' tools the agent gets for one turn.
//!
//! Every tool definition costs prompt tokens and latency, and a model choosing
//! among many similar tools picks worse, so only apps relevant to the turn are
//! offered. Relevance is lexical and local (no extra model or network call):
//! the app's own name, words shared with the app's tool names and
//! descriptions (weighted by how distinctive they are across the user's
//! apps), the recent conversation, recent use, and pending confirmations.

use chrono::{DateTime, Duration, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::policy::words;

/// Below this many tools in total, offering everything is cheaper than
/// risking a missed app.
const SMALL_TOOLSET: usize = 16;
const MAX_APPS: usize = 3;
const MAX_TOOLS: usize = 48;
const MIN_SCORE: f64 = 1.5;
const NAME_MATCH: f64 = 12.0;
/// Weight of earlier messages, newest first: a bare "yes" leans on the
/// message just before it far more than on older ones.
const HISTORY_WEIGHTS: [f64; 6] = [0.9, 0.6, 0.4, 0.3, 0.2, 0.2];
const RECENT_USE: f64 = 3.0;
const RECENT_USE_WINDOW_MINUTES: i64 = 30;

pub struct AppProfile {
    pub extension_id: Uuid,
    pub app_key: String,
    pub app_name: String,
    pub tools: Vec<Value>,
    pub last_used_at: Option<DateTime<Utc>>,
}

pub struct Selection {
    pub selected: Vec<Uuid>,
    /// Connected apps left out this turn, so the agent can mention them.
    pub omitted: Vec<String>,
}

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "could", "do", "for", "from", "get",
    "give", "have", "i", "in", "is", "it", "me", "my", "of", "on", "or", "please", "some", "that",
    "the", "this", "to", "use", "want", "was", "we", "what", "when", "with", "you", "your",
    "would", "will", "tool", "tools", "user", "users", "app", "id", "ids", "data", "return",
    "returns", "given", "using", "based", "also", "any", "all",
];

/// Everyday words mapped to the vocabulary apps use in their tool docs.
const EXPANSIONS: &[(&str, &[&str])] = &[
    ("hungry", &["food", "restaurant", "order"]),
    ("dinner", &["food", "restaurant"]),
    ("lunch", &["food", "restaurant"]),
    ("breakfast", &["food", "restaurant"]),
    ("eat", &["food", "restaurant"]),
    ("biryani", &["food", "dish", "restaurant"]),
    ("pizza", &["food", "dish", "restaurant"]),
    ("deliver", &["delivery", "order"]),
    ("grocery", &["product", "item", "cart"]),
    ("groceri", &["product", "item", "cart"]),
    ("milk", &["grocery", "product"]),
    ("vegetable", &["grocery", "product"]),
    ("fruit", &["grocery", "product"]),
    ("table", &["reservation", "book", "restaurant"]),
    ("reservation", &["book", "table", "restaurant"]),
    ("note", &["page", "notion"]),
    ("todo", &["task"]),
    ("remind", &["task"]),
    ("song", &["track", "music", "play"]),
    ("music", &["track", "play", "playlist"]),
    ("playlist", &["track", "music"]),
    ("meeting", &["event", "calendar"]),
    ("schedule", &["event", "calendar"]),
    ("email", &["mail", "message", "thread"]),
    ("mail", &["email", "message"]),
    ("design", &["canva", "template"]),
    ("poster", &["design", "template"]),
];

/// Lower-case, split, drop stopwords, and stem lightly.
fn terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in text
        .split(|c: char| !c.is_alphanumeric())
        .flat_map(words)
        .filter(|w| w.len() > 1)
    {
        if STOPWORDS.contains(&word.as_str()) {
            continue;
        }
        out.push(stem(&word));
    }
    out
}

fn stem(word: &str) -> String {
    let w = word.to_string();
    for suffix in ["ing", "ies", "ed", "es", "s"] {
        if w.len() > suffix.len() + 3 && w.ends_with(suffix) {
            return w[..w.len() - suffix.len()].to_string();
        }
    }
    w
}

fn expand(query: Vec<String>) -> Vec<String> {
    let mut out = query.clone();
    for term in &query {
        if let Some((_, extra)) = EXPANSIONS.iter().find(|(k, _)| stem(k) == *term) {
            out.extend(extra.iter().map(|e| stem(e)));
        }
    }
    out
}

struct Indexed {
    names: HashSet<String>,
    vocab: HashMap<String, f64>,
}

fn index(app: &AppProfile) -> Indexed {
    let mut names: HashSet<String> = terms(&app.app_name).into_iter().collect();
    names.extend(terms(&app.app_key));
    let mut vocab: HashMap<String, f64> = HashMap::new();
    let mut add = |text: &str, weight: f64| {
        for term in terms(text) {
            let entry = vocab.entry(term).or_insert(0.0);
            *entry = entry.max(weight);
        }
    };
    for tool in &app.tools {
        if let Some(name) = tool.get("name").and_then(Value::as_str) {
            add(name, 1.0);
        }
        if let Some(title) = tool.get("title").and_then(Value::as_str) {
            add(title, 1.0);
        }
        if let Some(description) = tool.get("description").and_then(Value::as_str) {
            add(description, 0.5);
        }
    }
    Indexed { names, vocab }
}

/// `history` is the recent conversation, newest message first.
pub fn select(
    apps: &[AppProfile],
    message: &str,
    history: &[&str],
    pinned: &HashSet<Uuid>,
    now: DateTime<Utc>,
) -> Selection {
    let total_tools: usize = apps.iter().map(|a| a.tools.len()).sum();
    if total_tools <= SMALL_TOOLSET {
        return Selection {
            selected: apps.iter().map(|a| a.extension_id).collect(),
            omitted: Vec::new(),
        };
    }

    let indexed: Vec<Indexed> = apps.iter().map(index).collect();
    let mut document_frequency: HashMap<&str, usize> = HashMap::new();
    for app in &indexed {
        for term in app.vocab.keys() {
            *document_frequency.entry(term.as_str()).or_insert(0) += 1;
        }
    }
    let n = apps.len() as f64;
    let idf = |term: &str| {
        let df = *document_frequency.get(term).unwrap_or(&0) as f64;
        (1.0 + n / (1.0 + df)).ln() + 0.2
    };
    let score_text = |app: &Indexed, query: &[String]| -> f64 {
        let mut seen = HashSet::new();
        let mut score = 0.0;
        for term in query {
            if !seen.insert(term.as_str()) {
                continue;
            }
            if app.names.contains(term) {
                score += NAME_MATCH;
            } else if let Some(weight) = app.vocab.get(term) {
                score += weight * idf(term);
            }
        }
        score
    };

    let current = expand(terms(message));
    let past: Vec<Vec<String>> = history
        .iter()
        .take(HISTORY_WEIGHTS.len())
        .map(|text| expand(terms(text)))
        .collect();
    let recent = now - Duration::minutes(RECENT_USE_WINDOW_MINUTES);

    let mut scored: Vec<(usize, f64)> = apps
        .iter()
        .zip(&indexed)
        .enumerate()
        .map(|(i, (app, idx))| {
            let mut score = score_text(idx, &current);
            for (weight, text) in HISTORY_WEIGHTS.iter().zip(&past) {
                score += weight * score_text(idx, text);
            }
            if app.last_used_at.is_some_and(|at| at > recent) {
                score += RECENT_USE;
            }
            (i, score)
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut selected = Vec::new();
    let mut budget = MAX_TOOLS;
    for app in apps.iter().filter(|a| pinned.contains(&a.extension_id)) {
        selected.push(app.extension_id);
        budget = budget.saturating_sub(app.tools.len());
    }
    for (i, score) in &scored {
        let app = &apps[*i];
        if selected.len() >= MAX_APPS || *score < MIN_SCORE {
            break;
        }
        if selected.contains(&app.extension_id) || app.tools.len() > budget {
            continue;
        }
        budget -= app.tools.len();
        selected.push(app.extension_id);
    }
    let omitted = apps
        .iter()
        .filter(|a| !selected.contains(&a.extension_id))
        .map(|a| a.app_name.clone())
        .collect();
    Selection { selected, omitted }
}
