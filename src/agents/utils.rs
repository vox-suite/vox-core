/**
* Utility functions for text processing and JSON fence extraction in agent outputs.
*/
pub fn structured_json(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed)
}

#[cfg(test)]
#[path = "../../tests/unit/agents_utils.rs"]
mod tests;
