//! The arrow-line parser: the shape `specs/README.md` §4 fixes for claim lines
//! and §3 reuses for `Unchanged` lines.
//!
//! ```text
//! <g>.<m> [BELIEVE:<path> — ] <condition> → <observable> [check: <attr> | gate: <id> | HUMAN-ATTENDED]
//! ```

use std::sync::OnceLock;

use regex::Regex;

/// The three oracle bindings. Exactly one per arrow line is law (§6).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingKind {
    Check,
    Gate,
    HumanAttended,
}

/// One binding token as written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    pub kind: BindingKind,
    /// The check attribute or gate id; empty for `[HUMAN-ATTENDED]`.
    pub value: String,
}

/// The provenance mark that opens a claim line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Believe {
    /// Unmarked — the DECIDE default, the spec is authoritative.
    Absent,
    /// `BELIEVE:<path> — `, the tree is authoritative.
    Mark(String),
    /// A `BELIEVE:` opening that does not carry the ` — ` separator.
    Malformed,
}

/// A parsed arrow-line body — everything after the line's id token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArrowLine {
    pub believe: Believe,
    pub bindings: Vec<Binding>,
    pub arrows: usize,
    pub condition: String,
    pub observable: String,
}

/// Split a claim id: `1.2 the rest` → `(1, 2, "the rest")`.
pub fn claim_id(text: &str) -> Option<(u32, u32, &str)> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern =
        PATTERN.get_or_init(|| Regex::new(r"^([0-9]+)\.([0-9]+) (.*)$").expect("compiles"));
    let captured = pattern.captures(text)?;
    Some((
        captured[1].parse().ok()?,
        captured[2].parse().ok()?,
        captured.get(3).expect("the body group exists").as_str(),
    ))
}

/// Split a dotted id of the `U.<m>` / `F.<m>` shape.
pub fn dotted_id(text: &str, letter: char) -> Option<(u32, &str)> {
    let rest = text.strip_prefix(letter)?.strip_prefix('.')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let body = rest[digits.len()..].strip_prefix(' ')?;
    Some((digits.parse().ok()?, body))
}

/// Parse the body of an arrow line — the text after its id token.
pub fn parse(body: &str) -> ArrowLine {
    static BINDING: OnceLock<Regex> = OnceLock::new();
    let binding = BINDING.get_or_init(|| {
        Regex::new(r"\[(check|gate): ([^\]]*)\]|\[HUMAN-ATTENDED\]").expect("compiles")
    });

    let mut bindings = Vec::new();
    let mut stripped = String::new();
    let mut cursor = 0;
    for found in binding.find_iter(body) {
        stripped.push_str(&body[cursor..found.start()]);
        cursor = found.end();
        let captured = binding
            .captures(found.as_str())
            .expect("the match captures itself");
        match captured.get(1).map(|kind| kind.as_str()) {
            Some("check") => bindings.push(Binding {
                kind: BindingKind::Check,
                value: captured[2].trim().to_owned(),
            }),
            Some("gate") => bindings.push(Binding {
                kind: BindingKind::Gate,
                value: captured[2].trim().to_owned(),
            }),
            _ => bindings.push(Binding {
                kind: BindingKind::HumanAttended,
                value: String::new(),
            }),
        }
    }
    stripped.push_str(&body[cursor..]);

    let rest = stripped.trim();
    let (believe, rest) = match rest.strip_prefix("BELIEVE:") {
        None => (Believe::Absent, rest),
        Some(marked) => match marked.split_once(" — ") {
            Some((path, tail)) if !path.trim().is_empty() && !path.contains(' ') => {
                (Believe::Mark(path.trim().to_owned()), tail)
            }
            _ => (Believe::Malformed, marked),
        },
    };

    let arrows = rest.matches('→').count();
    let (condition, observable) = match rest.split_once('→') {
        Some((condition, observable)) => (condition.trim(), observable.trim()),
        None => (rest.trim(), ""),
    };

    ArrowLine {
        believe,
        bindings,
        arrows,
        condition: condition.to_owned(),
        observable: observable.to_owned(),
    }
}

/// The backticked spans of a line, in order, without their backticks.
pub fn backticked(text: &str) -> Vec<String> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| Regex::new("`([^`]*)`").expect("compiles"));
    pattern
        .captures_iter(text)
        .map(|captured| captured[1].to_owned())
        .filter(|span| !span.trim().is_empty())
        .collect()
}

/// The same text with every backticked span blanked, so lexical scans read
/// prose only.
pub fn without_backticks(text: &str) -> String {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| Regex::new("`[^`]*`").expect("compiles"));
    pattern.replace_all(text, " ").into_owned()
}

/// Numerals that carry no provenance. A numeral welded to an id — `R2`, `F.2`,
/// `UNKNOWN-1`, `#r2` — is a cross-reference and exempt, as is a dotted pair
/// (`1.2`, the claim-id shape); so is anything inside backticks, which `L8`
/// judges instead.
pub fn unsourced_numerals(text: &str) -> Vec<String> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    static DOTTED: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| Regex::new("[0-9]+").expect("compiles"));
    let dotted = DOTTED.get_or_init(|| Regex::new(r"[0-9]+\.[0-9]+").expect("compiles"));
    let prose = dotted
        .replace_all(&without_backticks(text), " ")
        .into_owned();
    pattern
        .find_iter(&prose)
        .filter(|found| {
            prose[..found.start()]
                .chars()
                .next_back()
                .is_none_or(|previous| {
                    !previous.is_ascii_alphanumeric() && !".-#_/".contains(previous)
                })
        })
        .map(|found| found.as_str().to_owned())
        .collect()
}

