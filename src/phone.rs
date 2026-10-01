use regex::Regex;
use std::sync::OnceLock;

fn e164() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^\+[1-9]\d{6,14}$").expect("valid E.164 pattern"))
}

fn dialable() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[1-9]\d{6,14}$").expect("valid digits pattern"))
}

pub fn normalize_e164(raw: &str) -> Option<String> {
    let compact: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '(' | ')' | '.'))
        .collect();
    e164()
        .is_match(&compact)
        .then(|| compact.trim_start_matches('+').to_string())
}

pub fn is_dialable_digits(digits: &str) -> bool {
    dialable().is_match(digits)
}
