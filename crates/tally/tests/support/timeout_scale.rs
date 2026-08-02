//! Test-harness timeout scaling.
//!
//! The live flow suite waits on fixed wall-clock budgets. Those budgets are sized
//! for an idle machine, so a loaded gate host can miss one and turn a healthy tree
//! red. `TALLY_TEST_TIMEOUT_SCALE` multiplies every budget routed through
//! [`scaled`] so the wait can be widened without editing the tests and without
//! changing what they assert.
//!
//! Rules:
//!
//! - Unset means `1`: the budgets are byte-identical to the unscaled ones.
//! - A set value must parse as a number in `[1, `[`MAX_TIMEOUT_SCALE`]`]`. Anything
//!   else — empty, non-numeric, zero, negative, infinite, below 1, or large enough
//!   to overflow a `Duration` — panics naming this variable. A gate that quietly
//!   ignored a misspelled knob would be worse than having no knob, and a value
//!   below 1 would *tighten* every budget: the reds it produced would read as
//!   product timeouts with nothing pointing back at the knob that caused them.
//!
//! Reach: the variable is read from the test process environment, so it applies on
//! the direct `cargo test` reproduce path (`TALLY_TEST_TIMEOUT_SCALE=3 cargo test
//! -p tally --test flow_live`). It deliberately does **not** reach the tests run by
//! `nix flake check`: those execute inside `buildRustPackage`'s pure sandbox, which
//! sees no host environment. Diagnosing a red gate happens on the direct path
//! anyway, which is where the knob is useful.
//!
//! `test/fleet-gate.sh` scrubs this variable from its own `cargo test` stage, so an
//! ambient value left over from a reproduce run cannot silently widen the gate. A
//! gate on a genuinely loaded host is widened through `TALLY_GATE_TIMEOUT_SCALE`
//! instead, which the runner records in the transcript header.

use std::sync::OnceLock;
use std::time::Duration;

pub const TIMEOUT_SCALE_ENV: &str = "TALLY_TEST_TIMEOUT_SCALE";

/// The largest accepted multiplier. The bound exists so `Duration::mul_f64`
/// cannot overflow and panic inside libcore, where the backtrace names this
/// variable nowhere; 1000× the loosest budget in the suite is already 16 hours,
/// which is far past any honest allowance for a loaded host.
pub const MAX_TIMEOUT_SCALE: f64 = 1000.0;

/// Widen a fixed wait budget by the configured scale.
pub fn scaled(budget: Duration) -> Duration {
    apply(budget, env_scale())
}

/// [`scaled`] against an explicit raw value instead of the process environment.
pub fn scaled_with(budget: Duration, raw: Option<&str>) -> Duration {
    apply(budget, parse_scale(raw))
}

/// The multiplier in force for this process, so a wait that expires anyway can
/// name the knob and its value rather than reporting a bare elapsed budget.
pub fn effective_scale() -> f64 {
    env_scale()
}

fn apply(budget: Duration, scale: f64) -> Duration {
    if scale == 1.0 {
        return budget;
    }
    budget.mul_f64(scale)
}

fn env_scale() -> f64 {
    static SCALE: OnceLock<f64> = OnceLock::new();
    *SCALE.get_or_init(|| parse_scale(std::env::var(TIMEOUT_SCALE_ENV).ok().as_deref()))
}

fn parse_scale(raw: Option<&str>) -> f64 {
    let Some(raw) = raw else {
        return 1.0;
    };
    let scale = raw.trim().parse::<f64>().unwrap_or_else(|_| {
        panic!(
            "{TIMEOUT_SCALE_ENV}={raw:?} is not a number; \
             set a multiplier between 1 and {MAX_TIMEOUT_SCALE:.0} such as 3, \
             or unset {TIMEOUT_SCALE_ENV} to use the unscaled budgets"
        )
    });
    // One refusal per bound. A single message for both had to pick one
    // explanation, and the one it picked was true only of the lower bound: a
    // reader who typed `1e30` was told about values below 1 and never about the
    // overflow that is the actual reason the upper bound exists.
    assert!(
        scale.is_finite(),
        "{TIMEOUT_SCALE_ENV}={raw:?} must be a finite multiplier between 1 and \
         {MAX_TIMEOUT_SCALE:.0}"
    );
    assert!(
        scale >= 1.0,
        "{TIMEOUT_SCALE_ENV}={raw:?} must be at least 1; the knob only widens \
         budgets, so a value below 1 would tighten them and produce reds that read \
         as product timeouts"
    );
    assert!(
        scale <= MAX_TIMEOUT_SCALE,
        "{TIMEOUT_SCALE_ENV}={raw:?} must be at most {MAX_TIMEOUT_SCALE:.0}; a \
         larger multiplier overflows the scaled `Duration` and panics inside \
         libcore, where nothing names this variable"
    );
    scale
}

