//! Effective runtime budgets for campaign gates, derived from receipts.
//!
//! A gate's `runtimeMaxSec` is OPTIONAL. When a worklist declares one, that
//! number IS the budget and nothing here revises it — the same permanence the
//! declared `maxParallel` carries. When a worklist declares none, the budget is
//! derived from what that gate id has actually cost in this campaign's own
//! attempt receipts, so the number that binds is a measurement with a stated
//! multiplier rather than an unvalidated guess (`specs/substrate/evidence/
//! vestige-sweep.md` V-6: four gate numbers convicted as guesses with "zero
//! recorded firings, all week", destination already adjudicated as observed
//! duration times slack).
//!
//! Every constant below is the whole reason the guesses could retire, so each
//! one carries its own ruling rather than a value.

use serde::{Deserialize, Serialize};

/// The budget is twice the worst run this gate has ever recorded.
///
/// The high water is one sample of a distribution the host's load skews: the
/// same work runs materially slower under memory pressure without becoming a
/// different job (vestige-sweep interaction I-B, where reclaim thrash turns an
/// OOM into a reported "gate timeout"). Doubling absorbs that. A gate that
/// still exceeds this budget has a receipt proving it ran more than twice its
/// own worst observed run — a timeout that names the measurement it broke.
pub const GATE_BUDGET_SLACK_PERCENT: u64 = 200;

/// The smallest budget the derivation will hand an observed gate.
///
/// Below a minute, process startup and scheduler placement dominate the
/// measurement, and a multiplier over noise is still noise. A gate whose worst
/// recorded run is seconds long gets a minute regardless.
pub const GATE_BUDGET_MIN_DERIVED_SEC: u64 = 60;

/// The budget for a gate id with no recorded firing at all.
///
/// This is the one number here that no measurement backs, and it is deliberately
/// the largest budget any convicted gate guess ever carried
/// (`epsilon-extension.json:21`, 3600). Retiring the guesses therefore cannot
/// tighten a gate that has produced no evidence to justify tightening it; the
/// gate's first recorded firing replaces this with measurement.
pub const GATE_BUDGET_UNOBSERVED_SEC: u64 = 3_600;

/// Where the number that will bind this gate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateBudgetSource {
    /// The worklist stated the budget; the derivation did not run.
    Declared,
    /// Derived from this gate id's recorded durations.
    Derived,
    /// The gate has never fired, so the un-measured floor binds.
    Unobserved,
}

impl GateBudgetSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Derived => "derived",
            Self::Unobserved => "unobserved",
        }
    }
}

/// The budget one gate will run under, with the reasoning that produced it.
///
/// The `derivation` sentence exists so an operator reading an admission
/// rehearsal can see which budget binds and why, without reconstructing it from
/// the receipt log by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateBudget {
    pub gate_id: String,
    pub runtime_max_sec: u64,
    pub source: GateBudgetSource,
    pub observations: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_high_water_sec: Option<u64>,
    pub derivation: String,
}

/// Resolve the budget for one gate id from its declaration and its receipts.
///
/// `observations` are the recorded durations, in seconds, of every firing of
/// this gate id that the campaign's attempt receipts hold. Order does not
/// matter; only the high water participates.
#[must_use]
pub fn resolve_gate_budget(
    gate_id: &str,
    declared_runtime_max_sec: Option<u64>,
    observations: &[u64],
) -> GateBudget {
    let observed_high_water_sec = observations.iter().copied().max();
    let (runtime_max_sec, source) = match (declared_runtime_max_sec, observed_high_water_sec) {
        (Some(declared), _) => (declared, GateBudgetSource::Declared),
        (None, Some(high_water)) => (
            high_water
                .saturating_mul(GATE_BUDGET_SLACK_PERCENT)
                .saturating_div(100)
                .max(GATE_BUDGET_MIN_DERIVED_SEC),
            GateBudgetSource::Derived,
        ),
        (None, None) => (GATE_BUDGET_UNOBSERVED_SEC, GateBudgetSource::Unobserved),
    };
    let derivation = match source {
        GateBudgetSource::Declared => format!(
            "gate {gate_id}: {runtime_max_sec}s declared by the worklist and honored verbatim"
        ),
        GateBudgetSource::Derived => format!(
            "gate {gate_id}: {runtime_max_sec}s derived from {} receipt observation(s), high water {}s x {GATE_BUDGET_SLACK_PERCENT}% slack (floor {GATE_BUDGET_MIN_DERIVED_SEC}s)",
            observations.len(),
            observed_high_water_sec.unwrap_or_default(),
        ),
        GateBudgetSource::Unobserved => format!(
            "gate {gate_id}: {runtime_max_sec}s from the never-fired floor; no receipt records this gate firing"
        ),
    };
    GateBudget {
        gate_id: gate_id.to_owned(),
        runtime_max_sec,
        source,
        observations: observations.len(),
        observed_high_water_sec,
        derivation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_budget_derives_the_high_water_times_slack() {
        let budget = resolve_gate_budget("tests", None, &[310, 620, 145]);
        assert_eq!(budget.runtime_max_sec, 1_240);
        assert_eq!(budget.source, GateBudgetSource::Derived);
        assert_eq!(budget.observations, 3);
        assert_eq!(budget.observed_high_water_sec, Some(620));
        assert!(
            budget.derivation.contains("high water 620s")
                && budget.derivation.contains("3 receipt observation(s)"),
            "the derivation must name its own evidence: {}",
            budget.derivation
        );
    }

    #[test]
    fn gate_budget_never_derives_below_the_measurement_noise_floor() {
        let budget = resolve_gate_budget("forbid-paths", None, &[4]);
        assert_eq!(budget.runtime_max_sec, GATE_BUDGET_MIN_DERIVED_SEC);
        assert_eq!(budget.source, GateBudgetSource::Derived);
    }

    #[test]
    fn gate_budget_falls_back_to_the_stated_floor_when_a_gate_has_never_fired() {
        let budget = resolve_gate_budget("tests", None, &[]);
        assert_eq!(budget.runtime_max_sec, GATE_BUDGET_UNOBSERVED_SEC);
        assert_eq!(budget.source, GateBudgetSource::Unobserved);
        assert_eq!(budget.observed_high_water_sec, None);
        assert!(
            budget.derivation.contains("never-fired floor"),
            "an unobserved gate must say so: {}",
            budget.derivation
        );
    }

    #[test]
    fn gate_budget_declared_beats_every_observation() {
        let budget = resolve_gate_budget("tests", Some(45), &[620, 900]);
        assert_eq!(budget.runtime_max_sec, 45);
        assert_eq!(budget.source, GateBudgetSource::Declared);
        assert_eq!(
            budget.observed_high_water_sec,
            Some(900),
            "the receipts stay visible even when the declaration binds"
        );
        assert!(
            budget.derivation.contains("honored verbatim"),
            "a declared budget must say it was not revised: {}",
            budget.derivation
        );
    }
}
