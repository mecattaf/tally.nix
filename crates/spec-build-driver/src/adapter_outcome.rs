//! The adapter-terminal outcome class: a wall that names itself.
//!
//! An agent lane can end for a reason the agent had no say in. A provider quota
//! refuses the turn, the stream stops, and the harness writes the refusal as a
//! stdout stream event -- so stderr is empty, no final message is ever
//! projected, and the lane exits carrying nothing. The machinery then reads
//! that emptiness the only way it could: the advisory projection never landed,
//! the node is classified `result-projection-timeout`, the failure looks
//! transient, and the bounded machinery-retry budget is spent re-dispatching
//! against a wall whose own message states the hour it lifts. That is V-16 of
//! `specs/substrate/evidence/vestige-sweep.md`, and the truth it costs attempts
//! to guess at is sitting in the capture archive the whole time.
//!
//! So this module reads the archive. Every preset that can state a terminal
//! condition declares a `terminal` capture beside `finalMessage`
//! (`nix/lib/adapters.nix`), naming the event genre its harness ends on and the
//! path to the human-readable text inside it. When that capture resolves, the
//! adapter has stated the outcome itself, and a stated outcome outranks an
//! inferred one: the class is `adapter-terminal` no matter what the machinery's
//! own fallback code says, and it dispatches nothing further.
//!
//! Two properties are worth naming because they are what makes this
//! deterministic rather than a second opinion:
//!
//! * **Scraped, not inferred.** Nothing here matches on an adapter name, a
//!   status code, or the words in a message. The only question asked is
//!   whether the adapter's own declared capture resolved. A harness whose
//!   preset declares no `terminal` capture can never produce this class, and
//!   adding one is a declaration in the catalog, never a match arm here.
//! * **It stops the ladder.** An adapter-terminal outcome buys no machinery
//!   retry and settles the steering verdict to `blocked` without consulting
//!   the judge. It is kin to the judge's own `blocked` verdict, but no
//!   judgment slot is needed: the adapter stated it.
//!
//! The same read answers the second question an operator would otherwise
//! reconstruct by hand -- what did this lane spend. The token totals come from
//! the adapter's declared usage mapping through [`tally_core::usage`], so they
//! are the harness's own figures under logical names, and they ride the same
//! envelope whether or not the lane hit a wall.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tally_core::adapters::{AdapterConfig, AdapterEngine, ScrapeResult};
use tally_core::executor::ExecutionPaths;
use tally_core::usage::{self, UsageObservation};

use crate::error::{DriverError, Result};
use crate::json::Json;

/// Capture name a preset declares for its harness's own terminal event.
pub(crate) const TERMINAL_CAPTURE: &str = "terminal";

/// Logical field naming where the human-readable text sits inside the
/// captured terminal event. A preset declares ordered candidate paths for it
/// exactly as the usage mapping declares token paths, so a harness that nests
/// the text differently is a catalog edit rather than a Rust change.
pub(crate) const TERMINAL_MESSAGE_FIELD: &str = "terminalMessage";

/// The class an adapter's own terminal event produces.
pub(crate) const ADAPTER_TERMINAL_CLASS: &str = "adapter-terminal";

/// What a lane is called when nothing terminal was stated and the machinery
/// offered no code of its own either.
const UNCLASSIFIED_CLASS: &str = "unclassified";

/// One lane's outcome envelope: what the adapter said about how it ended, and
/// what it spent saying it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LaneOutcome {
    adapter: String,
    /// The text of the adapter's own terminal event, when it stated one.
    /// Present is the whole classification: there is no separate flag that
    /// could disagree with it.
    terminal_message: Option<String>,
    /// The classification the machinery reached without reading the stream --
    /// `result-projection-timeout` and its kin. Retained rather than
    /// discarded so a receipt can say which reading it displaced.
    fallback_code: Option<String>,
    usage: UsageObservation,
}

impl LaneOutcome {
    /// The class this lane settles to. An adapter's own statement outranks
    /// the machinery's inference; that ranking is this one expression.
    pub(crate) fn class(&self) -> &str {
        if self.terminal_message.is_some() {
            ADAPTER_TERMINAL_CLASS
        } else {
            self.fallback_code.as_deref().unwrap_or(UNCLASSIFIED_CLASS)
        }
    }

    pub(crate) fn is_adapter_terminal(&self) -> bool {
        self.terminal_message.is_some()
    }

