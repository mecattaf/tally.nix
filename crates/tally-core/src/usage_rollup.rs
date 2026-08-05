//! Rolling per-attempt usage up to one flow run.
//!
//! [`crate::usage`] normalizes what one attempt's harness reported.  This
//! module answers the next question — "what did this run cost" — and the whole
//! of its difficulty is that the answer is a sum over evidence that is
//! advisory, partial, and shaped differently per harness. A sum that hides any
//! of those three is worse than no sum at all: it reads as a measurement, it is
//! wrong in the reassuring direction, and nothing about its shape says so.
//! Four rules follow, and every field below exists to keep one of them.
//!
//! **1. Read per attempt, from the attestation ledger.** The durable row holds
//! only the most recently scraped attempt — exactly as `sessionRef` and
//! `finalMessage` do — so summing rows would charge a three-attempt task once.
//! Only the `reported` state has a durable per-attempt seat: the advisory
//! attestation ledger, keyed by `taskUuid`/`attempt`/`leaseEpoch`. The two
//! typed absences live in daemon memory and degrade to plain absence across a
//! restart, which is why coverage counts *what the ledger holds*, and counts
//! members it holds nothing about ([`UsageCoverage::tasks_without_attestation`])
//! rather than quietly excluding them from the denominator.
//!
//! **2. `inputTokens` alone is not the cross-harness fresh-input figure.**
//! claude-code's `cache_creation_input_tokens` are fresh, uncached prompt
//! tokens its `input_tokens` *excludes*; codex has no cache-write volume in any
//! observed capture. A rollup that summed `inputTokens` alone would understate
//! claude by its entire cache-write volume — in this project's own #381 fixture
//! that is 23,000 of 65,312 tokens, more than a third — while printing a number
//! that looks directly comparable to codex's. The comparable figure is
//! `inputTokens + cacheWriteTokens`, surfaced here as
//! [`UsageTokenRollup::fresh_input_tokens`], with the addition stated in
//! [`ROLLUP_COMPOSITION`] rather than left for a reader to infer.
//!
//! **3. Reasoning tokens are nested inside output tokens, never added to
//! them.** Codex reports `reasoning_output_tokens` *within* `output_tokens`
//! (`16075 + 221 = 16296 = total_tokens`, with `reasoning_output_tokens: 71`
//! inside the 221). It is rolled up so an operator can see how much of the
//! output was reasoning; it is never part of any total.
//!
//! **4. Say where each number came from.** A total is `harness-reported` only
//! when the adapter declared a `totalTokens` mapping and the harness filled it;
//! otherwise tally derives the total from the components and grades it
//! `derived-from-components`. **No shipped preset declares `totalTokens`** —
//! not `codex`, whose real `turn.completed` carries no `total_tokens` at all,
//! and not `claude-code`, whose `result` event carries a cumulative usage
//! object of components without a total among them. So every run over the
//! shipped presets reads `derived-from-components` today, including a run
//! spanning both harnesses. `harness-reported`, and the
//! [`UsageRollupTotalSource::Mixed`] grade that a run mixing the two produces,
//! are reserved for an operator-defined adapter that declares the mapping
//! (`extraConfig`-style adapter authoring, documented at
//! `nix/modules/common.nix`). The grade is published either way, because a
//! consumer must not have to know which kind of adapter ran to know what the
//! number is. The whole rollup is graded
//! [`FactAuthority::AdvisoryProviderCapture`]: it is what harnesses said about
//! themselves, never a bill tally verified.
//!
//! Cost is summed only where a harness reported it, and it is the harness's own
//! `costUsd`. Tally's cgroup `charge` is a different quantity, is not summed
//! here, and — per the ratified `W-382-RECORDER` waiver — is a **floor** that
//! includes tally's own exit-recorder overhead. [`UsageCostRollup::basis`]
//! carries that statement onto the wire beside the number.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::query_v2::FactAuthority;
use crate::usage::{UsageObservation, UsageReconciliation, UsageTotalSource};
use crate::witness::AttestationRecord;

/// Payload kind the exit recorder writes one of per scraped attempt.
const ADAPTER_SCRAPE_KIND: &str = "adapter-scrape";

/// Where the rollup's numbers came from.
pub const ROLLUP_PROVENANCE: &str =
    "adapter-scrape attestations, per attempt, keyed by taskUuid/attempt/leaseEpoch";

/// Exactly what the run total is a sum over. Stated on the wire because the
/// defect this rollup exists to avoid is a figure computed from the wrong one
/// of several similarly named token fields.
pub const ROLLUP_COMPOSITION: &str = concat!(
    "totalTokens sums each attempt's own total (inputTokens + cacheReadTokens + ",
    "cacheWriteTokens + outputTokens where the harness stated no total of its own); ",
    "freshInputTokens = inputTokens + cacheWriteTokens, which is the cross-harness ",
    "uncached-prompt figure that inputTokens alone understates; reasoningTokens is ",
    "nested inside outputTokens and is never added to any total; each harness's own ",
    "inputTokensAsReported is not summed, because the two conventions are not ",
    "commensurable"
);

