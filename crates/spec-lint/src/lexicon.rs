//! The three word lists the lint carries as bytes: the hedge lexicon of
//! `specs/README.md` §4, the open-set markers it bans document-wide, and the
//! model-name lexicon §7 L16 names.

use std::sync::OnceLock;

use regex::Regex;

/// The hedge lexicon, verbatim from `specs/README.md` §4. Blocking in Claims,
/// Unchanged, and Forbidden; a warning everywhere else.
pub const HEDGES: [&str; 10] = [
    "should",
    "ideally",
    "typically",
    "appropriately",
    "robust",
    "gracefully",
    "as needed",
    "if necessary",
    "reasonable",
    "properly",
];

/// The two markers that open a set the spec claimed to close.
pub const OPEN_SET_MARKERS: [&str; 2] = ["e.g.", "etc."];

/// Model families. A spec names a capability and a gate; which model answers is
/// a host-catalog fact and never spec bytes. `codex` is deliberately absent:
/// this tree ships `skills/steer-codex`, and a spec citing that path names a
/// committed directory, not a host catalog row.
pub const MODEL_NAMES: [&str; 13] = [
    "claude", "chatgpt", "gpt", "opus", "sonnet", "haiku", "fable", "gemini", "llama", "mistral",
    "qwen", "deepseek", "grok",
];

/// Hedge words present in `text`, in lexicon order.
pub fn hedges(text: &str) -> Vec<&'static str> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| HEDGES.iter().map(|word| word_pattern(word)).collect());
    HEDGES
        .iter()
        .zip(patterns)
        .filter(|(_, pattern)| pattern.is_match(text))
        .map(|(word, _)| *word)
        .collect()
}

/// Open-set markers present in `text`, in lexicon order.
pub fn open_set_markers(text: &str) -> Vec<&'static str> {
    let lowered = text.to_lowercase();
    OPEN_SET_MARKERS
        .into_iter()
        .filter(|marker| lowered.contains(marker))
        .collect()
}

/// Model names present in `text`, in lexicon order.
pub fn model_names(text: &str) -> Vec<&'static str> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    let patterns =
        PATTERNS.get_or_init(|| MODEL_NAMES.iter().map(|word| word_pattern(word)).collect());
    MODEL_NAMES
        .iter()
        .zip(patterns)
        .filter(|(_, pattern)| pattern.is_match(text))
        .map(|(word, _)| *word)
        .collect()
}

/// A case-insensitive whole-word pattern. Word boundaries treat `-` and `.` as
/// separators, so `claude-opus` and `raw/instinct-fable.md` both match.
fn word_pattern(word: &str) -> Regex {
    Regex::new(&format!("(?i)\\b{}\\b", regex::escape(word))).expect("a lexicon word compiles")
}

#[cfg(test)]
mod tests {
    use super::{hedges, model_names, open_set_markers};

    #[test]
    fn hedges_match_on_word_boundaries_only() {
        assert_eq!(
            hedges("the tool handles malformed input gracefully"),
            ["gracefully"]
        );
        assert_eq!(
            hedges("the run should retry as needed"),
            ["should", "as needed"]
        );
        assert!(hedges("a shoulder of the ladder is properly named").contains(&"properly"));
        assert!(hedges("a shoulder of the ladder").is_empty());
    }

    #[test]
    fn open_set_markers_are_caught_anywhere_in_the_line() {
        assert_eq!(open_set_markers("formats, e.g. GFM, parse"), ["e.g."]);
        assert_eq!(open_set_markers("json, yaml, etc."), ["etc."]);
        assert!(open_set_markers("the eggs are counted").is_empty());
    }

    #[test]
    fn model_names_are_caught_inside_hyphenated_identifiers() {
        assert_eq!(model_names("assigned to Sonnet"), ["sonnet"]);
        assert_eq!(model_names("raw/instinct-fable.md"), ["fable"]);
        assert!(model_names("the steward answers").is_empty());
        assert!(model_names("skills/steer-codex").is_empty());
    }
}
