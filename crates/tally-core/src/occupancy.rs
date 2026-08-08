//! Context occupancy: how full a session's context window is, never what it
//! cost. Spend lives in [`crate::usage`]; this module answers "can this
//! session absorb another task" without deciding it — recording only, per
//! the SSSF doctrine ("context is occupancy, not spend") and this project's
//! own non-goal: no admission or scheduling behavior reads these fields yet.
//!
//! [`context_tokens`] is the tokens resident in the context window as of the
//! attempt's **last valid assistant turn** — input plus both cache halves,
//! deliberately excluding that turn's own output tokens, which have not yet
//! been folded back into history at the moment this is measured. This is
//! **not** the same number [`crate::usage::observe`] normalizes: that
//! module's `usage` capture keeps the *last* `usage` object anywhere in the
//! stream, which for both claude-code (the `result` event) and codex (the
//! final `turn.completed`) is a session-lifetime roll-up, not a turn — a
//! quantity that grows without bound across a long session and reads as a
//! 1000%+ "occupancy" against a fixed window. Occupancy reads a
//! **different, narrower capture**: the last event that is actually one
//! assistant turn, declared and resolved through the exact same
//! [`crate::usage::resolve`] mapping mechanism `usage::observe` uses, under
//! **logical field names of its own** (`residentInputTokens` and friends)
//! so the two concerns can never collide inside `resolve`'s
//! searches-every-declared-capture semantics — a capture that declared the
//! same names as `usage`'s `inputTokens`/`cacheReadTokens`/`cacheWriteTokens`
//! would let occupancy's narrower value silently answer a spend query, or
//! vice versa, depending on which capture name sorts first.
//!
//! `codex exec --json` does not state occupancy at all: it emits exactly one
//! `turn.completed` per exec, carrying only the cumulative
//! `total_token_usage` shape, never a per-turn resident figure (confirmed
//! against codex's own separate rollout journal, which does carry a true
//! `last_token_usage` beside the cumulative `total_token_usage` — a shape
//! `codex exec --json` does not expose). The `codex` preset therefore
//! declares no occupancy capture: reusing the cumulative total under
//! occupancy's name is the mistake this module now exists to not repeat.
//!
//! `pi` is the mirror image and is worth stating exactly, because the two
//! adapters decline opposite mappings for opposite reasons.
//! `test/fixtures/traces/pi.jsonl` is a real `pi --mode json` capture, so
//! pi's key names are not in doubt — the reason its preset declares **no
//! usage mapping** is not absence of evidence but what the evidence says.
//! pi states usage per assistant message and never per attempt: there is no
//! `turn.completed`-style roll-up anywhere in its stream, so a declared
//! `inputTokens` there would report one turn's figures as the attempt's
//! spend. The per-turn reading the capture does support is occupancy, and
//! that is what the preset declares. "We have no data" and "we have data
//! and it does not support this mapping" are different claims, and pi's is
//! the second.
//!
//! [`context_window`] is the ceiling that total is measured against, and it
//! has two independent, distinguishable provenances:
//!
//! * a harness that states its own window inside the captured stream,
//!   declared the same way a usage field is — a capture's `fields` map (see
//!   the `claude-code` preset in `nix/lib/adapters.nix`, which declares
//!   `contextWindow` beside `usage` and `usageCost`) — resolved through
//!   [`crate::usage::resolve`], not a parallel mechanism.
//! * an operator-declared ceiling in the adapter's `extraConfig.contextWindow`.
//!
//! A stream-stated window wins when both are present, because it is what the
//! harness actually applied for this attempt's model; the config ceiling is
//! what the operator believes true absent better information. Neither is
//! fabricated: a harness that states nothing and a config that declares
//! nothing together mean `context_window` is `None` — the same refusal to
//! invent a number that keeps `pi`'s usage mapping undeclared, there
//! because the capture on hand states a per-message figure and no
//! per-attempt one, here because nobody has stated a window at all.

use serde::{Deserialize, Serialize};

use crate::adapters::{AdapterConfig, ScrapeResult};
use crate::usage;

/// Logical field name a capture declares to state its own context window.
pub const FIELD_CONTEXT_WINDOW: &str = "contextWindow";

/// Key read from an adapter's `extraConfig` for an operator-declared ceiling.
pub const CONFIG_CONTEXT_WINDOW: &str = "contextWindow";

/// Logical field names for the tokens resident in the context window as of
/// the last valid assistant turn. Deliberately spelled differently from
/// `crate::usage`'s `inputTokens`/`cacheReadTokens`/`cacheWriteTokens`: see
/// the module doc for why a shared name would be a silent cross-concern
/// collision inside `usage::resolve`, not a convenience.
pub const FIELD_RESIDENT_INPUT_TOKENS: &str = "residentInputTokens";
pub const FIELD_RESIDENT_CACHE_READ_TOKENS: &str = "residentCacheReadTokens";
pub const FIELD_RESIDENT_CACHE_WRITE_TOKENS: &str = "residentCacheWriteTokens";