/// What cost here is and is not.
pub const ROLLUP_COST_BASIS: &str = concat!(
    "harness-reported costUsd only, summed over the attempts that reported it. ",
    "Tally's cgroup charge is a distinct quantity, is not summed here, and is a ",
    "floor: it includes tally's own exit-recorder overhead and is not pure job cost"
);

/// The advisory attestation ledger as one rollup reader sees it.
///
/// The `verified` flag travels with the records because an unverified ledger
/// must not produce a confident-looking zero: the rollup reports
/// [`UsageRollupCaveat::LedgerUnverified`] and sums nothing.
#[derive(Debug, Clone, Copy)]
pub struct AttestationEvidence<'a> {
    verified: bool,
    records: &'a [AttestationRecord],
}

impl<'a> AttestationEvidence<'a> {
    #[must_use]
    pub const fn new(verified: bool, records: &'a [AttestationRecord]) -> Self {
        Self { verified, records }
    }

    /// Evidence that could not be read at all: a corrupt chain, or a query path
    /// that did not open the ledger.
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            verified: false,
            records: &[],
        }
    }
}

/// One component's sum and how many attempts contributed to it.
///
/// `attempts` below `UsageCoverage::attempts_reported` means the harnesses that
/// ran this run do not all report this component, and the sum is over the
/// subset that does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageSum {
    pub value: u64,
    pub attempts: usize,
}

impl UsageSum {
    /// Add one attempt's component. Returns false when the sum saturated, which
    /// the caller turns into a caveat rather than a silently capped number.
    fn add(&mut self, component: Option<u64>) -> bool {
        let Some(component) = component else {
            return true;
        };
        self.attempts += 1;
        match self.value.checked_add(component) {
            Some(value) => {
                self.value = value;
                true
            }
            None => {
                self.value = u64::MAX;
                false
            }
        }
    }
}

/// `inputTokens + cacheWriteTokens`: the fresh, uncached prompt volume, which
/// is the only input figure comparable across harnesses.
///
/// The two counts are kept apart because an attempt that reported one half and
/// not the other contributes a floor, not a measurement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageFreshInput {
    pub value: u64,
    /// Attempts that reported both halves.
    pub attempts_complete: usize,
    /// Attempts that reported exactly one half. The missing half contributed
    /// nothing, so this attempt's share of `value` is a floor.
    pub attempts_partial: usize,
}

/// Where a run total came from, once every attempt's own total is summed.
///
/// Reachability depends on the adapter, not on the harness's reputation for
/// verbosity: only a declared `totalTokens` mapping produces
/// [`Self::HarnessReported`]. No shipped preset declares one, so the shipped
/// presets always produce [`Self::DerivedFromComponents`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageRollupTotalSource {
    /// Every contributing attempt's total was stated by its harness, through a
    /// declared `totalTokens` mapping.
    HarnessReported,
    /// Every contributing attempt's total was derived from its components.
    /// What both shipped presets produce.
    DerivedFromComponents,
    /// Both kinds are inside this sum: a run whose nodes span an
    /// operator-defined adapter that declares `totalTokens` and one that does
    /// not.
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageTotalRollup {
    pub value: u64,
    pub attempts: usize,
    pub source: UsageRollupTotalSource,
}

/// Component-wise token sums across the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageTokenRollup {
    /// Input tokens excluding both cache halves. Not the fresh-input figure on
    /// its own — see [`UsageTokenRollup::fresh_input_tokens`].
    pub input_tokens: UsageSum,
    pub cache_read_tokens: UsageSum,
    pub cache_write_tokens: UsageSum,
    /// Output tokens, reasoning included.
    pub output_tokens: UsageSum,
    /// Reasoning tokens, nested inside `output_tokens` rather than added.
    pub reasoning_tokens: UsageSum,
    /// `input_tokens + cache_write_tokens`, the cross-harness fresh-prompt
    /// volume.
    pub fresh_input_tokens: UsageFreshInput,
    /// Sum of each attempt's own total. Absent when no attempt carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<UsageTotalRollup>,
}

/// Cost as harnesses reported it, never as tally computed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageCostRollup {
    /// Absent when no attempt reported a cost, which is not the same fact as a
    /// reported zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_usd: Option<f64>,
    /// Attempts that reported a cost.
    pub attempts: usize,
    /// What this number is, and what it deliberately is not. See
    /// [`ROLLUP_COST_BASIS`].
    pub basis: String,
}

impl Default for UsageCostRollup {
    fn default() -> Self {
        Self {
            amount_usd: None,
            attempts: 0,
            basis: ROLLUP_COST_BASIS.to_owned(),
        }
    }
}