    /// Whether the retry ladder may dispatch anything further for this
    /// attempt. An adapter-terminal outcome is not a machinery fault and not
    /// a statement about the work, so neither budget answers it.
    pub(crate) fn dispatches_retry(&self) -> bool {
        !self.is_adapter_terminal()
    }

    #[cfg(test)]
    fn terminal_message(&self) -> Option<&str> {
        self.terminal_message.as_deref()
    }

    /// The durable envelope. This is the artifact the class exists to produce:
    /// a lane that would otherwise exit with nothing exits with the adapter's
    /// own sentence and its own token figures. [`Self::note`] renders it into
    /// the receipt an operator reads, block included, so the structured form
    /// and the prose can never disagree.
    fn envelope(&self) -> Json {
        Json::object([
            ("adapter", Json::from(self.adapter.as_str())),
            ("class", Json::from(self.class())),
            ("terminal", Json::from(self.is_adapter_terminal())),
            (
                "message",
                self.terminal_message
                    .as_deref()
                    .map_or(Json::Null, Json::from),
            ),
            (
                "displacedClass",
                self.fallback_code
                    .as_deref()
                    .filter(|_| self.is_adapter_terminal())
                    .map_or(Json::Null, Json::from),
            ),
            ("dispatchesRetry", Json::from(self.dispatches_retry())),
            ("tokens", self.token_envelope()),
        ])
    }

    /// The token half of the envelope. The three usage states stay distinct
    /// here for the same reason [`tally_core::usage`] keeps them distinct: a
    /// harness that reported nothing and an adapter that declared no scrape
    /// are different facts, and neither is a zero.
    fn token_envelope(&self) -> Json {
        match &self.usage {
            UsageObservation::NotDeclared => {
                Json::object([("observation", Json::from("not-declared"))])
            }
            UsageObservation::NotReported => {
                Json::object([("observation", Json::from("not-reported"))])
            }
            UsageObservation::Reported(breakdown) => {
                let mut fields = vec![("observation".to_owned(), Json::from("reported"))];
                for (name, value) in [
                    ("inputTokens", breakdown.input_tokens),
                    ("cacheReadTokens", breakdown.cache_read_tokens),
                    ("cacheWriteTokens", breakdown.cache_write_tokens),
                    ("outputTokens", breakdown.output_tokens),
                    ("reasoningTokens", breakdown.reasoning_tokens),
                ] {
                    if let Some(value) = value {
                        fields.push((name.to_owned(), count(value)));
                    }
                }
                if let Some(total) = &breakdown.total_tokens {
                    fields.push(("totalTokens".to_owned(), count(total.value)));
                }
                Json::object(fields)
            }
        }
    }

    /// The sentence a durable receipt carries. A retry reason and a diagnosis
    /// are both public prose, so the envelope reaches an operator as text
    /// rather than as a field only a reader with the schema can decode.
    pub(crate) fn note(&self) -> String {
        let mut lines = Vec::new();
        if let Some(message) = &self.terminal_message {
            lines.push(format!(
                "Adapter-terminal outcome: the {:?} stream stated its own terminal condition, so the retry ladder stops here -- no in-epoch retry and no machinery retry. The adapter said:",
                self.adapter
            ));
            lines.push(String::new());
            for line in message.lines() {
                lines.push(format!("    {line}"));
            }
            if let Some(displaced) = &self.fallback_code {
                lines.push(String::new());
                lines.push(format!(
                    "Read from this lane's own capture, which outranks the machinery classification {displaced:?}."
                ));
            }
        }
        if let Some(spend) = self.spend_sentence() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(spend);
        }
        if lines.is_empty() {
            // Nothing was stated and nothing was spent: an empty note, so a
            // receipt that has nothing to add says nothing rather than
            // carrying an envelope of absences.
            return String::new();
        }
        // The envelope itself, beside its own prose. The prose is what an
        // operator reads mid-incident; the block is what the next reader of
        // this durable receipt can parse instead of reconstructing a session
        // ledger by hand.
        lines.extend([
            String::new(),
            "```json".to_owned(),
            self.envelope().stringify(),
            "```".to_owned(),
        ]);
        lines.join("\n")
    }

    /// The spend line, stated only when the harness actually reported
    /// figures. An adapter that declared no usage scrape gets silence, not a
    /// row of zeroes.
    fn spend_sentence(&self) -> Option<String> {
        let UsageObservation::Reported(breakdown) = &self.usage else {
            return None;
        };
        let mut parts = Vec::new();
        for (label, value) in [
            ("input", breakdown.input_tokens),
            ("cache read", breakdown.cache_read_tokens),
            ("cache write", breakdown.cache_write_tokens),
            ("output", breakdown.output_tokens),
            ("reasoning", breakdown.reasoning_tokens),
        ] {
            if let Some(value) = value {
                parts.push(format!("{label} {value}"));
            }
        }
        if let Some(total) = &breakdown.total_tokens {
            parts.push(format!("total {}", total.value));
        }
        if parts.is_empty() {
            return None;
        }
        Some(format!(
            "Token spend scraped from this lane's stream: {}.",
            parts.join(", ")
        ))
    }

    #[cfg(test)]
    fn breakdown(&self) -> Option<&tally_core::usage::UsageBreakdown> {
        match &self.usage {
            UsageObservation::Reported(breakdown) => Some(breakdown),
            _ => None,
        }
    }
}