/// The word after an ` and ` that joins two verbs, when the observable holds
/// one. Verb shape is `-s`, `-es`, or `-ed`; an enumeration (a comma before the
/// ` and `) is a list, not a second claim.
pub fn and_joined_verb(observable: &str) -> Option<String> {
    let prose = without_backticks(observable);
    let mut cursor = 0;
    while let Some(offset) = prose[cursor..].find(" and ") {
        let at = cursor + offset;
        let left = &prose[..at];
        let right = &prose[at + " and ".len()..];
        let next = right.split_whitespace().next().unwrap_or_default();
        if verb_shaped(next) && !left.contains(',') && left.split_whitespace().any(verb_shaped) {
            return Some(trim_word(next).to_owned());
        }
        cursor = at + " and ".len();
    }
    None
}

fn verb_shaped(word: &str) -> bool {
    let word = trim_word(word);
    if word.len() < 3
        || !word
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        return false;
    }
    (word.ends_with('s') && !word.ends_with("ss")) || word.ends_with("ed")
}

fn trim_word(word: &str) -> &str {
    word.trim_matches(|character: char| !character.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::{
        and_joined_verb, backticked, claim_id, dotted_id, parse, unsourced_numerals, Believe,
        BindingKind,
    };

    #[test]
    fn a_claim_line_splits_into_id_condition_observable_and_binding() {
        let (group, index, body) =
            claim_id("1.2 a defect appears → the run exits 2 (given). [gate: cargo-tests]")
                .expect("the id parses");
        assert_eq!((group, index), (1, 2));

        let line = parse(body);
        assert_eq!(line.believe, Believe::Absent);
        assert_eq!(line.arrows, 1);
        assert_eq!(line.condition, "a defect appears");
        assert_eq!(line.observable, "the run exits 2 (given).");
        assert_eq!(line.bindings.len(), 1);
        assert_eq!(line.bindings[0].kind, BindingKind::Gate);
        assert_eq!(line.bindings[0].value, "cargo-tests");
    }

    #[test]
    fn a_believe_mark_is_lifted_off_the_condition() {
        let (_, _, body) = claim_id(
            "3.3 BELIEVE:test/fleet-gate.sh — the ladder runs it → the head is graded. [check: spec-lint]",
        )
        .expect("the id parses");
        let line = parse(body);
        assert_eq!(line.believe, Believe::Mark("test/fleet-gate.sh".to_owned()));
        assert_eq!(line.condition, "the ladder runs it");

        let malformed = parse("BELIEVE:test/fleet-gate.sh the ladder runs it → it is graded.");
        assert_eq!(malformed.believe, Believe::Malformed);
    }

    #[test]
    fn zero_and_two_bindings_are_both_visible_to_the_caller() {
        assert!(parse("a → b (given).").bindings.is_empty());
        assert_eq!(
            parse("a → b (given). [gate: cargo-tests] [check: spec-lint]")
                .bindings
                .len(),
            2
        );
        assert_eq!(
            parse("a → b. [HUMAN-ATTENDED]").bindings[0].kind,
            BindingKind::HumanAttended
        );
    }

    #[test]
    fn a_dotted_id_parses_for_unchanged_and_forbidden_lines() {
        assert_eq!(dotted_id("U.1 a → b.", 'U'), Some((1, "a → b.")));
        assert_eq!(
            dotted_id("F.12 Do not shout.", 'F'),
            Some((12, "Do not shout."))
        );
        assert_eq!(dotted_id("F12 Do not shout.", 'F'), None);
        assert_eq!(dotted_id("U.1", 'U'), None);
    }

    #[test]
    fn only_numerals_without_provenance_or_an_id_are_reported() {
        assert_eq!(
            unsourced_numerals("a run over 30 files → exit 0."),
            ["30", "0"]
        );
        assert!(unsourced_numerals("either side of 1.1/1.2 flips").is_empty());
        assert!(unsourced_numerals("claims R1 and S2 cite A22 and #r2").is_empty());
        assert!(unsourced_numerals("the tag `v0.1.0` is cut").is_empty());
    }

    #[test]
    fn an_and_joined_pair_of_verbs_is_a_split_defect_but_a_list_is_not() {
        assert_eq!(
            and_joined_verb("the tool validates it and writes the report."),
            Some("writes".to_owned())
        );
        assert_eq!(
            and_joined_verb("the run exits 2 and the map matches."),
            None
        );
        assert_eq!(
            and_joined_verb("the report lists paths, counts and totals."),
            None
        );
    }

    #[test]
    fn backticked_spans_come_back_without_their_fences() {
        assert_eq!(
            backticked("`spec-lint --mode check` over `specs/zeta`"),
            ["spec-lint --mode check", "specs/zeta"]
        );
        assert!(backticked("no spans here").is_empty());
    }
}
