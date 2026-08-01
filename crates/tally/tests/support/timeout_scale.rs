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
//! - A set value must parse as a positive, finite number. Anything else — empty,
//!   non-numeric, zero, negative, infinite — panics. A gate that quietly ignored a
//!   misspelled knob would be worse than having no knob.
//!
//! Reach: the variable is read from the test process environment, so it applies on
//! the direct `cargo test` reproduce path (`TALLY_TEST_TIMEOUT_SCALE=3 cargo test
//! -p tally --test flow_live`). It deliberately does **not** reach the tests run by
//! `nix flake check`: those execute inside `buildRustPackage`'s pure sandbox, which
//! sees no host environment. Diagnosing a red gate happens on the direct path
//! anyway, which is where the knob is useful.

use std::sync::OnceLock;
use std::time::Duration;

pub const TIMEOUT_SCALE_ENV: &str = "TALLY_TEST_TIMEOUT_SCALE";

/// Widen a fixed wait budget by the configured scale.
pub fn scaled(budget: Duration) -> Duration {
    apply(budget, env_scale())
}

/// [`scaled`] against an explicit raw value instead of the process environment.
pub fn scaled_with(budget: Duration, raw: Option<&str>) -> Duration {
    apply(budget, parse_scale(raw))
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
             set a positive multiplier such as 3, or unset it to use the unscaled budgets"
        )
    });
    assert!(
        scale.is_finite() && scale > 0.0,
        "{TIMEOUT_SCALE_ENV}={raw:?} must be a positive, finite multiplier"
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
#[should_panic(expected = "must be a positive, finite multiplier")]
fn zero_scale_fails_loudly() {
    scaled_with(Duration::from_secs(60), Some("0"));
}

#[test]
#[should_panic(expected = "must be a positive, finite multiplier")]
fn negative_scale_fails_loudly() {
    scaled_with(Duration::from_secs(60), Some("-2"));
}

#[test]
#[should_panic(expected = "must be a positive, finite multiplier")]
fn infinite_scale_fails_loudly() {
    scaled_with(Duration::from_secs(60), Some("inf"));
}

#[test]
fn env_backed_scale_follows_the_same_rule() {
    let raw = std::env::var(TIMEOUT_SCALE_ENV).ok();
    assert_eq!(
        scaled(Duration::from_secs(60)),
        scaled_with(Duration::from_secs(60), raw.as_deref())
    );
}