fn count(value: u64) -> Json {
    Json::Number(value.to_string())
}

/// Classify one lane from the bytes its adapter wrote.
///
/// `fallback_code` is the machinery's own classification of the same failure.
/// It is an input rather than a competitor: when the stream states a terminal
/// condition the fallback is recorded as displaced, and when it does not the
/// fallback stands untouched.
///
/// Production always classifies from the retained archive
/// ([`classify_paths`]); this entry point exists so the committed capture
/// corpus can be driven from its own bytes without a temporary file standing
/// between the fixture and the assertion.
#[cfg(test)]
fn classify_text(
    adapter_name: &str,
    adapter: &AdapterConfig,
    stdout: &str,
    stderr: &str,
    fallback_code: Option<&str>,
) -> Result<LaneOutcome> {
    let catalog = BTreeMap::from([(adapter_name.to_owned(), adapter.clone())]);
    let captures = AdapterEngine::new(&catalog)
        .scrape_text(adapter_name, stdout, stderr)
        .map_err(|error| {
            DriverError::new(format!(
                "cannot scrape lane capture for adapter {adapter_name:?}: {error}"
            ))
        })?;
    Ok(outcome(adapter_name, adapter, &captures, fallback_code))
}

/// Classify one lane from its retained capture files.
///
/// The read is [`AdapterEngine::scrape_paths`]'s: hardened against a replaced
/// capture path and bounded by the same trace-read ceiling every other reader
/// of this archive obeys, so this path introduces no bound of its own.
pub(crate) fn classify_paths(
    adapter_name: &str,
    adapter: &AdapterConfig,
    stdout: &Path,
    stderr: Option<&Path>,
    fallback_code: Option<&str>,
) -> Result<LaneOutcome> {
    let catalog = BTreeMap::from([(adapter_name.to_owned(), adapter.clone())]);
    // `scrape_paths` opens only the two streams a capture can be declared
    // against. The remaining members name artifacts no scrape reads; they are
    // the empty path rather than a plausible-looking one so that nothing here
    // can be mistaken for evidence about them.
    let paths = ExecutionPaths {
        stdout: stdout.to_path_buf(),
        stderr: stderr.map(Path::to_path_buf).unwrap_or_default(),
        failure_stderr: PathBuf::new(),
        exit_record: PathBuf::new(),
        capture_generation: PathBuf::new(),
    };
    let captures = AdapterEngine::new(&catalog)
        .scrape_paths(adapter_name, &paths)
        .map_err(|error| {
            DriverError::new(format!(
                "cannot read lane capture for adapter {adapter_name:?}: {error}"
            ))
        })?;
    Ok(outcome(adapter_name, adapter, &captures, fallback_code))
}

fn outcome(
    adapter_name: &str,
    adapter: &AdapterConfig,
    captures: &ScrapeResult,
    fallback_code: Option<&str>,
) -> LaneOutcome {
    LaneOutcome {
        adapter: adapter_name.to_owned(),
        terminal_message: terminal_message(adapter, captures),
        fallback_code: fallback_code.map(str::to_owned),
        usage: usage::observe(adapter, captures),
    }
}