/// How much of the run the sums actually cover.
///
/// Every count here is over the run's **durable membership**, so a task a run
/// was handed but whose row names a different creating run — the W-316 shape —
/// is inside the denominator and inside the sums.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageCoverage {
    /// Tasks the run durably holds.
    pub tasks: usize,
    /// Member tasks with at least one attempt that contributed a figure to
    /// these sums. An attempt that reported usage the declared mapping could
    /// read nothing out of does not make its task one of these — see
    /// [`UsageCoverage::attempts_reported_without_figures`].
    pub tasks_with_reported_usage: usize,
    /// Member tasks the ledger holds no attempt for at all. An attempt whose
    /// adapter captured nothing writes no attestation, and a task whose
    /// attestations aged out of retention reads the same way; either way these
    /// tasks are invisible to the sums and are counted rather than dropped.
    pub tasks_without_attestation: usize,
    /// Distinct `(task, attempt, leaseEpoch)` triples found for member tasks.
    pub attempts_observed: usize,
    /// Attempts whose attestation carries a `reported` usage record.
    pub attempts_reported: usize,
    /// The subset of [`UsageCoverage::attempts_reported`] that contributed
    /// nothing: the harness reported usage and no figure this rollup sums
    /// survived the adapter's declared mapping.
    ///
    /// This is the ordinary harness-drift shape, and it is why the reported
    /// count alone is not a coverage statement. A harness renames a key, every
    /// declared path resolves to absent — which is not the same as unreadable,
    /// so nothing lands in `unreadableFields` — and
    /// [`crate::usage::observe`] still returns `Reported`, with
    /// [`crate::usage::UsageShape::Unmapped`]. Counting that attempt as covered
    /// would let a run whose mapping resolved nothing read as complete and
    /// costless. It is counted here and raises
    /// [`UsageRollupCaveat::ReportedWithoutFigures`] instead.
    pub attempts_reported_without_figures: usize,
    /// Attempts whose attestation states `not-reported`: a usage scrape was
    /// declared, the stream was read, and it carried no usage.
    pub attempts_not_reported: usize,
    /// Attempts whose attestation states `not-declared`: the adapter declared
    /// no usage scrape.
    pub attempts_not_declared: usize,
    /// Attempts whose attestation predates the usage record, or carries one
    /// this build cannot read. Counted as absence, exactly as `not-declared`
    /// is.
    pub attempts_without_usage_record: usize,
    /// Whether the advisory attestation chain verified. When false nothing was
    /// summed.
    pub ledger_verified: bool,
}

/// Named reasons a reader must not treat these sums as a complete bill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageRollupCaveat {
    /// The advisory attestation chain did not verify. Nothing was summed.
    LedgerUnverified,
    /// Some member task has no attestation at all, so its usage — if any —
    /// is not in these sums.
    MembersWithoutAttestation,
    /// Some observed attempt reported no usage, by either typed absence.
    AttemptsWithoutUsage,
    /// Some attempt reported usage that yielded no figure at all — the
    /// adapter's declared mapping resolved nothing out of what the harness
    /// emitted. The sums are missing that attempt entirely.
    ReportedWithoutFigures,
    /// Some attestation carries no readable usage record.
    UnreadableUsageRecord,
    /// An attempt named a declared field its harness emitted in an unusable
    /// shape, so that field's sum is missing that attempt's share.
    UnreadableFields,
    /// The total mixes harness-stated totals with tally-derived ones.
    MixedTotalAuthority,
    /// An attempt stated a total its own components do not sum to. Neither
    /// number was corrected; the attempt's own total is what was summed.
    TotalComponentMismatch,
    /// An attempt reported one half of the fresh-input figure and not the
    /// other, so `freshInputTokens` is a floor.
    PartialFreshInput,
    /// Cost was reported by only some of the attempts that reported usage.
    PartialCost,
    /// An attempt reported a cost in a currency that is not USD; it was not
    /// summed.
    NonUsdCost,
    /// A sum reached `u64::MAX` and stopped there.
    SumSaturated,
}

/// A run's usage, with the statement of how partial it is attached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageRollup {
    /// Advisory by construction: harnesses reporting on themselves.
    pub authority: FactAuthority,
    /// See [`ROLLUP_PROVENANCE`].
    pub provenance: String,
    /// See [`ROLLUP_COMPOSITION`].
    pub composition: String,
    pub coverage: UsageCoverage,
    pub tokens: UsageTokenRollup,
    pub cost: UsageCostRollup,
    /// Empty exactly when every member task's every attempt reported complete,
    /// self-consistent usage over a verified ledger.
    pub caveats: Vec<UsageRollupCaveat>,
}

impl UsageRollup {
    /// Whether these sums cover every attempt the ledger could speak for.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.caveats.is_empty()
    }
}

/// One attestation's usage, as the ledger holds it.
enum LedgerUsage {
    Observed(UsageObservation),
    /// The payload predates the usage record, or carries one this build cannot
    /// read.
    NoRecord,
}

