//! Normalized per-attempt harness usage.
//!
//! Harnesses report token usage in incompatible shapes: codex nests
//! `cached_input_tokens` inside an `input_tokens` figure that already contains
//! it, claude-code reports `cache_creation_input_tokens` and
//! `cache_read_input_tokens` beside an `input_tokens` figure that excludes
//! both. Tally does not learn those shapes in Rust. An adapter declares, per
//! capture, a `fields` map from a logical field name to the ordered candidate
//! paths that carry it inside that capture's scraped value; this module reads
//! the mapping and produces one normalized record. Adding a harness is a
//! declaration in `nix/lib/adapters.nix`, never a match arm here.
//!
//! Three states are kept distinct, because collapsing them produces a number
//! that reads as a measurement and is not one:
//!
//! * [`UsageObservation::NotDeclared`] — the adapter declared no usage scrape.
//! * [`UsageObservation::NotReported`] — a usage scrape was declared, the
//!   stream was read, and it carried no usage.
//! * [`UsageObservation::Reported`] — the harness reported usage. A reported
//!   zero is a measurement and lives here, never in the other two.
//!
//! Durability differs by state, and a consumer that reads across a daemon
//! restart must know which. `Reported` has a durable seat: it is written into
//! the advisory attestation ledger beside the raw captures, keyed by task,
//! attempt, and lease epoch. The two absences are recorded on the live row
//! only — no attestation is written for an adapter that scrapes nothing, and
//! recovery skips such adapters — so after a restart they read back as a
//! missing field rather than as a stated absence. Both are recomputable from
//! the adapter configuration, which is why this is a loss of statement rather
//! than of fact, but a rollup counting coverage should treat a missing record
//! and a `not-declared` record as the same answer.

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

use crate::adapters::{AdapterConfig, ScrapeCapture, ScrapeResult};

/// Capture name the adapter contract declares for harness-reported usage.
/// Its presence is what separates "the adapter never configured a usage
/// scrape" from "the harness reported nothing".
pub const USAGE_CAPTURE: &str = "usage";

/// Input tokens excluding cache reads and cache writes (the claude-code
/// convention).
pub const FIELD_INPUT_TOKENS: &str = "inputTokens";
/// Input tokens that already include the cache-read tokens reported beside
/// them (the codex convention). Declaring this spelling instead of
/// [`FIELD_INPUT_TOKENS`] is how an adapter states its convention without
/// tally matching on the adapter's name.
pub const FIELD_INPUT_TOKENS_WITH_CACHE_READ: &str = "inputTokensWithCacheRead";
/// Tokens served from the provider's prompt cache.
pub const FIELD_CACHE_READ_TOKENS: &str = "cacheReadTokens";
/// Tokens written into the provider's prompt cache.
pub const FIELD_CACHE_WRITE_TOKENS: &str = "cacheWriteTokens";
/// Output tokens, including any reasoning tokens.
pub const FIELD_OUTPUT_TOKENS: &str = "outputTokens";
/// Reasoning tokens, nested within the output-token figure.
pub const FIELD_REASONING_TOKENS: &str = "reasoningTokens";
/// A total the harness reported itself.
pub const FIELD_TOTAL_TOKENS: &str = "totalTokens";
/// Cost in US dollars as the harness reported it. Tally has no pricing table
/// and never computes this.
pub const FIELD_COST_USD: &str = "costUsd";

/// Every logical usage field this module reads. A capture declaring any of
/// them declares a usage scrape.
pub const USAGE_FIELDS: [&str; 8] = [
    FIELD_INPUT_TOKENS,
    FIELD_INPUT_TOKENS_WITH_CACHE_READ,
    FIELD_CACHE_READ_TOKENS,
    FIELD_CACHE_WRITE_TOKENS,
    FIELD_OUTPUT_TOKENS,
    FIELD_REASONING_TOKENS,
    FIELD_TOTAL_TOKENS,
    FIELD_COST_USD,
];

/// The mapping applied to a declared `usage` capture that carries no `fields`
/// of its own. It is deliberately the exact key set the pool-meter feeder has
/// always read — `total_tokens`, else `input_tokens` plus `output_tokens` —
/// so an adapter that predates the mapping keeps feeding meters the same
/// number. Anything richer is a declaration, not a guess.
fn default_paths(logical: &str) -> &'static [&'static str] {
    match logical {
        FIELD_INPUT_TOKENS => &["input_tokens"],
        FIELD_OUTPUT_TOKENS => &["output_tokens"],
        FIELD_TOTAL_TOKENS => &["total_tokens"],
        _ => &[],
    }
}

/// Whether the record's component figures were read, only a lump was, or the
/// harness reported something no declared mapping could read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageShape {
    /// At least one per-component token figure was read.
    Components,
    /// No components, but a total and/or a cost was.
    Lump,
    /// The harness reported a usage value and no declared field could be read
    /// from it.
    Unmapped,
}

/// Where a total came from. A derived total is never presented as one the
/// harness stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageTotalSource {
    HarnessReported,
    DerivedFromComponents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageTotalTokens {
    pub value: u64,
    pub source: UsageTotalSource,
}