/// Which provenance answered for a [`ContextWindow`]. Kept apart so a
/// consumer never has to guess whether a ceiling is what the provider said
/// about itself or what the operator asserted about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContextWindowSource {
    /// The harness stated its own context window inside the captured
    /// stream.
    ProviderCapture,
    /// The operator declared a ceiling in the adapter's configuration.
    AdapterConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContextWindow {
    pub tokens: u64,
    pub source: ContextWindowSource,
}

/// Tokens resident in the context window as of the attempt's last valid
/// assistant turn: input plus both cache halves, excluding that turn's own
/// output. `None` whenever no capture declared any of the three resident
/// fields — an adapter with no occupancy scrape (`codex`), or a stream
/// that never reached a valid assistant turn — never a fabricated zero,
/// and never `crate::usage`'s cumulative session total under a borrowed
/// name. A component the capture did carry but this project's mapping could
/// not read from is treated the same as one the harness omitted, mirroring
/// `UsageBreakdown::component_sum`'s established precedent: an all-absent
/// read is `None`, a partially-read one sums what did resolve.
#[must_use]
pub fn context_tokens(adapter: &AdapterConfig, captures: &ScrapeResult) -> Option<u64> {
    let input = resident_count(adapter, captures, FIELD_RESIDENT_INPUT_TOKENS);
    let cache_read = resident_count(adapter, captures, FIELD_RESIDENT_CACHE_READ_TOKENS);
    let cache_write = resident_count(adapter, captures, FIELD_RESIDENT_CACHE_WRITE_TOKENS);
    if input.is_none() && cache_read.is_none() && cache_write.is_none() {
        return None;
    }
    [input, cache_read, cache_write]
        .into_iter()
        .try_fold(0_u64, |sum, value| sum.checked_add(value.unwrap_or(0)))
}

fn resident_count(adapter: &AdapterConfig, captures: &ScrapeResult, logical: &str) -> Option<u64> {
    usage::resolve(adapter, captures, logical).and_then(usage::as_count)
}

/// The context window ceiling for this attempt, from whichever provenance
/// answered. `captures` is `None` for an adapter with no scrape captured at
/// all for this attempt — the config ceiling still needs checking, because
/// it depends on nothing scraped.
#[must_use]
pub fn context_window(
    adapter: &AdapterConfig,
    captures: Option<&ScrapeResult>,
) -> Option<ContextWindow> {
    if let Some(captures) = captures {
        if let Some(tokens) =
            usage::resolve(adapter, captures, FIELD_CONTEXT_WINDOW).and_then(usage::as_count)
        {
            return Some(ContextWindow {
                tokens,
                source: ContextWindowSource::ProviderCapture,
            });
        }
    }
    configured_context_window(adapter).map(|tokens| ContextWindow {
        tokens,
        source: ContextWindowSource::AdapterConfig,
    })
}