/// Sum the usage of every attempt the ledger holds for `members`.
///
/// `members` is the run's durable membership. Attestations for any other task
/// are ignored: a rollup is a run's answer, not the daemon's.
#[must_use]
pub fn roll_up<'a>(
    members: impl IntoIterator<Item = &'a str>,
    evidence: &AttestationEvidence<'_>,
) -> UsageRollup {
    let members = members.into_iter().collect::<BTreeSet<_>>();
    let mut coverage = UsageCoverage {
        tasks: members.len(),
        ledger_verified: evidence.verified,
        ..UsageCoverage::default()
    };
    let mut caveats = BTreeSet::new();
    if !evidence.verified {
        caveats.insert(UsageRollupCaveat::LedgerUnverified);
    }

    // One entry per attempt, last writer winning: a re-scrape of the same
    // attempt supersedes the earlier record rather than being charged twice.
    let mut attempts: BTreeMap<(&str, Option<u64>, Option<u64>), LedgerUsage> = BTreeMap::new();
    if evidence.verified {
        for record in evidence.records {
            let payload = &record.payload;
            if payload.get("kind").and_then(Value::as_str) != Some(ADAPTER_SCRAPE_KIND) {
                continue;
            }
            let Some(task) = payload
                .get("taskUuid")
                .and_then(Value::as_str)
                .and_then(|task| members.get(task).copied())
            else {
                continue;
            };
            let key = (
                task,
                payload.get("attempt").and_then(Value::as_u64),
                payload.get("leaseEpoch").and_then(Value::as_u64),
            );
            let usage = match payload.get("usage") {
                None => LedgerUsage::NoRecord,
                Some(value) => match serde_json::from_value::<UsageObservation>(value.clone()) {
                    Ok(observation) => LedgerUsage::Observed(observation),
                    Err(_) => LedgerUsage::NoRecord,
                },
            };
            attempts.insert(key, usage);
        }
    }

    let mut tokens = UsageTokenRollup::default();
    let mut cost = UsageCostRollup::default();
    let mut cost_amount = 0.0_f64;
    let mut tasks_with_usage = BTreeSet::new();
    let mut total_value = 0_u64;
    let mut total_attempts = 0_usize;
    let mut saw_harness_total = false;
    let mut saw_derived_total = false;
    let mut saturated = false;

    for ((task, _, _), usage) in &attempts {
        coverage.attempts_observed += 1;
        let observation = match usage {
            LedgerUsage::NoRecord => {
                coverage.attempts_without_usage_record += 1;
                caveats.insert(UsageRollupCaveat::UnreadableUsageRecord);
                continue;
            }
            LedgerUsage::Observed(observation) => observation,
        };
        let breakdown = match observation {
            UsageObservation::NotDeclared => {
                coverage.attempts_not_declared += 1;
                continue;
            }
            UsageObservation::NotReported => {
                coverage.attempts_not_reported += 1;
                continue;
            }
            UsageObservation::Reported(breakdown) => breakdown,
        };
        coverage.attempts_reported += 1;
        // Reported is a discriminant, not a measurement. Whether this attempt
        // contributes anything is decided by the figures that survived the
        // adapter's declared mapping, and it is decided over exactly the
        // fields this rollup sums: `inputTokensAsReported` is excluded because
        // it is never summed, so an attempt carrying only that would be
        // claiming a contribution the sums do not contain.
        let contributed = breakdown.input_tokens.is_some()
            || breakdown.cache_read_tokens.is_some()
            || breakdown.cache_write_tokens.is_some()
            || breakdown.output_tokens.is_some()
            || breakdown.reasoning_tokens.is_some()
            || breakdown.total_tokens.is_some()
            || breakdown.cost.is_some();
        if contributed {
            tasks_with_usage.insert(*task);
        } else {
            coverage.attempts_reported_without_figures += 1;
            caveats.insert(UsageRollupCaveat::ReportedWithoutFigures);
        }

        saturated |= !tokens.input_tokens.add(breakdown.input_tokens);
        saturated |= !tokens.cache_read_tokens.add(breakdown.cache_read_tokens);
        saturated |= !tokens.cache_write_tokens.add(breakdown.cache_write_tokens);
        saturated |= !tokens.output_tokens.add(breakdown.output_tokens);
        saturated |= !tokens.reasoning_tokens.add(breakdown.reasoning_tokens);

        match (breakdown.input_tokens, breakdown.cache_write_tokens) {
            (None, None) => {}
            (input, cache_write) => {
                if input.is_some() && cache_write.is_some() {
                    tokens.fresh_input_tokens.attempts_complete += 1;
                } else {
                    tokens.fresh_input_tokens.attempts_partial += 1;
                    caveats.insert(UsageRollupCaveat::PartialFreshInput);
                }
                let fresh = input.unwrap_or(0).saturating_add(cache_write.unwrap_or(0));
                match tokens.fresh_input_tokens.value.checked_add(fresh) {
                    Some(value) => tokens.fresh_input_tokens.value = value,
                    None => {
                        tokens.fresh_input_tokens.value = u64::MAX;
                        saturated = true;
                    }
                }
            }
        }

        if let Some(total) = breakdown.total_tokens {
            total_attempts += 1;
            match total.source {
                UsageTotalSource::HarnessReported => saw_harness_total = true,
                UsageTotalSource::DerivedFromComponents => saw_derived_total = true,
            }
            match total_value.checked_add(total.value) {
                Some(value) => total_value = value,
                None => {
                    total_value = u64::MAX;
                    saturated = true;
                }
            }
        }
        if matches!(
            breakdown.reconciliation(),
            UsageReconciliation::Mismatch { .. }
        ) {
            caveats.insert(UsageRollupCaveat::TotalComponentMismatch);
        }
        if !breakdown.unreadable_fields.is_empty() {
            caveats.insert(UsageRollupCaveat::UnreadableFields);
        }
        if let Some(reported) = breakdown.cost.as_ref() {
            if reported.currency == "USD" {
                match reported.as_f64() {
                    Some(amount) => {
                        cost.attempts += 1;
                        cost_amount += amount;
                    }
                    None => {
                        caveats.insert(UsageRollupCaveat::UnreadableFields);
                    }
                }
            } else {
                caveats.insert(UsageRollupCaveat::NonUsdCost);
            }
        }
    }

    coverage.tasks_with_reported_usage = tasks_with_usage.len();
    coverage.tasks_without_attestation = members
        .iter()
        .filter(|task| !attempts.keys().any(|(held, _, _)| held == *task))
        .count();

    if total_attempts > 0 {
        tokens.total_tokens = Some(UsageTotalRollup {
            value: total_value,
            attempts: total_attempts,
            source: match (saw_harness_total, saw_derived_total) {
                (true, true) => UsageRollupTotalSource::Mixed,
                (true, false) => UsageRollupTotalSource::HarnessReported,
                // No total at all cannot reach here: `total_attempts` counted
                // one, so one of the two flags is set.
                (false, _) => UsageRollupTotalSource::DerivedFromComponents,
            },
        });
    }
    if cost.attempts > 0 {
        cost.amount_usd = Some(cost_amount);
    }

    if coverage.tasks_without_attestation > 0 {
        caveats.insert(UsageRollupCaveat::MembersWithoutAttestation);
    }
    if coverage.attempts_not_reported > 0 || coverage.attempts_not_declared > 0 {
        caveats.insert(UsageRollupCaveat::AttemptsWithoutUsage);
    }
    if saw_harness_total && saw_derived_total {
        caveats.insert(UsageRollupCaveat::MixedTotalAuthority);
    }
    if cost.attempts > 0 && cost.attempts < coverage.attempts_reported {
        caveats.insert(UsageRollupCaveat::PartialCost);
    }
    if saturated {
        caveats.insert(UsageRollupCaveat::SumSaturated);
    }

    UsageRollup {
        authority: FactAuthority::AdvisoryProviderCapture,
        provenance: ROLLUP_PROVENANCE.to_owned(),
        composition: ROLLUP_COMPOSITION.to_owned(),
        coverage,
        tokens,
        cost,
        caveats: caveats.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::usage::{UsageBreakdown, UsageCost, UsageShape, UsageTotalTokens};

    fn attestation(seq: u64, task: &str, attempt: u64, usage: Value) -> AttestationRecord {
        AttestationRecord {
            observed_at: "2026-08-05T00:00:00.000Z".to_owned(),
            payload: json!({
                "kind": "adapter-scrape",
                "taskUuid": task,
                "jobId": task,
                "adapter": "codex",
                "attempt": attempt,
                "leaseEpoch": 1,
                "captures": {},
                "usage": usage,
                "usageAuthority": "advisory-only",
            }),
            seq,
            prev_hash: "sha256:prev".to_owned(),
            hash: "sha256:hash".to_owned(),
        }
    }

    /// The real codex `turn.completed` shape from `test/fixtures/usage`:
    /// components only, no harness total, cache write zero, reasoning nested.
    fn codex_usage() -> Value {
        serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Components,
            input_tokens: Some(262_086),
            input_tokens_as_reported: Some(7_060_166),
            cache_read_tokens: Some(6_798_080),
            cache_write_tokens: Some(0),
            output_tokens: Some(32_842),
            reasoning_tokens: Some(15_163),
            total_tokens: Some(UsageTotalTokens {
                value: 7_093_008,
                source: UsageTotalSource::DerivedFromComponents,
            }),
            cost: None,
            unreadable_fields: Vec::new(),
        }))
        .unwrap()
    }

    /// The real claude-code `result` shape, exactly as
    /// `usage::tests::claude_code_capture_reconciles_components_and_keeps_reported_cost`
    /// pins it against the verbatim fixture: cache write is a large fresh-input
    /// volume that `input_tokens` excludes, a cost is stated, and the total is
    /// **derived** — the `result` event's usage object carries no
    /// `total_tokens` and the `claude-code` preset declares no `totalTokens`
    /// mapping, so nothing about it is harness-reported.
    fn claude_usage() -> Value {
        serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Components,
            input_tokens: Some(83),
            input_tokens_as_reported: Some(83),
            cache_read_tokens: Some(11_093_140),
            cache_write_tokens: Some(265_127),
            output_tokens: Some(22_298),
            reasoning_tokens: None,
            total_tokens: Some(UsageTotalTokens {
                value: 11_380_648,
                source: UsageTotalSource::DerivedFromComponents,
            }),
            cost: Some(UsageCost {
                amount: serde_json::Number::from_f64(8.755_705).unwrap(),
                currency: "USD".to_owned(),
            }),
            unreadable_fields: Vec::new(),
        }))
        .unwrap()
    }

    /// An **operator-defined** adapter that declares a `totalTokens` mapping
    /// its harness fills. No shipped preset does, which is why this fixture
    /// names no harness: it exists to exercise the one path that produces
    /// `harness-reported`, and pretending a preset produces it is what the
    /// first draft of this module got wrong.
    fn declared_total_usage() -> Value {
        serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Components,
            input_tokens: Some(100),
            input_tokens_as_reported: Some(100),
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(0),
            output_tokens: Some(20),
            reasoning_tokens: None,
            total_tokens: Some(UsageTotalTokens {
                value: 120,
                source: UsageTotalSource::HarnessReported,
            }),
            cost: None,
            unreadable_fields: Vec::new(),
        }))
        .unwrap()
    }

    #[test]
    fn a_two_harness_run_sums_every_component_and_grades_both_totals_derived() {
        let records = [
            attestation(1, "task-codex", 1, codex_usage()),
            attestation(2, "task-claude", 1, claude_usage()),
        ];
        let rollup = roll_up(
            ["task-codex", "task-claude"],
            &AttestationEvidence::new(true, &records),
        );

        assert_eq!(rollup.authority, FactAuthority::AdvisoryProviderCapture);
        assert_eq!(rollup.coverage.tasks, 2);
        assert_eq!(rollup.coverage.attempts_observed, 2);
        assert_eq!(rollup.coverage.attempts_reported, 2);
        assert_eq!(rollup.coverage.tasks_with_reported_usage, 2);
        assert_eq!(rollup.coverage.tasks_without_attestation, 0);

        assert_eq!(rollup.tokens.input_tokens.value, 262_086 + 83);
        assert_eq!(
            rollup.tokens.cache_read_tokens.value,
            6_798_080 + 11_093_140
        );
        assert_eq!(rollup.tokens.cache_write_tokens.value, 265_127);
        assert_eq!(rollup.tokens.output_tokens.value, 32_842 + 22_298);
        // Reasoning is rolled up for visibility and is inside the output sum,
        // never beside it.
        assert_eq!(rollup.tokens.reasoning_tokens.value, 15_163);
        assert_eq!(rollup.tokens.reasoning_tokens.attempts, 1);

        // The defect this rollup exists to avoid: summing `inputTokens` alone
        // would report 262,169 fresh input and lose claude's entire 265,127
        // cache-write volume. Codex's own cache write is a measured zero and
        // contributes nothing, which is why the sum is the other three terms.
        assert_eq!(
            rollup.tokens.fresh_input_tokens.value,
            262_086 + 83 + 265_127
        );
        assert_ne!(
            rollup.tokens.fresh_input_tokens.value,
            rollup.tokens.input_tokens.value
        );
        assert_eq!(rollup.tokens.fresh_input_tokens.attempts_complete, 2);
        assert_eq!(rollup.tokens.fresh_input_tokens.attempts_partial, 0);

        let total = rollup.tokens.total_tokens.expect("both attempts total");
        assert_eq!(total.value, 7_093_008 + 11_380_648);
        assert_eq!(total.attempts, 2);
        // Both shipped presets derive their total, so the two-preset run is
        // not `mixed` — it is uniformly derived, and the caveat stays silent.
        assert_eq!(total.source, UsageRollupTotalSource::DerivedFromComponents);
        assert!(!rollup
            .caveats
            .contains(&UsageRollupCaveat::MixedTotalAuthority));

        // Cost is where a harness reported it: one of two attempts.
        assert_eq!(rollup.cost.attempts, 1);
        assert_eq!(rollup.cost.amount_usd, Some(8.755_705));
        assert!(rollup.caveats.contains(&UsageRollupCaveat::PartialCost));
        assert_eq!(rollup.cost.basis, ROLLUP_COST_BASIS);
        assert!(
            rollup.cost.basis.contains("floor"),
            "the charge floor caveat travels with the cost figure"
        );
    }

    #[test]
    fn only_a_declared_total_mapping_reaches_the_harness_reported_grade() {
        // Nothing a shipped preset produces can reach `harness-reported`: the
        // grade comes from a declared `totalTokens` mapping, and neither
        // preset declares one. An operator-defined adapter that does can.
        let declared = [attestation(1, "declared", 1, declared_total_usage())];
        let harness = roll_up(["declared"], &AttestationEvidence::new(true, &declared));
        assert_eq!(
            harness.tokens.total_tokens.unwrap().source,
            UsageRollupTotalSource::HarnessReported
        );
        assert!(harness.is_complete(), "{:?}", harness.caveats);

        // And a run mixing that adapter with a preset is where `mixed` — and
        // its caveat — actually becomes reachable.
        let records = [
            attestation(1, "declared", 1, declared_total_usage()),
            attestation(2, "preset", 1, codex_usage()),
        ];
        let mixed = roll_up(
            ["declared", "preset"],
            &AttestationEvidence::new(true, &records),
        );
        let total = mixed.tokens.total_tokens.expect("both attempts total");
        assert_eq!(total.value, 120 + 7_093_008);
        assert_eq!(total.source, UsageRollupTotalSource::Mixed);
        assert!(mixed
            .caveats
            .contains(&UsageRollupCaveat::MixedTotalAuthority));
    }

    #[test]
    fn an_attempt_whose_mapping_resolved_nothing_is_not_a_complete_costless_run() {
        // Harness drift: the `usage` capture resolves, no declared path is
        // present in what the harness emitted, and absence is not
        // unreadability -- so `observe` returns `Reported` with an `unmapped`
        // shape, every component `None`, and an empty `unreadableFields`.
        // Counting that as covered would grade a run whose mapping resolved
        // nothing as complete and costless.
        let unmapped = serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
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
        }))
        .unwrap();
        let rollup = roll_up(
            ["task"],
            &AttestationEvidence::new(true, &[attestation(1, "task", 1, unmapped)]),
        );
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(rollup.coverage.attempts_reported_without_figures, 1);
        assert_eq!(
            rollup.coverage.tasks_with_reported_usage, 0,
            "an attempt that contributed nothing does not make its task covered"
        );
        assert_eq!(rollup.tokens, UsageTokenRollup::default());
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::ReportedWithoutFigures));
        assert!(!rollup.is_complete());

        // The weaker same-shaped variant: a breakdown with one component and
        // no total still contributes, so it is not in this bucket.
        let reasoning_only = serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Components,
            input_tokens: None,
            input_tokens_as_reported: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            output_tokens: None,
            reasoning_tokens: Some(7),
            total_tokens: None,
            cost: None,
            unreadable_fields: Vec::new(),
        }))
        .unwrap();
        let partial = roll_up(
            ["task"],
            &AttestationEvidence::new(true, &[attestation(1, "task", 1, reasoning_only)]),
        );
        assert_eq!(partial.coverage.attempts_reported_without_figures, 0);
        assert_eq!(partial.coverage.tasks_with_reported_usage, 1);
        assert_eq!(partial.tokens.reasoning_tokens.value, 7);
        assert_eq!(partial.tokens.total_tokens, None);
    }

    #[test]
    fn every_attempt_of_a_retried_task_is_charged_once_and_only_once() {
        // The row holds only the last attempt; the ledger holds all three, and
        // a re-scrape of attempt 2 supersedes rather than double-charges.
        let records = [
            attestation(1, "task", 1, codex_usage()),
            attestation(2, "task", 2, codex_usage()),
            attestation(3, "task", 2, codex_usage()),
            attestation(4, "task", 3, codex_usage()),
        ];
        let rollup = roll_up(["task"], &AttestationEvidence::new(true, &records));
        assert_eq!(rollup.coverage.attempts_observed, 3);
        assert_eq!(rollup.coverage.attempts_reported, 3);
        assert_eq!(rollup.tokens.output_tokens.value, 32_842 * 3);
        assert_eq!(rollup.tokens.output_tokens.attempts, 3);
    }

    #[test]
    fn typed_absence_is_counted_apart_from_reported_usage_and_never_summed() {
        let records = [
            attestation(1, "reported", 1, codex_usage()),
            attestation(
                2,
                "quiet",
                1,
                serde_json::to_value(UsageObservation::NotReported).unwrap(),
            ),
            attestation(
                3,
                "silent",
                1,
                serde_json::to_value(UsageObservation::NotDeclared).unwrap(),
            ),
        ];
        let rollup = roll_up(
            ["reported", "quiet", "silent", "never-scraped"],
            &AttestationEvidence::new(true, &records),
        );
        assert_eq!(rollup.coverage.tasks, 4);
        assert_eq!(rollup.coverage.attempts_observed, 3);
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(rollup.coverage.attempts_not_reported, 1);
        assert_eq!(rollup.coverage.attempts_not_declared, 1);
        assert_eq!(rollup.coverage.tasks_without_attestation, 1);
        assert_eq!(rollup.tokens.output_tokens.attempts, 1);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::AttemptsWithoutUsage));
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::MembersWithoutAttestation));
        assert!(!rollup.is_complete());
    }

    #[test]
    fn an_attestation_that_predates_the_usage_record_reads_as_absence_not_zero() {
        let mut record = attestation(1, "task", 1, codex_usage());
        record
            .payload
            .as_object_mut()
            .expect("payload is an object")
            .remove("usage");
        let rollup = roll_up(["task"], &AttestationEvidence::new(true, &[record]));
        assert_eq!(rollup.coverage.attempts_observed, 1);
        assert_eq!(rollup.coverage.attempts_reported, 0);
        assert_eq!(rollup.coverage.attempts_without_usage_record, 1);
        assert_eq!(rollup.tokens.total_tokens, None);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::UnreadableUsageRecord));
    }

    #[test]
    fn an_unverified_ledger_sums_nothing_and_says_why() {
        let records = [attestation(1, "task", 1, codex_usage())];
        let rollup = roll_up(["task"], &AttestationEvidence::new(false, &records));
        assert!(!rollup.coverage.ledger_verified);
        assert_eq!(rollup.coverage.attempts_observed, 0);
        assert_eq!(rollup.tokens, UsageTokenRollup::default());
        assert_eq!(rollup.cost.amount_usd, None);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::LedgerUnverified));
        assert_eq!(
            roll_up(["task"], &AttestationEvidence::unavailable()).coverage,
            rollup.coverage
        );
    }

    #[test]
    fn an_attestation_for_a_task_the_run_does_not_hold_is_not_charged_to_it() {
        let records = [
            attestation(1, "mine", 1, codex_usage()),
            attestation(2, "someone-elses", 1, codex_usage()),
        ];
        let rollup = roll_up(["mine"], &AttestationEvidence::new(true, &records));
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(rollup.tokens.output_tokens.value, 32_842);
    }

    #[test]
    fn a_complete_single_harness_run_carries_no_caveats() {
        let records = [attestation(1, "task", 1, claude_usage())];
        let rollup = roll_up(["task"], &AttestationEvidence::new(true, &records));
        assert!(
            rollup.is_complete(),
            "unexpected caveats: {:?}",
            rollup.caveats
        );
        let total = rollup
            .tokens
            .total_tokens
            .expect("the components derive a total");
        assert_eq!(total.source, UsageRollupTotalSource::DerivedFromComponents);
        assert_eq!(rollup.cost.attempts, 1);
    }

    #[test]
    fn a_half_reported_fresh_input_is_a_floor_and_says_so() {
        let usage = serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Components,
            input_tokens: Some(100),
            input_tokens_as_reported: Some(100),
            cache_read_tokens: None,
            cache_write_tokens: None,
            output_tokens: Some(10),
            reasoning_tokens: None,
            total_tokens: Some(UsageTotalTokens {
                value: 110,
                source: UsageTotalSource::DerivedFromComponents,
            }),
            cost: None,
            unreadable_fields: Vec::new(),
        }))
        .unwrap();
        let rollup = roll_up(
            ["task"],
            &AttestationEvidence::new(true, &[attestation(1, "task", 1, usage)]),
        );
        assert_eq!(rollup.tokens.fresh_input_tokens.value, 100);
        assert_eq!(rollup.tokens.fresh_input_tokens.attempts_partial, 1);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialFreshInput));
    }

    #[test]
    fn a_mismatched_and_unreadable_attempt_names_both_problems() {
        // A mismatch is only representable for a declared-total adapter: with
        // no `totalTokens` mapping there is no stated total to disagree with
        // the components, which is why this fixture states one.
        let usage = serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Components,
            input_tokens: Some(10),
            input_tokens_as_reported: Some(10),
            cache_read_tokens: None,
            cache_write_tokens: Some(0),
            output_tokens: Some(5),
            reasoning_tokens: None,
            total_tokens: Some(UsageTotalTokens {
                value: 99,
                source: UsageTotalSource::HarnessReported,
            }),
            cost: None,
            unreadable_fields: vec!["costUsd".to_owned()],
        }))
        .unwrap();
        let rollup = roll_up(
            ["task"],
            &AttestationEvidence::new(true, &[attestation(1, "task", 1, usage)]),
        );
        // The attempt's own total is what is summed; neither number is fixed.
        assert_eq!(rollup.tokens.total_tokens.unwrap().value, 99);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::TotalComponentMismatch));
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::UnreadableFields));
    }

    #[test]
    fn the_rollup_round_trips_through_its_own_wire_shape() {
        let records = [
            attestation(1, "task-codex", 1, codex_usage()),
            attestation(2, "task-claude", 1, claude_usage()),
        ];
        let rollup = roll_up(
            ["task-codex", "task-claude"],
            &AttestationEvidence::new(true, &records),
        );
        let encoded = serde_json::to_string(&rollup).unwrap();
        let decoded: UsageRollup = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, rollup);
        let value: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["authority"], json!("advisory-provider-capture"));
        // The composition statement is on the wire, not only in the doc: a
        // consumer must not have to guess which token fields the total is over.
        assert!(value["composition"]
            .as_str()
            .unwrap()
            .contains("freshInputTokens = inputTokens + cacheWriteTokens"));
    }
}