/// Cost exactly as the harness reported it. The amount is retained as the JSON
/// number that arrived, so nothing is re-rounded on the way to a query
/// surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageCost {
    pub amount: Number,
    pub currency: String,
}

impl UsageCost {
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.amount.as_f64()
    }
}

/// The normalized breakdown for one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageBreakdown {
    pub shape: UsageShape,
    /// Input tokens excluding both cache reads and cache writes.
    ///
    /// This field alone is **not** the cross-harness "fresh input" figure, and
    /// a rollup that sums it alone understates any harness with a cache-write
    /// category. claude-code's `cache_creation_input_tokens` are fresh,
    /// uncached prompt tokens that its `input_tokens` excludes; codex has no
    /// cache-write category at all, so for codex this field already is the
    /// whole fresh input. The comparable figure is
    /// `input_tokens + cache_write_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// The harness's own input-token figure under the harness's own
    /// convention. Retained because it is what the harness billed and what the
    /// built-in pool meter has always read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens_as_reported: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Output tokens, reasoning included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Reasoning tokens, nested within `output_tokens` rather than added to
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<UsageTotalTokens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<UsageCost>,
    /// Declared fields whose path resolved to a value that was not a usable
    /// count or amount. A mapping that has drifted from its harness says so
    /// here instead of silently reading as an absence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable_fields: Vec<String>,
}

/// Outcome of checking the record's components against its total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageReconciliation {
    /// Components are present and the total equals their sum.
    Reconciled { total: u64 },
    /// The harness stated a total its own components do not sum to. Both
    /// numbers are kept; neither is corrected.
    Mismatch { reported: u64, computed: u64 },
    /// No component figures to reconcile.
    NoComponents,
}

/// One attempt's usage, in exactly one of three states.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "breakdown", rename_all = "kebab-case")]
pub enum UsageObservation {
    /// The adapter declared no usage scrape. Nothing was ever looked for.
    NotDeclared,
    /// A usage scrape was declared and the stream carried no usage.
    NotReported,
    /// The harness reported usage, including when it reported zero.
    Reported(UsageBreakdown),
}

impl UsageObservation {
    #[must_use]
    pub const fn breakdown(&self) -> Option<&UsageBreakdown> {
        match self {
            Self::Reported(breakdown) => Some(breakdown),
            Self::NotDeclared | Self::NotReported => None,
        }
    }

    /// Whether this observation is a typed absence rather than a measurement.
    /// A rollup reporting coverage counts these apart from reported usage.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::NotDeclared | Self::NotReported)
    }

    /// The token amount the built-in pool-meter feeder charges to a windowed
    /// consumption pool.
    ///
    /// This reproduces the pre-normalization reader on every shape a harness
    /// actually emits: a total the harness stated, else the harness's own
    /// input figure plus its output figure, and never a zero — a zero-token
    /// attempt writes no meter observation, as it never did. A figure the
    /// harness emitted in a shape that is not a count (a stringified total, a
    /// negative input) charges nothing at all rather than charging the part
    /// that did parse, which is also what it did before.
    ///
    /// Two shapes no harness emits do diverge, and both diverge upward — the
    /// old reader wrote no meter event and this one charges a number: a key
    /// present with a JSON `null` value, which the old reader saw as present
    /// and then failed to parse, and a whole-valued float, which
    /// `Value::as_u64` rejected. For a windowed-consumption pool, charging is
    /// the conservative direction; the old behaviour left a stale, lower
    /// utilization in place. Pinned by
    /// `the_meter_diverges_from_the_pre_normalization_reader_only_upward`.
    #[must_use]
    pub fn meter_amount(&self) -> Option<u64> {
        let breakdown = self.breakdown()?;
        if breakdown.unreadable_fields.iter().any(|field| {
            matches!(
                field.as_str(),
                FIELD_TOTAL_TOKENS
                    | FIELD_INPUT_TOKENS
                    | FIELD_INPUT_TOKENS_WITH_CACHE_READ
                    | FIELD_OUTPUT_TOKENS
            )
        }) {
            return None;
        }
        let amount = match breakdown.total_tokens {
            Some(total) if total.source == UsageTotalSource::HarnessReported => total.value,
            _ => breakdown
                .input_tokens_as_reported
                .unwrap_or(0)
                .checked_add(breakdown.output_tokens.unwrap_or(0))?,
        };
        (amount > 0).then_some(amount)
    }
}

impl UsageBreakdown {
    /// Sum of the component figures the harness reported. Components the
    /// harness did not report contribute nothing; the caller learns how
    /// complete the sum is from the individual fields.
    #[must_use]
    pub fn component_sum(&self) -> Option<u64> {
        let components = [
            self.input_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
            self.output_tokens,
        ];
        if components.iter().all(Option::is_none) {
            return None;
        }
        components
            .into_iter()
            .try_fold(0_u64, |sum, value| sum.checked_add(value.unwrap_or(0)))
    }

    #[must_use]
    pub fn reconciliation(&self) -> UsageReconciliation {
        let Some(computed) = self.component_sum() else {
            return UsageReconciliation::NoComponents;
        };
        match self.total_tokens {
            Some(total) if total.value != computed => UsageReconciliation::Mismatch {
                reported: total.value,
                computed,
            },
            _ => UsageReconciliation::Reconciled { total: computed },
        }
    }
}

