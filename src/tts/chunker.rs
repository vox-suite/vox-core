/**
 * Splits streaming LLM text tokens into natural sentences/clauses for low-latency TTS.
 */
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Default)]
pub struct SentenceChunker {
    buffer: String,
}

impl SentenceChunker {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        self.drain_sentences(false)
    }

    pub fn flush(&mut self) -> Option<String> {
        let remaining = self.buffer.trim().to_string();
        self.buffer.clear();
        if remaining.is_empty() || !remaining.chars().any(|c| c.is_alphabetic()) {
            None
        } else {
            Some(remaining)
        }
    }

    fn drain_sentences(&mut self, flush: bool) -> Vec<String> {
        let mut sentences = Vec::new();
        while let Some(index) = self.find_sentence_boundary() {
            let sentence = self.buffer[..index].trim().to_string();
            self.buffer = self.buffer[index..].trim_start().to_string();
            if !sentence.is_empty() && sentence.chars().any(|c| c.is_alphabetic()) {
                sentences.push(sentence);
            }
        }
        if let Some(final_sentence) = flush.then(|| self.flush()).flatten() {
            sentences.push(final_sentence);
        }
        sentences
    }

    fn find_sentence_boundary(&self) -> Option<usize> {
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        if len == 0 {
            return None;
        }

        let mut word_count = 0usize;
        let mut last_clause_boundary: Option<usize> = None;

        for (idx, bound) in self.buffer.split_word_bound_indices() {
            if bound.chars().any(char::is_alphanumeric) {
                word_count += 1;
            }
            if (bound == "," || bound == ";" || bound == ":") && word_count >= 7 {
                let end = idx + bound.len();
                if end < len && bytes[end].is_ascii_whitespace() {
                    last_clause_boundary = Some(end);
                }
            }
        }

        for i in 0..len {
            let b = bytes[i];

            if b == b'.' || b == b'!' || b == b'?' {
                let next_is_boundary = if i + 1 < len {
                    bytes[i + 1].is_ascii_whitespace()
                } else {
                    if b == b'!' || b == b'?' {
                        true
                    } else if b == b'.' {
                        let prev_digit = i > 0 && bytes[i - 1].is_ascii_digit();
                        let prev_dot = i > 0 && bytes[i - 1] == b'.';
                        let prefix = &self.buffer[..i];
                        !prev_digit && !prev_dot && !is_abbreviation(prefix)
                    } else {
                        false
                    }
                };

                if next_is_boundary {
                    let current_words = self.buffer[..i].unicode_words().count();
                    if current_words >= 14 && last_clause_boundary.is_some() {
                        return last_clause_boundary;
                    }
                    if b == b'.' && i > 0 && i + 1 < len {
                        let prev = bytes[i - 1];
                        let next = bytes[i + 1];
                        if prev.is_ascii_digit() && next.is_ascii_digit() {
                            continue;
                        }
                    }
                    if b == b'.' && is_abbreviation(&self.buffer[..i]) {
                        continue;
                    }
                    return Some(i + 1);
                }
            }
        }

        let total_words = self.buffer.unicode_words().count();
        if total_words >= 14 && last_clause_boundary.is_some() {
            return last_clause_boundary;
        }
        None
    }
}

pub fn is_abbreviation(prefix: &str) -> bool {
    let last_word = prefix
        .unicode_words()
        .next_back()
        .map(|w| w.to_lowercase())
        .unwrap_or_default();

    matches!(
        last_word.as_str(),
        "mr" | "mrs"
            | "ms"
            | "dr"
            | "prof"
            | "sr"
            | "jr"
            | "vs"
            | "etc"
            | "eg"
            | "ie"
            | "approx"
            | "appt"
            | "dept"
            | "est"
            | "min"
            | "sec"
            | "tel"
            | "temp"
            | "vet"
            | "vol"
            | "yd"
            | "st"
            | "ave"
            | "rd"
            | "blvd"
            | "ln"
            | "ct"
            | "pl"
            | "ste"
            | "apt"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sentence_chunker_splits_sentences() {
        let mut chunker = SentenceChunker::new();
        let chunks = chunker.push("Hello there! How can I help you today? ");
        assert_eq!(chunks, vec!["Hello there!", "How can I help you today?"]);
        assert_eq!(chunker.flush(), None);
    }

    #[test]
    fn test_sentence_chunker_handles_streaming_tokens() {
        let mut chunker = SentenceChunker::new();
        assert_eq!(chunker.push("Hello"), Vec::<String>::new());
        assert_eq!(chunker.push(" world. "), vec!["Hello world."]);
        assert_eq!(chunker.push("This is a test."), vec!["This is a test."]);
        assert_eq!(chunker.flush(), None);
    }

    #[test]
    fn test_sentence_chunker_ignores_abbreviations() {
        let mut chunker = SentenceChunker::new();
        let chunks = chunker.push("Dr. Smith went to the market. ");
        assert_eq!(chunks, vec!["Dr. Smith went to the market."]);
    }
}