#[test]
fn unset_scale_leaves_budgets_untouched() {
    assert_eq!(
        scaled_with(Duration::from_secs(60), None),
        Duration::from_secs(60)
    );
    assert_eq!(
        scaled_with(Duration::from_secs(20), None),
        Duration::from_secs(20)
    );
}

#[test]
fn scale_of_three_triples_the_runner_output_budget() {
    assert_eq!(
        scaled_with(Duration::from_secs(60), Some("3")),
        Duration::from_secs(180)
    );
}

#[test]
fn fractional_and_padded_scales_are_accepted() {
    assert_eq!(
        scaled_with(Duration::from_secs(60), Some("1.5")),
        Duration::from_secs(90)
    );
    assert_eq!(
        scaled_with(Duration::from_secs(60), Some(" 2 ")),
        Duration::from_secs(120)
    );
}

#[test]
fn the_upper_bound_itself_is_accepted() {
    assert_eq!(
        scaled_with(Duration::from_secs(60), Some("1000")),
        Duration::from_secs(60_000)
    );
}

#[test]
#[should_panic(expected = "is not a number")]
fn non_numeric_scale_fails_loudly() {
    scaled_with(Duration::from_secs(60), Some("abc"));
}

#[test]
#[should_panic(expected = "is not a number")]
fn empty_scale_fails_loudly() {
    scaled_with(Duration::from_secs(60), Some(""));
}

#[test]
#[should_panic(expected = "TALLY_TEST_TIMEOUT_SCALE=\"0\" must be at least 1")]
fn zero_scale_fails_loudly() {
    scaled_with(Duration::from_secs(60), Some("0"));
}

#[test]
#[should_panic(expected = "TALLY_TEST_TIMEOUT_SCALE=\"-2\" must be at least 1")]
fn negative_scale_fails_loudly() {
    scaled_with(Duration::from_secs(60), Some("-2"));
}

#[test]
#[should_panic(expected = "must be a finite multiplier between 1 and 1000")]
fn infinite_scale_fails_loudly() {
    scaled_with(Duration::from_secs(60), Some("inf"));
}

#[test]
#[should_panic(expected = "must be a finite multiplier between 1 and 1000")]
fn not_a_number_scale_fails_loudly() {
    scaled_with(Duration::from_secs(60), Some("NaN"));
}

/// The knob widens; it never narrows. A value left over from an experiment used
/// to tighten every budget it reached, which is the one way this harness can
/// invent a red that reads exactly like a product timeout.
#[test]
#[should_panic(expected = "would tighten them")]
fn sub_unit_scale_is_rejected_rather_than_narrowing() {
    scaled_with(Duration::from_secs(60), Some("0.5"));
}

/// Each bound explains itself. A single shared message had to pick one
/// explanation and told an over-large value about the lower bound.
#[test]
#[should_panic(expected = "must be at most 1000")]
fn just_past_the_upper_bound_is_rejected_and_says_why() {
    scaled_with(Duration::from_secs(60), Some("1000.5"));
}

/// Without the upper bound this overflowed inside `core::time`, whose panic
/// names neither the knob nor its value. The refusal names both, and explains
/// the overflow rather than the lower bound.
#[test]
#[should_panic(
    expected = "TALLY_TEST_TIMEOUT_SCALE=\"1e30\" must be at most 1000; a larger multiplier overflows"
)]
fn overflowing_scale_is_rejected_before_it_reaches_libcore() {
    scaled_with(Duration::from_secs(60), Some("1e30"));
}

#[test]
fn env_backed_scale_follows_the_same_rule() {
    let raw = std::env::var(TIMEOUT_SCALE_ENV).ok();
    assert_eq!(
        scaled(Duration::from_secs(60)),
        scaled_with(Duration::from_secs(60), raw.as_deref())
    );
    assert_eq!(
        scaled(Duration::from_secs(60)),
        Duration::from_secs(60).mul_f64(effective_scale())
    );
}