/// Whether this adapter declares a usage scrape at all.
#[must_use]
pub fn declares_usage(adapter: &AdapterConfig) -> bool {
    adapter.scrape.iter().any(|(name, capture)| {
        name == USAGE_CAPTURE
            || USAGE_FIELDS
                .iter()
                .any(|field| capture.fields.contains_key(*field))
    })
}

/// Normalize one attempt's scrape into a usage observation.
#[must_use]
pub fn observe(adapter: &AdapterConfig, captures: &ScrapeResult) -> UsageObservation {
    if !declares_usage(adapter) {
        return UsageObservation::NotDeclared;
    }
    let mut unreadable = Vec::new();
    let mut count = |logical: &str| -> Option<u64> {
        match resolve(adapter, captures, logical) {
            None => None,
            Some(value) => match as_count(value) {
                Some(count) => Some(count),
                None => {
                    unreadable.push(logical.to_owned());
                    None
                }
            },
        }
    };
    let input_exclusive = count(FIELD_INPUT_TOKENS);
    let input_inclusive = count(FIELD_INPUT_TOKENS_WITH_CACHE_READ);
    let cache_read = count(FIELD_CACHE_READ_TOKENS);
    let cache_write = count(FIELD_CACHE_WRITE_TOKENS);
    let output = count(FIELD_OUTPUT_TOKENS);
    let reasoning = count(FIELD_REASONING_TOKENS);
    let reported_total = count(FIELD_TOTAL_TOKENS);
    let cost = match resolve(adapter, captures, FIELD_COST_USD) {
        None => None,
        Some(value) => match as_amount(value) {
            Some(amount) => Some(UsageCost {
                amount,
                currency: "USD".to_owned(),
            }),
            None => {
                unreadable.push(FIELD_COST_USD.to_owned());
                None
            }
        },
    };

    let input_as_reported = input_exclusive.or(input_inclusive);
    // The inclusive spelling states that cache reads are already inside the
    // input figure. Subtracting them is the only way to get a number that
    // means the same thing as the exclusive spelling. A stream where the
    // subtraction underflows is inconsistent with itself, so the canonical
    // field stays unknown rather than becoming a plausible zero.
    let input_tokens = match (input_exclusive, input_inclusive) {
        (Some(value), _) => Some(value),
        (None, Some(value)) => value.checked_sub(cache_read.unwrap_or(0)),
        (None, None) => None,
    };

    let has_components = input_as_reported.is_some()
        || cache_read.is_some()
        || cache_write.is_some()
        || output.is_some()
        || reasoning.is_some();
    let has_lump = reported_total.is_some() || cost.is_some();
    if !has_components && !has_lump && unreadable.is_empty() {
        return if captures.captures.contains_key(USAGE_CAPTURE) {
            UsageObservation::Reported(UsageBreakdown {
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
            })
        } else {
            UsageObservation::NotReported
        };
    }

    let shape = if has_components {
        UsageShape::Components
    } else if has_lump {
        UsageShape::Lump
    } else {
        UsageShape::Unmapped
    };

    let mut breakdown = UsageBreakdown {
        shape,
        input_tokens,
        input_tokens_as_reported: input_as_reported,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        output_tokens: output,
        reasoning_tokens: reasoning,
        total_tokens: reported_total.map(|value| UsageTotalTokens {
            value,
            source: UsageTotalSource::HarnessReported,
        }),
        cost,
        unreadable_fields: unreadable,
    };
    if breakdown.total_tokens.is_none() {
        breakdown.total_tokens = breakdown.component_sum().map(|value| UsageTotalTokens {
            value,
            source: UsageTotalSource::DerivedFromComponents,
        });
    }
    UsageObservation::Reported(breakdown)
}

fn field_paths<'a>(capture_name: &str, capture: &'a ScrapeCapture, logical: &str) -> Vec<&'a str> {
    if capture.fields.is_empty() {
        if capture_name == USAGE_CAPTURE {
            return default_paths(logical).to_vec();
        }
        return Vec::new();
    }
    capture
        .fields
        .get(logical)
        .map_or_else(Vec::new, |paths| paths.iter().map(String::as_str).collect())
}