/// Read the terminal event's text through the preset's own declaration.
///
/// A capture that resolved but states no readable text is not a terminal
/// outcome. That is deliberate: this class stops a retry ladder, and it may
/// only do so on a sentence an operator can read, never on the bare fact that
/// some event matched.
fn terminal_message(adapter: &AdapterConfig, captures: &ScrapeResult) -> Option<String> {
    let capture = adapter.scrape.get(TERMINAL_CAPTURE)?;
    let root = captures.captures.get(TERMINAL_CAPTURE)?;
    let declared = capture.fields.get(TERMINAL_MESSAGE_FIELD);
    let paths: Vec<&str> = declared.map_or_else(
        // No declared path means the capture selected the text itself.
        || vec!["$"],
        |paths| paths.iter().map(String::as_str).collect(),
    );
    for path in paths {
        let Some(value) = resolve_path(root, path) else {
            continue;
        };
        let Value::String(text) = value else {
            continue;
        };
        let text = text.trim();
        if !text.is_empty() {
            return Some(text.to_owned());
        }
    }
    None
}

/// One declared path inside a captured value, in the grammar
/// [`tally_core::adapters::ScrapeCapture::fields`] documents: `$` (or the
/// empty string) is the captured value itself, anything else is
/// dot-separated object keys with numeric segments indexing arrays.
fn resolve_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let trimmed = path.strip_prefix('$').unwrap_or(path);
    let trimmed = trimmed.strip_prefix('.').unwrap_or(trimmed);
    if trimmed.is_empty() {
        return Some(root);
    }
    let mut current = root;
    for segment in trimmed.split('.') {
        current = match current {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// The committed snapshot of the preset catalog in `nix/lib/adapters.nix`.
    ///
    /// It is generated from that file (`nix eval` over `presets`, filtered to
    /// the adapters these cases exercise) rather than hand-typed, and it is
    /// embedded rather than read at runtime so the packaged build carries it.
    /// The nix expression stays the authority; this is a copy of what it
    /// evaluates to.
    const CATALOG: &str =
        include_str!("../../../test/fixtures/spec-build/adapter-terminal-catalog.json");

    const CLAUDE_CODE_QUOTA: &str =
        include_str!("../../../test/fixtures/traces/claude-code-quota.jsonl");
    const CODEX_QUOTA: &str = include_str!("../../../test/fixtures/traces/codex-quota.jsonl");
    const PI_QUOTA: &str = include_str!("../../../test/fixtures/traces/pi-quota.jsonl");
    const PI_NORMAL: &str = include_str!("../../../test/fixtures/traces/pi.jsonl");

    /// A claude-code stream that ended the way one is supposed to: a `result`
    /// event, and no `error` event anywhere. The committed
    /// `claude-code.jsonl` is not usable here -- it ends in a deliberately
    /// malformed line, which is a different fixture's subject.
    const CLAUDE_CODE_NORMAL: &str = concat!(
        r#"{"type":"system","subtype":"init","session_id":"claude-session-healthy"}"#,
        "\n",
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}],"usage":{"input_tokens":11,"output_tokens":3}}}"#,
        "\n",
        r#"{"type":"result","subtype":"success","result":"Done.","usage":{"input_tokens":11,"output_tokens":3}}"#,
        "\n",
    );

    fn catalog() -> BTreeMap<String, AdapterConfig> {
        serde_json::from_str(CATALOG).expect("the committed catalog snapshot parses")
    }

    fn adapter(name: &str) -> AdapterConfig {
        let mut catalog = catalog();
        catalog
            .remove(name)
            .unwrap_or_else(|| panic!("the catalog snapshot declares {name:?}"))
    }

    fn classify(name: &str, stdout: &str, fallback: Option<&str>) -> LaneOutcome {
        classify_text(name, &adapter(name), stdout, "", fallback)
            .expect("a committed fixture scrapes")
    }

    /// The whole point of the class, stated once per adapter: the wall names
    /// itself, the reset time it names survives into the envelope, and the
    /// projection-timeout reading the machinery reached without the capture
    /// is displaced rather than believed.
    #[test]
    fn a_quota_walled_claude_code_capture_is_adapter_terminal_not_projection_timeout() {
        let outcome = classify(
            "claude-code",
            CLAUDE_CODE_QUOTA,
            Some("result-projection-timeout"),
        );
        assert_eq!(outcome.class(), ADAPTER_TERMINAL_CLASS);
        assert!(outcome.is_adapter_terminal());
        let message = outcome.terminal_message().expect("the wall stated itself");
        assert!(message.contains("usage limit"), "{message}");
        assert!(message.contains("Aug 20th"), "{message}");
        let envelope = outcome.envelope();
        assert_eq!(
            envelope.get("class").and_then(Json::as_str),
            Some(ADAPTER_TERMINAL_CLASS)
        );
        assert_eq!(
            envelope.get("displacedClass").and_then(Json::as_str),
            Some("result-projection-timeout")
        );
    }

    #[test]
    fn a_quota_walled_codex_capture_is_adapter_terminal_through_its_turn_failed_genre() {
        let outcome = classify("codex", CODEX_QUOTA, Some("result-projection-timeout"));
        assert_eq!(outcome.class(), ADAPTER_TERMINAL_CLASS);
        let message = outcome.terminal_message().expect("the wall stated itself");
        assert!(message.contains("usage limit"), "{message}");
        assert!(message.contains("2026-08-20T13:00:00Z"), "{message}");
    }

    /// codex declares two genres and the committed capture can only carry
    /// one, so the other candidate path is exercised against the stream-level
    /// shape the preset names beside it.
    #[test]
    fn a_codex_stream_level_error_event_is_adapter_terminal() {
        let stream = concat!(
            r#"{"type":"thread.started","thread_id":"codex-thread-error"}"#,
            "\n",
            r#"{"type":"error","message":"stream error: usage limit reached"}"#,
            "\n",
        );
        let outcome = classify("codex", stream, None);
        assert_eq!(outcome.class(), ADAPTER_TERMINAL_CLASS);
        assert_eq!(
            outcome.terminal_message(),
            Some("stream error: usage limit reached")
        );
    }

    #[test]
    fn a_quota_walled_pi_capture_is_adapter_terminal_and_keeps_the_last_valid_turn() {
        let outcome = classify("pi", PI_QUOTA, Some("result-projection-timeout"));
        assert_eq!(outcome.class(), ADAPTER_TERMINAL_CLASS);
        let message = outcome.terminal_message().expect("the wall stated itself");
        assert!(message.contains("usage limit"), "{message}");
        assert!(message.contains("quota resets"), "{message}");
        // The refused turn zero-fills its usage object. The spend guard is
        // the same one `occupancy` and `finalMessage` carry, so the figures
        // still describe the work that happened rather than the refusal.
        let breakdown = outcome.breakdown().expect("pi reported its own figures");
        assert_eq!(breakdown.input_tokens, Some(190));
        assert_eq!(breakdown.output_tokens, Some(46));
    }

    /// The negative half, and the one that would have been red before this
    /// class existed for the opposite reason: a healthy stream must not
    /// acquire a terminal outcome, so the fallback classification stands.
    #[test]
    fn a_healthy_capture_is_not_adapter_terminal_and_keeps_the_machinery_class() {
        for (name, stream) in [("claude-code", CLAUDE_CODE_NORMAL), ("pi", PI_NORMAL)] {
            let outcome = classify(name, stream, Some("result-projection-timeout"));
            assert!(!outcome.is_adapter_terminal(), "{name}");
            assert_eq!(outcome.class(), "result-projection-timeout", "{name}");
            assert!(outcome.dispatches_retry(), "{name}");
        }
    }

    /// A lane with no machinery classification at all -- the shape an
    /// envelope-less exit actually has -- still names itself rather than
    /// falling through to nothing.
    #[test]
    fn an_adapter_terminal_outcome_dispatches_no_retry_without_any_fallback_code() {
        let outcome = classify("claude-code", CLAUDE_CODE_QUOTA, None);
        assert_eq!(outcome.class(), ADAPTER_TERMINAL_CLASS);
        assert!(!outcome.dispatches_retry());
        let envelope = outcome.envelope();
        assert_eq!(
            envelope.get("dispatchesRetry").and_then(Json::as_bool),
            Some(false)
        );
        assert_eq!(envelope.get("displacedClass"), Some(&Json::Null));
    }

    #[test]
    fn an_adapter_terminal_note_carries_the_reset_bearing_message() {
        let outcome = classify(
            "claude-code",
            CLAUDE_CODE_QUOTA,
            Some("result-projection-timeout"),
        );
        let note = outcome.note();
        assert!(note.contains("Adapter-terminal outcome"), "{note}");
        assert!(note.contains("reset at 9am"), "{note}");
        assert!(note.contains("result-projection-timeout"), "{note}");
    }

    /// The retained-capture path reads the same bytes from the archive the
    /// lane actually wrote to, which is where the evidence lives in
    /// production.
    #[test]
    fn an_adapter_terminal_outcome_reads_from_the_retained_capture_archive() {
        let root = std::env::temp_dir().join(format!(
            "tally-adapter-terminal-{}-{}",
            std::process::id(),
            "archive"
        ));
        fs::create_dir_all(&root).expect("fixture capture directory");
        let stdout = root.join("lane.out");
        fs::write(&stdout, CLAUDE_CODE_QUOTA).expect("fixture capture write");
        let outcome = classify_paths(
            "claude-code",
            &adapter("claude-code"),
            &stdout,
            None,
            Some("result-projection-timeout"),
        )
        .expect("the retained capture scrapes");
        fs::remove_dir_all(&root).ok();
        assert_eq!(outcome.class(), ADAPTER_TERMINAL_CLASS);
    }

    /// The spend ledger: a pi lane's envelope carries the harness's own
    /// figures, which before this declaration existed had to be
    /// reconstructed by hand from a session transcript.
    #[test]
    fn a_pi_lane_outcome_envelope_carries_scraped_token_spend() {
        let outcome = classify("pi", PI_NORMAL, None);
        let breakdown = outcome.breakdown().expect("pi reported its own figures");
        assert_eq!(breakdown.input_tokens, Some(190));
        assert_eq!(breakdown.output_tokens, Some(46));
        assert_eq!(breakdown.cache_read_tokens, Some(842));
        assert_eq!(breakdown.cache_write_tokens, Some(0));
        assert_eq!(breakdown.reasoning_tokens, Some(0));
        // 190 + 46 + 842 + 0 == 1078, and pi states the total itself, so the
        // envelope reports a harness figure that reconciles rather than a
        // sum tally computed and hoped agreed.
        let total = breakdown.total_tokens.expect("pi states its own total");
        assert_eq!(total.value, 1078);
        let tokens = outcome.envelope();
        let tokens = tokens.get("tokens").expect("the envelope carries tokens");
        assert_eq!(
            tokens.get("observation").and_then(Json::as_str),
            Some("reported")
        );
        assert_eq!(tokens.get("totalTokens").and_then(Json::as_u64), Some(1078));
        assert_eq!(tokens.get("outputTokens").and_then(Json::as_u64), Some(46));
    }

    /// The spend declaration is a separate capture from `occupancy` on
    /// purpose: the two read the same bytes and answer different questions,
    /// and neither concern's lookup may resolve against the other's names.
    #[test]
    fn a_pi_token_spend_capture_does_not_disturb_the_occupancy_reading() {
        let adapter = adapter("pi");
        let catalog = BTreeMap::from([("pi".to_owned(), adapter.clone())]);
        let captures = AdapterEngine::new(&catalog)
            .scrape_text("pi", PI_NORMAL, "")
            .expect("the committed capture scrapes");
        // input + cacheRead + cacheWrite of the last valid turn, unchanged by
        // the spend declaration standing beside it.
        let resident = tally_core::occupancy::context_tokens(&adapter, &captures);
        assert_eq!(resident, Some(1032));
    }

    #[test]
    fn a_claude_code_lane_token_spend_reaches_the_envelope_through_the_usage_mapping() {
        let outcome = classify("claude-code", CLAUDE_CODE_QUOTA, None);
        let breakdown = outcome
            .breakdown()
            .expect("claude-code reported its own figures");
        assert_eq!(breakdown.input_tokens, Some(1310));
        assert_eq!(breakdown.output_tokens, Some(41));
        assert_eq!(breakdown.cache_read_tokens, Some(20480));
        let note = outcome.note();
        assert!(note.contains("Token spend scraped"), "{note}");
        assert!(note.contains("output 41"), "{note}");
    }

    /// codex's refused turn emits no `turn.completed`, so it reports no
    /// usage at all. The envelope says exactly that instead of a zero.
    #[test]
    fn a_codex_token_spend_absence_is_reported_as_not_reported() {
        let outcome = classify("codex", CODEX_QUOTA, None);
        assert!(outcome.breakdown().is_none());
        let envelope = outcome.envelope();
        let tokens = envelope.get("tokens").expect("the envelope carries tokens");
        assert_eq!(
            tokens.get("observation").and_then(Json::as_str),
            Some("not-reported")
        );
        assert!(!outcome.note().contains("Token spend scraped"));
    }

    /// An adapter that declares no terminal capture cannot acquire the class,
    /// however its stream ends. The catalog is the only place the class can
    /// be granted.
    #[test]
    fn an_adapter_declaring_no_capture_is_never_adapter_terminal() {
        let stream = concat!(
            r#"{"type":"error","message":"nothing declares this"}"#,
            "\n"
        );
        let outcome = classify("shell", stream, Some("result-projection-timeout"));
        assert!(!outcome.is_adapter_terminal());
        assert_eq!(outcome.class(), "result-projection-timeout");
    }
}