fn configured_context_window(adapter: &AdapterConfig) -> Option<u64> {
    adapter
        .extra_config
        .get(CONFIG_CONTEXT_WINDOW)
        .and_then(usage::as_count)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::adapters::{AdapterEngine, ScrapeCapture, ScrapeMode, ScrapeStream};

    // The same real, redacted captures `crate::usage`'s own tests scrape —
    // see `test/fixtures/usage/README.md` for provenance. Reusing them here
    // is deliberate: occupancy is read from the same real corpus this
    // project already proved usage against, not a second fixture that could
    // quietly drift from it.
    const CODEX_STREAM: &str = include_str!("../../../test/fixtures/usage/codex.jsonl");
    const CODEX_QUIET_STREAM: &str =
        include_str!("../../../test/fixtures/usage/codex-no-usage.jsonl");
    const CLAUDE_STREAM: &str = include_str!("../../../test/fixtures/usage/claude-code.jsonl");
    const CLAUDE_QUIET_STREAM: &str =
        include_str!("../../../test/fixtures/usage/claude-code-no-usage.jsonl");

    const CODEX_USAGE_FIELDS: &str = r#"{"cacheReadTokens":["cached_input_tokens"],"cacheWriteTokens":["cache_write_input_tokens"],"inputTokensWithCacheRead":["input_tokens"],"outputTokens":["output_tokens"],"reasoningTokens":["reasoning_output_tokens"]}"#;
    const CLAUDE_USAGE_FIELDS: &str = r#"{"cacheReadTokens":["cache_read_input_tokens"],"cacheWriteTokens":["cache_creation_input_tokens"],"inputTokens":["input_tokens"],"outputTokens":["output_tokens"]}"#;
    const CLAUDE_OCCUPANCY_FIELDS: &str = r#"{"residentInputTokens":["input_tokens"],"residentCacheReadTokens":["cache_read_input_tokens"],"residentCacheWriteTokens":["cache_creation_input_tokens"]}"#;

    fn capture(mode: ScrapeMode, pattern: &str, fields: &str) -> ScrapeCapture {
        ScrapeCapture {
            stream: ScrapeStream::Stdout,
            mode,
            pattern: pattern.to_owned(),
            counter_scope: None,
            fields: serde_json::from_str(fields).expect("declared fields parse"),
        }
    }

    /// Mirrors the real `codex` preset in `nix/lib/adapters.nix`: no
    /// `occupancy` or `contextWindow` scrape, because no real codex capture
    /// has ever stated a per-turn resident figure or a window — `codex exec
    /// --json` states only cumulative totals.
    fn codex_adapter() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["codex".to_owned()],
            scrape: BTreeMap::from([(
                "usage".to_owned(),
                capture(ScrapeMode::JsonPath, "$..usage", CODEX_USAGE_FIELDS),
            )]),
            ..Default::default()
        }
    }

    /// Mirrors the real `claude-code` preset: `usage` for spend (last
    /// `usage` object anywhere, a session-lifetime roll-up), a sibling
    /// `occupancy` capture scoped to only assistant-turn events for
    /// residency, and `contextWindow` for the harness's own stated ceiling.
    fn claude_adapter() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["claude".to_owned()],
            scrape: BTreeMap::from([
                (
                    "usage".to_owned(),
                    capture(ScrapeMode::JsonPath, "$..usage", CLAUDE_USAGE_FIELDS),
                ),
                (
                    "occupancy".to_owned(),
                    capture(
                        ScrapeMode::JsonPathLast,
                        "$[?@.type == 'assistant'].message.usage",
                        CLAUDE_OCCUPANCY_FIELDS,
                    ),
                ),
                (
                    "contextWindow".to_owned(),
                    capture(
                        ScrapeMode::JsonPathLast,
                        "$[?@.type == 'result'].modelUsage.*.contextWindow",
                        r#"{"contextWindow":["$"]}"#,
                    ),
                ),
            ]),
            ..Default::default()
        }
    }

    fn scraped(adapter: &AdapterConfig, name: &str, stream: &str) -> ScrapeResult {
        let adapters = BTreeMap::from([(name.to_owned(), adapter.clone())]);
        AdapterEngine::new(&adapters)
            .scrape_text(name, stream, "")
            .expect("fixture stream scrapes")
    }

    #[test]
    fn occupancy_is_the_last_assistant_turns_residency_not_the_session_total() {
        let captures = scraped(&claude_adapter(), "claude-code", CLAUDE_STREAM);
        // Independently verified against the fixture: the last of three
        // `type == "assistant"` events carries
        // input_tokens=2, cache_read_input_tokens=227005,
        // cache_creation_input_tokens=309 -- residency 227316.
        assert_eq!(context_tokens(&claude_adapter(), &captures), Some(227_316));

        // The number this repairs: #381's own normalized total for the same
        // stream is the `result` event's session-lifetime roll-up, 50x
        // larger than the real resident figure and larger than the window
        // it would have been measured against.
        let usage = crate::usage::observe(&claude_adapter(), &captures);
        assert_eq!(
            usage.breakdown().unwrap().total_tokens.unwrap().value,
            11_380_648,
            "the cumulative total usage.rs reports under its own name, for contrast"
        );
        assert_ne!(
            context_tokens(&claude_adapter(), &captures),
            usage
                .breakdown()
                .unwrap()
                .total_tokens
                .map(|total| total.value),
            "occupancy must never collapse to the cumulative spend total"
        );
    }

    #[test]
    fn occupancy_never_exceeds_the_real_window_it_is_measured_against() {
        let captures = scraped(&claude_adapter(), "claude-code", CLAUDE_STREAM);
        let tokens = context_tokens(&claude_adapter(), &captures).expect("occupancy is reported");
        let window =
            context_window(&claude_adapter(), Some(&captures)).expect("window is reported");
        assert!(
            tokens <= window.tokens,
            "occupancy {tokens} exceeded its own window {}",
            window.tokens
        );
        // Loosely bounded sanity: real per-turn residency is a fraction of
        // the window, not multiples of it -- this is exactly the assertion
        // a cumulative-total regression would fail.
        assert!(tokens * 4 < window.tokens);
    }

    #[test]
    fn codex_declares_no_occupancy_capture_and_renders_none() {
        // codex exec --json emits exactly one turn.completed per exec,
        // carrying only the cumulative total_token_usage shape -- it does
        // not state a per-turn resident figure, so the real preset declares
        // no occupancy capture and this must read None, not the cumulative
        // total.
        let captures = scraped(&codex_adapter(), "codex", CODEX_STREAM);
        assert_eq!(context_tokens(&codex_adapter(), &captures), None);
    }

    #[test]
    fn a_stream_with_no_assistant_turn_leaves_occupancy_unknown_not_zero() {
        let codex_captures = scraped(&codex_adapter(), "codex", CODEX_QUIET_STREAM);
        assert_eq!(context_tokens(&codex_adapter(), &codex_captures), None);

        let claude_captures = scraped(&claude_adapter(), "claude-code", CLAUDE_QUIET_STREAM);
        assert_eq!(context_tokens(&claude_adapter(), &claude_captures), None);
    }

    #[test]
    fn a_capture_that_reused_usages_field_names_would_not_be_picked_up_by_spend() {
        // Regression guard for the collision `usage::resolve` searches every
        // declared capture for a matching logical name: if `occupancy` ever
        // grows a `fields` map spelled `inputTokens`/`cacheReadTokens`/
        // `cacheWriteTokens` again, this proves whether it silently answers
        // a spend query instead of `usage`'s own capture.
        let adapter = claude_adapter();
        let captures = scraped(&adapter, "claude-code", CLAUDE_STREAM);
        let usage = crate::usage::observe(&adapter, &captures);
        let breakdown = usage.breakdown().unwrap();
        // The three real assistant-turn input_tokens values are all 2; the
        // `usage` capture's own reading is the `result` event's 83. If
        // `occupancy`'s capture were ever consulted for `usage::FIELD_INPUT_TOKENS`
        // this would read 2 instead.
        assert_eq!(breakdown.input_tokens, Some(83));
    }

    #[test]
    fn the_real_claude_capture_states_its_own_context_window() {
        let captures = scraped(&claude_adapter(), "claude-code", CLAUDE_STREAM);
        let window = context_window(&claude_adapter(), Some(&captures));
        assert_eq!(
            window,
            Some(ContextWindow {
                tokens: 1_000_000,
                source: ContextWindowSource::ProviderCapture,
            })
        );
    }

    #[test]
    fn codex_declares_no_context_window_scrape_and_none_is_configured() {
        // No real codex capture in this project's corpus has ever stated a
        // context window, so the real preset declares nothing, and this is
        // the honest answer rather than a guessed one.
        let captures = scraped(&codex_adapter(), "codex", CODEX_STREAM);
        assert_eq!(context_window(&codex_adapter(), Some(&captures)), None);
    }

    #[test]
    fn a_config_declared_ceiling_is_distinguishable_from_a_scraped_one() {
        let mut configured = codex_adapter();
        configured
            .extra_config
            .insert(CONFIG_CONTEXT_WINDOW.to_owned(), json!(400_000));
        let captures = scraped(&configured, "codex", CODEX_STREAM);
        assert_eq!(
            context_window(&configured, Some(&captures)),
            Some(ContextWindow {
                tokens: 400_000,
                source: ContextWindowSource::AdapterConfig,
            })
        );

        // An adapter with no scrape captured at all for this attempt still
        // reads its configured ceiling: the config path depends on nothing
        // scraped.
        assert_eq!(
            context_window(&configured, None),
            Some(ContextWindow {
                tokens: 400_000,
                source: ContextWindowSource::AdapterConfig,
            })
        );
    }

    #[test]
    fn a_scraped_window_wins_over_a_configured_one() {
        let mut adapter = claude_adapter();
        adapter
            .extra_config
            .insert(CONFIG_CONTEXT_WINDOW.to_owned(), json!(1));
        let captures = scraped(&adapter, "claude-code", CLAUDE_STREAM);
        assert_eq!(
            context_window(&adapter, Some(&captures)),
            Some(ContextWindow {
                tokens: 1_000_000,
                source: ContextWindowSource::ProviderCapture,
            })
        );
    }

    #[test]
    fn no_scrape_and_no_config_is_none_never_a_fabricated_zero() {
        let adapter = AdapterConfig {
            argv: vec!["agent".to_owned()],
            ..Default::default()
        };
        assert_eq!(context_window(&adapter, None), None);
        let empty = ScrapeResult::default();
        assert_eq!(context_window(&adapter, Some(&empty)), None);
        assert_eq!(context_tokens(&adapter, &empty), None);
    }

    #[test]
    fn an_unreadable_configured_ceiling_is_absent_not_a_crash() {
        let mut adapter = codex_adapter();
        adapter
            .extra_config
            .insert(CONFIG_CONTEXT_WINDOW.to_owned(), json!("not-a-number"));
        assert_eq!(context_window(&adapter, None), None);
    }
}
