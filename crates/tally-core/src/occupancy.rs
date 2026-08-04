//! Context occupancy: how full a session's context window is, never what it
//! cost. Spend lives in [`crate::usage`]; this module answers "can this
//! session absorb another task" without deciding it — recording only, per
//! the SSSF doctrine ("context is occupancy, not spend") and this project's
//! own non-goal: no admission or scheduling behavior reads these fields yet.
//!
//! [`context_tokens`] is the last valid assistant turn's usage total — the
//! same figure [`crate::usage::observe`] already normalizes, read under its
//! occupancy meaning rather than its spend meaning. It needs no adapter
//! declaration of its own: whenever an attempt's usage is `Reported`, its
//! total is occupancy as of that turn's completion. A `None` here never
//! means zero; it means the sibling `usage` observation carried no total
//! either (no scrape declared, nothing reported, or a shape this project's
//! mapping could not turn into one).
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
//! nothing together mean `context_window` is `None`, exactly like `pi`'s
//! usage mapping staying undeclared until a real capture justifies one.

use serde::{Deserialize, Serialize};

use crate::adapters::{AdapterConfig, ScrapeResult};
use crate::usage::{self, UsageObservation};

/// Logical field name a capture declares to state its own context window.
pub const FIELD_CONTEXT_WINDOW: &str = "contextWindow";

/// Key read from an adapter's `extraConfig` for an operator-declared ceiling.
pub const CONFIG_CONTEXT_WINDOW: &str = "contextWindow";

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

/// Occupancy as of the attempt's last valid assistant turn. See the module
/// doc for why this needs no adapter declaration of its own.
#[must_use]
pub fn context_tokens(usage: &UsageObservation) -> Option<u64> {
    usage
        .breakdown()
        .and_then(|breakdown| breakdown.total_tokens.as_ref())
        .map(|total| total.value)
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
    use crate::usage::{UsageBreakdown, UsageShape, UsageTotalSource, UsageTotalTokens};

    // The same real, redacted captures `crate::usage`'s own tests scrape —
    // see `test/fixtures/usage/README.md` for provenance. Reusing them here
    // is deliberate: occupancy is read from the exact same normalized usage
    // this project already proved against real corpus, not a second fixture
    // that could quietly drift from it.
    const CODEX_STREAM: &str = include_str!("../../../test/fixtures/usage/codex.jsonl");
    const CODEX_QUIET_STREAM: &str =
        include_str!("../../../test/fixtures/usage/codex-no-usage.jsonl");
    const CLAUDE_STREAM: &str = include_str!("../../../test/fixtures/usage/claude-code.jsonl");
    const CLAUDE_QUIET_STREAM: &str =
        include_str!("../../../test/fixtures/usage/claude-code-no-usage.jsonl");

    const CODEX_USAGE_FIELDS: &str = r#"{"cacheReadTokens":["cached_input_tokens"],"cacheWriteTokens":["cache_write_input_tokens"],"inputTokensWithCacheRead":["input_tokens"],"outputTokens":["output_tokens"],"reasoningTokens":["reasoning_output_tokens"]}"#;
    const CLAUDE_USAGE_FIELDS: &str = r#"{"cacheReadTokens":["cache_read_input_tokens"],"cacheWriteTokens":["cache_creation_input_tokens"],"inputTokens":["input_tokens"],"outputTokens":["output_tokens"]}"#;

    fn capture(mode: ScrapeMode, pattern: &str, fields: &str) -> ScrapeCapture {
        ScrapeCapture {
            stream: ScrapeStream::Stdout,
            mode,
            pattern: pattern.to_owned(),
            fields: serde_json::from_str(fields).expect("declared fields parse"),
        }
    }

    /// Mirrors the real `codex` preset in `nix/lib/adapters.nix`: no
    /// `contextWindow` scrape, because no real codex capture has ever stated
    /// one.
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

    /// Mirrors the real `claude-code` preset: `usage` for spend, a sibling
    /// `contextWindow` capture for the harness's own stated ceiling, exactly
    /// as `usageCost` sits beside `usage` for cost.
    fn claude_adapter() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["claude".to_owned()],
            scrape: BTreeMap::from([
                (
                    "usage".to_owned(),
                    capture(ScrapeMode::JsonPath, "$..usage", CLAUDE_USAGE_FIELDS),
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
    fn occupancy_is_the_same_total_usage_already_normalized() {
        let captures = scraped(&codex_adapter(), "codex", CODEX_STREAM);
        let usage = crate::usage::observe(&codex_adapter(), &captures);
        assert_eq!(
            usage.breakdown().unwrap().total_tokens,
            Some(UsageTotalTokens {
                value: 7093008,
                source: UsageTotalSource::DerivedFromComponents,
            })
        );
        assert_eq!(context_tokens(&usage), Some(7093008));

        let captures = scraped(&claude_adapter(), "claude-code", CLAUDE_STREAM);
        let usage = crate::usage::observe(&claude_adapter(), &captures);
        assert_eq!(context_tokens(&usage), Some(11380648));
    }

    #[test]
    fn a_stream_with_no_usage_leaves_occupancy_unknown_not_zero() {
        let codex_captures = scraped(&codex_adapter(), "codex", CODEX_QUIET_STREAM);
        let codex_usage = crate::usage::observe(&codex_adapter(), &codex_captures);
        assert!(codex_usage.is_absent());
        assert_eq!(context_tokens(&codex_usage), None);

        let claude_captures = scraped(&claude_adapter(), "claude-code", CLAUDE_QUIET_STREAM);
        let claude_usage = crate::usage::observe(&claude_adapter(), &claude_captures);
        assert!(claude_usage.is_absent());
        assert_eq!(context_tokens(&claude_usage), None);
    }

    #[test]
    fn a_declared_but_unmapped_usage_object_has_no_total_and_no_occupancy() {
        let usage = UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Unmapped,
            input_tokens: None,
            input_tokens_as_reported: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            output_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost: None,
            unreadable_fields: Vec::new(),
        });
        assert_eq!(context_tokens(&usage), None);
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