fn resolve<'a>(
    adapter: &AdapterConfig,
    captures: &'a ScrapeResult,
    logical: &str,
) -> Option<&'a Value> {
    for (name, capture) in &adapter.scrape {
        let Some(root) = captures.captures.get(name) else {
            continue;
        };
        for path in field_paths(name, capture, logical) {
            if let Some(value) = resolve_path(root, path) {
                if !value.is_null() {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Resolve one declared path inside a captured value. `$` (or the empty
/// string) is the captured value itself; otherwise the path is dot-separated
/// object keys, with numeric segments indexing arrays.
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

/// A token count is a non-negative integer. A float that happens to be a whole
/// number is accepted because JSON writers do emit them; anything else is
/// unreadable rather than silently zero.
fn as_count(value: &Value) -> Option<u64> {
    if let Some(count) = value.as_u64() {
        return Some(count);
    }
    let float = value.as_f64()?;
    if !float.is_finite() || float < 0.0 || float.fract() != 0.0 || float > u64::MAX as f64 {
        return None;
    }
    Some(float as u64)
}

fn as_amount(value: &Value) -> Option<Number> {
    let number = value.as_number()?;
    let amount = number.as_f64()?;
    (amount.is_finite() && amount >= 0.0).then(|| number.clone())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::adapters::{AdapterEngine, ScrapeMode, ScrapeStream};

    // Every fixture below is an order-preserving excerpt of a real capture from
    // this project's own dispatch corpus, redacted for a public repository but
    // with every `usage` object and `total_cost_usd` copied verbatim. See
    // `test/fixtures/usage/README.md` for provenance.
    const CODEX_STREAM: &str = include_str!("../../../test/fixtures/usage/codex.jsonl");
    const CODEX_QUIET_STREAM: &str =
        include_str!("../../../test/fixtures/usage/codex-no-usage.jsonl");
    const CLAUDE_STREAM: &str = include_str!("../../../test/fixtures/usage/claude-code.jsonl");
    const CLAUDE_QUIET_STREAM: &str =
        include_str!("../../../test/fixtures/usage/claude-code-no-usage.jsonl");
    const N_MINUS_1: &str = include_str!("../../../test/fixtures/usage/n-minus-1-records.json");

    /// The `fields` maps these tests exercise are the ones the presets in
    /// `nix/lib/adapters.nix` declare. The evaluated-configuration check in
    /// `flake.nix` pins the same JSON, so a preset that drifts from this
    /// mirror fails the gate rather than passing two agreeing-but-wrong tests.
    const CODEX_USAGE_FIELDS: &str = r#"{"cacheReadTokens":["cached_input_tokens"],"cacheWriteTokens":["cache_write_input_tokens"],"inputTokensWithCacheRead":["input_tokens"],"outputTokens":["output_tokens"],"reasoningTokens":["reasoning_output_tokens"]}"#;
    const CLAUDE_USAGE_FIELDS: &str = r#"{"cacheReadTokens":["cache_read_input_tokens"],"cacheWriteTokens":["cache_creation_input_tokens"],"inputTokens":["input_tokens"],"outputTokens":["output_tokens"]}"#;
    const CLAUDE_COST_FIELDS: &str = r#"{"costUsd":["$"]}"#;

    fn capture(mode: ScrapeMode, pattern: &str, fields: &str) -> ScrapeCapture {
        ScrapeCapture {
            stream: ScrapeStream::Stdout,
            mode,
            pattern: pattern.to_owned(),
            fields: serde_json::from_str(fields).expect("declared fields parse"),
        }
    }

    fn codex_adapter() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["codex".to_owned()],
            scrape: BTreeMap::from([
                (
                    "sessionRef".to_owned(),
                    capture(ScrapeMode::JsonPath, "$..thread_id", "{}"),
                ),
                (
                    "usage".to_owned(),
                    capture(ScrapeMode::JsonPath, "$..usage", CODEX_USAGE_FIELDS),
                ),
            ]),
            ..Default::default()
        }
    }

    fn claude_adapter() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["claude".to_owned()],
            scrape: BTreeMap::from([
                (
                    "sessionRef".to_owned(),
                    capture(ScrapeMode::JsonPath, "$..session_id", "{}"),
                ),
                (
                    "usage".to_owned(),
                    capture(ScrapeMode::JsonPath, "$..usage", CLAUDE_USAGE_FIELDS),
                ),
                (
                    "usageCost".to_owned(),
                    capture(
                        ScrapeMode::JsonPathLast,
                        "$[?@.type == 'result'].total_cost_usd",
                        CLAUDE_COST_FIELDS,
                    ),
                ),
            ]),
            ..Default::default()
        }
    }

    fn scraped(adapter: &AdapterConfig, name: &str, stream: &str) -> UsageObservation {
        let adapters = BTreeMap::from([(name.to_owned(), adapter.clone())]);
        let captures = AdapterEngine::new(&adapters)
            .scrape_text(name, stream, "")
            .expect("fixture stream scrapes");
        observe(adapter, &captures)
    }

    #[test]
    fn codex_capture_reconciles_components_to_totals() {
        let observation = scraped(&codex_adapter(), "codex", CODEX_STREAM);
        let breakdown = observation.breakdown().expect("codex reported usage");
        assert_eq!(breakdown.shape, UsageShape::Components);
        assert!(breakdown.unreadable_fields.is_empty());
        // The real `turn.completed` this capture ends with reports all five
        // keys codex emits: input_tokens 7060166 (cache-inclusive),
        // cached_input_tokens 6798080, cache_write_input_tokens 0,
        // output_tokens 32842, reasoning_output_tokens 15163.
        assert_eq!(breakdown.input_tokens_as_reported, Some(7060166));
        assert_eq!(breakdown.input_tokens, Some(262086));
        assert_eq!(breakdown.cache_read_tokens, Some(6798080));
        // Zero is what the harness measured, so it is stated. An absent
        // `cacheWriteTokens` here would be the absence-for-zero conflation the
        // record exists to prevent.
        assert_eq!(breakdown.cache_write_tokens, Some(0));
        assert_eq!(breakdown.output_tokens, Some(32842));
        assert_eq!(breakdown.reasoning_tokens, Some(15163));
        assert_eq!(
            breakdown.total_tokens,
            Some(UsageTotalTokens {
                value: 7093008,
                source: UsageTotalSource::DerivedFromComponents,
            })
        );
        assert_eq!(
            breakdown.reconciliation(),
            UsageReconciliation::Reconciled { total: 7093008 }
        );
        // Independent arithmetic: codex's own convention totals a turn as
        // input_tokens + output_tokens, with the cached tokens already inside
        // the input figure and reasoning already inside output. 7060166 +
        // 32842 is that number, computed without touching the components this
        // record split apart.
        assert_eq!(7060166_u64 + 32842, 7093008);
        // Reasoning is nested within output, never added to it: a total that
        // double-counted it would be 7093008 + 15163.
        assert_eq!(breakdown.component_sum(), Some(7093008));
        assert_eq!(breakdown.cost, None, "codex reports no cost");
    }

    #[test]
    fn declaring_codex_cache_write_moves_neither_the_meter_nor_the_derived_total() {
        // `cache_write_input_tokens` is 0 in every real codex turn in this
        // project's corpus, but the record must not depend on that: pin what
        // reading it does and does not change.
        let with_cache_write = scraped(&codex_adapter(), "codex", CODEX_STREAM);
        let mut narrower = codex_adapter();
        narrower.scrape.insert(
            "usage".to_owned(),
            capture(
                ScrapeMode::JsonPath,
                "$..usage",
                r#"{"inputTokensWithCacheRead":["input_tokens"],"cacheReadTokens":["cached_input_tokens"],"outputTokens":["output_tokens"]}"#,
            ),
        );
        let without = scraped(&narrower, "codex", CODEX_STREAM);
        assert_eq!(with_cache_write.meter_amount(), without.meter_amount());
        assert_eq!(
            with_cache_write.breakdown().unwrap().total_tokens,
            without.breakdown().unwrap().total_tokens
        );
        // A nonzero cache write would move the derived total and still not the
        // meter, which reads only the harness's own input and output figures.
        let nonzero = observe(
            &codex_adapter(),
            &ScrapeResult {
                captures: BTreeMap::from([(
                    "usage".to_owned(),
                    json!({
                        "input_tokens": 7060166,
                        "cached_input_tokens": 6798080,
                        "cache_write_input_tokens": 1000,
                        "output_tokens": 32842,
                        "reasoning_output_tokens": 15163
                    }),
                )]),
            },
        );
        assert_eq!(
            nonzero.breakdown().unwrap().total_tokens,
            Some(UsageTotalTokens {
                value: 7094008,
                source: UsageTotalSource::DerivedFromComponents,
            })
        );
        assert_eq!(nonzero.meter_amount(), with_cache_write.meter_amount());
    }

    #[test]
    fn claude_code_capture_reconciles_components_and_keeps_reported_cost() {
        let observation = scraped(&claude_adapter(), "claude-code", CLAUDE_STREAM);
        let breakdown = observation.breakdown().expect("claude-code reported usage");
        assert_eq!(breakdown.shape, UsageShape::Components);
        assert!(breakdown.unreadable_fields.is_empty());
        // Three assistant turns carry usage, a user turn and a rate-limit
        // event carry none; the result event's cumulative usage is the one
        // that survives. Its `iterations` array carries an `input_tokens` of
        // its own — the record must read the top-level 83, not that 2.
        assert_eq!(breakdown.input_tokens, Some(83));
        assert_eq!(breakdown.input_tokens_as_reported, Some(83));
        assert_eq!(breakdown.cache_read_tokens, Some(11093140));
        assert_eq!(breakdown.cache_write_tokens, Some(265127));
        assert_eq!(breakdown.output_tokens, Some(22298));
        assert_eq!(
            breakdown.reasoning_tokens, None,
            "claude reports no separate reasoning figure; thinking is inside output"
        );
        assert_eq!(
            breakdown.total_tokens,
            Some(UsageTotalTokens {
                value: 11380648,
                source: UsageTotalSource::DerivedFromComponents,
            })
        );
        assert_eq!(83_u64 + 11093140 + 265127 + 22298, 11380648);
        assert_eq!(
            breakdown.reconciliation(),
            UsageReconciliation::Reconciled { total: 11380648 }
        );
        // The result event also carries a per-model `costUSD` nested under
        // `modelUsage`. Cost comes from the harness's own `total_cost_usd` and
        // is retained as the number that arrived, unrounded.
        let cost = breakdown.cost.as_ref().expect("claude-code reports cost");
        assert_eq!(cost.currency, "USD");
        assert_eq!(cost.as_f64(), Some(8.755705000000003));
        assert_eq!(
            serde_json::to_string(&cost.amount).unwrap(),
            "8.755705000000003"
        );
    }

    #[test]
    fn a_stream_with_no_usage_is_a_typed_absence_not_a_zero() {
        // A complete real codex run that hit a rate limit: thread started,
        // turn started, turn failed, no usage anywhere.
        let codex = scraped(&codex_adapter(), "codex", CODEX_QUIET_STREAM);
        // A real claude capture truncated before its first usage-bearing
        // event, which is what the capture file holds when a job is preempted
        // during its first turn.
        let claude = scraped(&claude_adapter(), "claude-code", CLAUDE_QUIET_STREAM);
        for observation in [&codex, &claude] {
            assert_eq!(observation, &UsageObservation::NotReported);
            assert!(observation.is_absent());
            assert_eq!(observation.breakdown(), None);
            assert_eq!(observation.meter_amount(), None);
            assert_eq!(
                serde_json::to_value(observation).unwrap(),
                json!({"state": "not-reported"})
            );
        }
    }

    #[test]
    fn the_three_states_stay_distinct_in_the_persisted_record() {
        let mut silent = claude_adapter();
        silent.scrape.remove("usage");
        silent.scrape.remove("usageCost");
        let not_declared = scraped(&silent, "claude-code", CLAUDE_STREAM);
        let not_reported = scraped(&claude_adapter(), "claude-code", CLAUDE_QUIET_STREAM);
        let zero = observe(
            &claude_adapter(),
            &ScrapeResult {
                captures: BTreeMap::from([(
                    "usage".to_owned(),
                    json!({
                        "input_tokens": 0,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "output_tokens": 0
                    }),
                )]),
            },
        );

        assert_eq!(not_declared, UsageObservation::NotDeclared);
        assert_eq!(not_reported, UsageObservation::NotReported);
        // A reported zero is a measurement. It is neither absence, and it
        // renders as a breakdown of zeros rather than as nothing.
        assert!(!zero.is_absent());
        let breakdown = zero.breakdown().expect("a reported zero is reported");
        assert_eq!(breakdown.input_tokens, Some(0));
        assert_eq!(breakdown.output_tokens, Some(0));
        assert_eq!(
            breakdown.total_tokens,
            Some(UsageTotalTokens {
                value: 0,
                source: UsageTotalSource::DerivedFromComponents,
            })
        );

        // The distinction survives the round trip through the durable record,
        // which is where a later rollup reads it from.
        let persisted = [&not_declared, &not_reported, &zero]
            .map(|observation| serde_json::to_string(observation).unwrap());
        assert_eq!(persisted[0], r#"{"state":"not-declared"}"#);
        assert_eq!(persisted[1], r#"{"state":"not-reported"}"#);
        assert!(
            persisted[2].starts_with(r#"{"state":"reported","breakdown":{"shape":"components""#),
            "reported zero serialized as {}",
            persisted[2]
        );
        for encoded in &persisted {
            let decoded: UsageObservation = serde_json::from_str(encoded).unwrap();
            assert_eq!(
                encoded,
                &serde_json::to_string(&decoded).unwrap(),
                "the persisted record round-trips"
            );
        }
        assert_ne!(persisted[0], persisted[1]);
        assert_ne!(persisted[1], persisted[2]);
    }

    #[test]
    fn a_declared_but_unmapped_usage_object_is_reported_without_components() {
        let observation = observe(
            &claude_adapter(),
            &ScrapeResult {
                captures: BTreeMap::from([(
                    "usage".to_owned(),
                    json!({"service_tier": "standard"}),
                )]),
            },
        );
        let breakdown = observation
            .breakdown()
            .expect("the harness did report something");
        assert_eq!(breakdown.shape, UsageShape::Unmapped);
        assert_eq!(breakdown.component_sum(), None);
        assert_eq!(
            breakdown.reconciliation(),
            UsageReconciliation::NoComponents
        );
        assert_eq!(observation.meter_amount(), None);
    }

    #[test]
    fn a_lump_total_is_a_lump_and_says_so() {
        let mut adapter = claude_adapter();
        adapter.scrape.insert(
            "usage".to_owned(),
            capture(
                ScrapeMode::JsonPath,
                "$..usage",
                r#"{"totalTokens":["total_tokens"]}"#,
            ),
        );
        let observation = observe(
            &adapter,
            &ScrapeResult {
                captures: BTreeMap::from([("usage".to_owned(), json!({"total_tokens": 4608}))]),
            },
        );
        let breakdown = observation.breakdown().expect("a lump is still reported");
        assert_eq!(breakdown.shape, UsageShape::Lump);
        assert_eq!(
            breakdown.total_tokens,
            Some(UsageTotalTokens {
                value: 4608,
                source: UsageTotalSource::HarnessReported,
            })
        );
        assert_eq!(
            breakdown.reconciliation(),
            UsageReconciliation::NoComponents
        );
    }

    #[test]
    fn a_reported_total_that_disagrees_with_its_components_is_a_mismatch() {
        let mut adapter = claude_adapter();
        adapter.scrape.insert(
            "usage".to_owned(),
            capture(
                ScrapeMode::JsonPath,
                "$..usage",
                r#"{"inputTokens":["input_tokens"],"outputTokens":["output_tokens"],"totalTokens":["total_tokens"]}"#,
            ),
        );
        let observation = observe(
            &adapter,
            &ScrapeResult {
                captures: BTreeMap::from([(
                    "usage".to_owned(),
                    json!({"input_tokens": 10, "output_tokens": 5, "total_tokens": 99}),
                )]),
            },
        );
        let breakdown = observation.breakdown().unwrap();
        // Neither number is corrected and neither is dropped.
        assert_eq!(
            breakdown.total_tokens,
            Some(UsageTotalTokens {
                value: 99,
                source: UsageTotalSource::HarnessReported,
            })
        );
        assert_eq!(breakdown.component_sum(), Some(15));
        assert_eq!(
            breakdown.reconciliation(),
            UsageReconciliation::Mismatch {
                reported: 99,
                computed: 15,
            }
        );
    }

    #[test]
    fn an_inclusive_input_smaller_than_its_cache_read_leaves_the_canonical_field_unknown() {
        let observation = observe(
            &codex_adapter(),
            &ScrapeResult {
                captures: BTreeMap::from([(
                    "usage".to_owned(),
                    json!({"input_tokens": 5, "cached_input_tokens": 9, "output_tokens": 1}),
                )]),
            },
        );
        let breakdown = observation.breakdown().unwrap();
        assert_eq!(breakdown.input_tokens, None, "no plausible zero");
        assert_eq!(breakdown.input_tokens_as_reported, Some(5));
        // The meter still charges what the harness billed.
        assert_eq!(observation.meter_amount(), Some(6));
    }

    #[test]
    fn a_field_the_harness_emitted_in_the_wrong_shape_is_named_not_silently_absent() {
        let observation = observe(
            &claude_adapter(),
            &ScrapeResult {
                captures: BTreeMap::from([(
                    "usage".to_owned(),
                    json!({"input_tokens": "many", "output_tokens": 5}),
                )]),
            },
        );
        let breakdown = observation.breakdown().unwrap();
        assert_eq!(breakdown.unreadable_fields, vec!["inputTokens".to_owned()]);
        assert_eq!(breakdown.input_tokens, None);
        assert_eq!(observation.meter_amount(), None);
    }

    /// The reader the built-in pool meter used before this record existed,
    /// transcribed from `scraped_token_amount` as it stood at
    /// `daemon/rpc/query.rs` before normalization. The property under test is
    /// that the new path charges the same number for every shape the old one
    /// ever saw.
    fn legacy_scraped_token_amount(captures: &ScrapeResult) -> Option<u64> {
        let usage = captures.captures.get("usage")?.as_object()?;
        let amount = if let Some(total) = usage.get("total_tokens") {
            total.as_u64()?
        } else {
            let input = match usage.get("input_tokens") {
                Some(value) => value.as_u64()?,
                None => 0,
            };
            let output = match usage.get("output_tokens") {
                Some(value) => value.as_u64()?,
                None => 0,
            };
            input.checked_add(output)?
        };
        (amount > 0).then_some(amount)
    }

    fn legacy_shaped_adapter() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["agent".to_owned()],
            scrape: BTreeMap::from([(
                "usage".to_owned(),
                capture(ScrapeMode::JsonPath, "$..usage", "{}"),
            )]),
            ..Default::default()
        }
    }

    #[test]
    fn the_meter_charges_the_same_number_the_pre_normalization_reader_did() {
        let corpus = [
            json!({"input_tokens": 30, "output_tokens": 50}),
            json!({"total_tokens": 10}),
            json!({"total_tokens": "80"}),
            json!({"input_tokens": -1, "output_tokens": 4}),
            json!({"input_tokens": 0, "output_tokens": 0}),
            json!({"total_tokens": 0}),
            json!({"input_tokens": 999999}),
            json!({"output_tokens": 17}),
            json!({"input_tokens": u64::MAX, "output_tokens": 1}),
            json!({"total_tokens": 4608, "input_tokens": 4096, "output_tokens": 512}),
            json!({"service_tier": "standard"}),
            json!({}),
            json!(7),
            json!("nope"),
        ];
        let adapter = legacy_shaped_adapter();
        for usage in corpus {
            let captures = ScrapeResult {
                captures: BTreeMap::from([("usage".to_owned(), usage.clone())]),
            };
            assert_eq!(
                observe(&adapter, &captures).meter_amount(),
                legacy_scraped_token_amount(&captures),
                "meter amount changed for {usage}"
            );
        }
        let empty = ScrapeResult::default();
        assert_eq!(
            observe(&adapter, &empty).meter_amount(),
            legacy_scraped_token_amount(&empty)
        );
    }

    #[test]
    fn the_meter_diverges_from_the_pre_normalization_reader_only_upward() {
        // The shapes where normalization does not reproduce the old reader.
        // No harness in the corpus emits either — real codex and real
        // claude-code both emit integers — but the divergence is real, so it
        // is stated here rather than left to be discovered.
        let divergent = [
            (
                json!({"total_tokens": null, "input_tokens": 100, "output_tokens": 50}),
                150,
            ),
            (json!({"input_tokens": null, "output_tokens": 50}), 50),
            (json!({"total_tokens": 100.0}), 100),
            (json!({"input_tokens": 30.0, "output_tokens": 50}), 80),
            (json!({"input_tokens": 30, "output_tokens": 50.0}), 80),
        ];
        let adapter = legacy_shaped_adapter();
        for (usage, charged) in divergent {
            let captures = ScrapeResult {
                captures: BTreeMap::from([("usage".to_owned(), usage.clone())]),
            };
            assert_eq!(
                legacy_scraped_token_amount(&captures),
                None,
                "the old reader wrote no meter event for {usage}"
            );
            assert_eq!(
                observe(&adapter, &captures).meter_amount(),
                Some(charged),
                "{usage} now charges a number"
            );
        }
    }

    #[test]
    fn the_preset_mappings_do_not_move_the_number_the_meter_charges() {
        // A richer breakdown must not become a bigger bill. Both presets
        // charge exactly what the pre-normalization reader charged for the
        // same stream.
        let codex = scraped(&codex_adapter(), "codex", CODEX_STREAM);
        assert_eq!(codex.meter_amount(), Some(7060166 + 32842));
        let claude = scraped(&claude_adapter(), "claude-code", CLAUDE_STREAM);
        assert_eq!(
            claude.meter_amount(),
            Some(83 + 22298),
            "the cache halves are recorded but never charged"
        );
    }

    #[test]
    fn declared_paths_reach_nested_and_root_values() {
        let value = json!({"a": {"b": [1, {"c": 9}]}});
        assert_eq!(resolve_path(&value, "$"), Some(&value));
        assert_eq!(resolve_path(&value, ""), Some(&value));
        assert_eq!(resolve_path(&value, "a.b.0"), Some(&json!(1)));
        assert_eq!(resolve_path(&value, "$.a.b.1.c"), Some(&json!(9)));
        assert_eq!(resolve_path(&value, "a.missing"), None);
        assert_eq!(resolve_path(&value, "a.b.9"), None);
    }

    #[test]
    fn the_first_candidate_path_that_resolves_wins() {
        let adapter = AdapterConfig {
            argv: vec!["agent".to_owned()],
            scrape: BTreeMap::from([(
                "usage".to_owned(),
                capture(
                    ScrapeMode::JsonPath,
                    "$..usage",
                    r#"{"outputTokens":["completion_tokens","output_tokens"]}"#,
                ),
            )]),
            ..Default::default()
        };
        let first = observe(
            &adapter,
            &ScrapeResult {
                captures: BTreeMap::from([(
                    "usage".to_owned(),
                    json!({"completion_tokens": 3, "output_tokens": 8}),
                )]),
            },
        );
        assert_eq!(first.breakdown().unwrap().output_tokens, Some(3));
        let fallback = observe(
            &adapter,
            &ScrapeResult {
                captures: BTreeMap::from([("usage".to_owned(), json!({"output_tokens": 8}))]),
            },
        );
        assert_eq!(fallback.breakdown().unwrap().output_tokens, Some(8));
    }

    #[test]
    fn records_written_before_this_field_existed_read_back_unchanged() {
        let fixture: Value = serde_json::from_str(N_MINUS_1).unwrap();

        // The durable row gains an optional field, so an N-1 row still parses
        // and re-serializes to exactly the bytes it arrived as.
        // Two arms: the rowVersion current main writes, which is the true N-1
        // shape, and the older one a pinned-behind estate may still hold.
        for arm in ["row", "rowVersion3"] {
            let row_json = fixture[arm].clone();
            let row: crate::taskdb::RowSeed = serde_json::from_value(row_json.clone()).unwrap();
            assert_eq!(
                row.usage, None,
                "{arm}: no attempt was ever scraped for this row"
            );
            let reserialized = serde_json::to_value(&row).unwrap();
            assert!(
                reserialized.get("usage").is_none(),
                "{arm}: an absent usage record adds no key to a row that never had one"
            );
            for (key, value) in row_json.as_object().unwrap() {
                assert_eq!(
                    reserialized.get(key),
                    Some(value),
                    "{arm}: field {key} did not read back unchanged"
                );
            }
        }
        assert_eq!(
            fixture["row"]["rowVersion"],
            json!(crate::taskdb::CURRENT_ROW_VERSION),
            "the N-1 arm must track the row version main actually writes"
        );

        // The attestation payload gains a sibling key, so an N-1 payload keeps
        // producing the captures the daemon reads out of it.
        let payload = &fixture["attestationPayload"];
        assert!(
            payload.get("usage").is_none(),
            "N-1 payloads carry no usage"
        );
        let captures: BTreeMap<String, Value> =
            serde_json::from_value(payload["captures"].clone()).unwrap();
        let captures = ScrapeResult { captures };
        assert_eq!(captures.session_ref().unwrap(), Some("codex-usage-thread"));
        assert_eq!(captures.usage().unwrap()["input_tokens"], json!(7060166));

        // And normalizing that retained capture reproduces the record a fresh
        // scrape of the same stream produces, so a restart does not change a
        // completed attempt's answer.
        assert_eq!(
            observe(&codex_adapter(), &captures),
            scraped(&codex_adapter(), "codex", CODEX_STREAM)
        );
    }
}
