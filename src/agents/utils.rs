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
mod tests {
    use super::*;

    #[test]
    fn strips_json_markdown_fences() {
        let raw = "```json\n{\"status\": \"ok\"}\n```";
        assert_eq!(structured_json(raw), "{\"status\": \"ok\"}");
    }

    #[test]
    fn preserves_raw_json_without_fences() {
        let raw = "{\"status\": \"ok\"}";
        assert_eq!(structured_json(raw), "{\"status\": \"ok\"}");
    }

    #[test]
    fn trims_surrounding_whitespace() {
        let raw = "   \n```json\n  {\"key\": 123}  \n```  \n";
        assert_eq!(structured_json(raw), "{\"key\": 123}");
    }
}
