//! Rolling per-attempt usage up to one flow run.
//!
//! The rollup is a derived projection. Durable rows, witness records, and
//! attestation observations remain its canonical/advisory inputs; no aggregate
//! here is written back as a second source of truth.
//!
//! [`crate::usage`] normalizes what one attempt's harness reported.  This
//! module answers the next question — "what did this run cost" — and the whole
//! of its difficulty is that the answer is a sum over evidence that is
//! advisory, partial, and shaped differently per harness. A sum that hides any
//! of those three is worse than no sum at all: it reads as a measurement, it is
//! wrong in the reassuring direction, and nothing about its shape says so.
//! Four rules follow, and every field below exists to keep one of them.
//!
//! **1. Derive attempts independently, then read their accounting from the
//! attestation ledger.** A durable row's attempt counter defines the expected
//! `1..=N` roster; when a member has no row detail, the latest canonical
//! witness attempt is the fallback. The advisory ledger can satisfy that
//! roster but cannot enlarge it. Every completed scrape, including both typed
//! absences, has a durable `usageEvidence` seat keyed by
//! `taskUuid`/`attempt`/`leaseEpoch`; duplicate leases select the last record
//! and contribute once. The rollup sums exact per-attempt
//! `accounting.usage`, never a raw cumulative observation. Missing,
//! over-ceiling, and unknown-ceiling evidence is caveated. Pre-schema raw-only
//! records are visible on job detail but are excluded here rather than guessed
//! fresh. Their **reported-shape** remains diagnostic only: because the
//! declared surface is unknown, it can explain an ambiguous total-only record
//! but can never become a completeness denominator.
//!
//! **2. `inputTokens` alone is not the cross-harness fresh-input figure.**
//! claude-code's `cache_creation_input_tokens` are fresh, uncached prompt
//! tokens its `input_tokens` *excludes*; codex has no cache-write volume in any
//! observed capture. A rollup that summed `inputTokens` alone would understate
//! claude by its entire cache-write volume — in this project's own #381 fixture
//! that is 23,000 of 65,312 tokens, more than a third — while printing a number
//! that looks directly comparable to codex's. The comparable figure is
//! the adapter's declared input convention plus `cacheWriteTokens` when that
//! field was declared, surfaced here as
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
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapters::UsageCounterScope;
use crate::query_v2::FactAuthority;
use crate::usage::{
    account_fresh, breakdown_reports_field, unavailable_delta, UsageAccounting,
    UsageAccountingBasis, UsageAccountingReason, UsageAccountingState, UsageEvidence,
    UsageObservation, UsagePredecessor, UsageReconciliation, UsageTotalSource,
    FIELD_CACHE_READ_TOKENS, FIELD_CACHE_WRITE_TOKENS, FIELD_COST_USD, FIELD_INPUT_TOKENS,
    FIELD_INPUT_TOKENS_WITH_CACHE_READ, FIELD_OUTPUT_TOKENS, FIELD_REASONING_TOKENS,
    FIELD_TOTAL_TOKENS, USAGE_EVIDENCE_SCHEMA_VERSION, USAGE_FIELDS,
};
use crate::witness::AttestationRecord;

/// Payload kind the exit recorder writes one of per scraped attempt.
const ADAPTER_SCRAPE_KIND: &str = "adapter-scrape";

/// Maximum number of missing expected-attempt identities published beside the
/// complete count. A run can carry an arbitrarily large durable attempt
/// counter; diagnostics must not make its query response arbitrarily large.
pub const MAX_MISSING_ATTEMPT_IDENTITIES: usize = 64;

/// Where the rollup's numbers came from.
pub const ROLLUP_PROVENANCE: &str =
    "expected attempts from durable row counters with canonical-witness fallback; last adapter-scrape usageEvidence.accounting per taskUuid/attempt, selected leaseEpoch retained; legacy raw observations excluded, with reported-shape ambiguity retained only as declared-surface-unknown diagnosis";

/// Exactly what the run total is a sum over. Stated on the wire because the
/// defect this rollup exists to avoid is a figure computed from the wrong one
/// of several similarly named token fields.
pub const ROLLUP_COMPOSITION: &str = concat!(
    "totalTokens sums each attempt's declared harness total, or its exact declared ",
    "token components where the harness stated no total of its own; ",
    "freshInputTokens = inputTokens + cacheWriteTokens where cacheWriteTokens was ",
    "declared, which is the cross-harness uncached-prompt figure that inputTokens ",
    "alone understates for a cache-writing adapter; reasoningTokens is ",
    "nested inside outputTokens and is never added to any total; each harness's own ",
    "inputTokensAsReported is not summed, because the two conventions are not ",
    "commensurable"
);

/// What cost here is and is not.
pub const ROLLUP_COST_BASIS: &str = concat!(
    "harness-reported costUsd only, summed over attempts that declared and exactly reported it. ",
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

/// One task whose independently known attempt history belongs in a rollup.
///
/// The ceiling comes from durable row state, or from the canonical witness
/// chain when no row detail exists. It never comes from the advisory
/// attestation ledger being measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedUsageTask {
    task_uuid: String,
    attempt_ceiling: Option<NonZeroU32>,
}

impl ExpectedUsageTask {
    #[must_use]
    pub fn known(task_uuid: impl Into<String>, attempt_ceiling: NonZeroU32) -> Self {
        Self {
            task_uuid: task_uuid.into(),
            attempt_ceiling: Some(attempt_ceiling),
        }
    }

    #[must_use]
    pub fn unknown(task_uuid: impl Into<String>) -> Self {
        Self {
            task_uuid: task_uuid.into(),
            attempt_ceiling: None,
        }
    }
}

/// The independent denominator for one run's usage rollup.
///
/// [`roll_up`] accepts this type rather than task strings so a caller cannot
/// accidentally derive both membership and attempt count from the attestation
/// ledger. Duplicate task entries retain the highest independently known
/// ceiling; a known ceiling always wins over an unknown one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExpectedUsageRoster {
    tasks: BTreeMap<String, Option<NonZeroU32>>,
}

impl ExpectedUsageRoster {
    #[must_use]
    pub fn new(tasks: impl IntoIterator<Item = ExpectedUsageTask>) -> Self {
        let mut roster = Self::default();
        for task in tasks {
            roster
                .tasks
                .entry(task.task_uuid)
                .and_modify(|ceiling| {
                    *ceiling = match (*ceiling, task.attempt_ceiling) {
                        (Some(left), Some(right)) => Some(left.max(right)),
                        (known @ Some(_), None) | (None, known @ Some(_)) => known,
                        (None, None) => None,
                    };
                })
                .or_insert(task.attempt_ceiling);
        }
        roster
    }
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
/// Compare `attempts` with this field's declaration-aware entry in
/// [`UsageCoverage::field_coverage`]. A lower count means the sum is over a
/// strict subset of the attempts that promised the field and raises
/// [`UsageRollupCaveat::PartialComponents`]. Attempts from adapters that did
/// not declare the field are intentionally outside that denominator.
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
/// A declared cache-write field participates in the formula and makes a
/// missing half a floor. An adapter that declares only an input convention has
/// a complete one-field formula rather than a permanent partial caveat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageFreshInput {
    pub value: u64,
    /// Attempts that exactly reported every field in their declared formula.
    pub attempts_complete: usize,
    /// Attempts that reported some, but not every, field in their declared
    /// formula. The missing share contributed nothing, so this attempt's value
    /// is a floor. An attempt that reported none of its formula contributes no
    /// fresh-input figure and is diagnosed by declared-field coverage instead.
    pub attempts_partial: usize,
}

/// How many selected attempts declared and exactly supplied one logical field.
///
/// `attemptsDeclared` is the denominator. `attemptsReported` counts exact
/// per-attempt values in `usageEvidence.accounting.usage`;
/// `attemptsUnreadable` describes the raw provider value, while
/// `attemptsAccountingUnavailable` describes a value tally could not reduce
/// to an exact fresh/delta amount. An absent declared key increments only the
/// denominator, so the four counts distinguish absence from both failure
/// modes without treating an undeclared field as missing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageFieldCoverage {
    pub attempts_declared: usize,
    pub attempts_reported: usize,
    pub attempts_unreadable: usize,
    pub attempts_accounting_unavailable: usize,
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
    /// The declared input convention plus cache-write tokens where the adapter
    /// declared that field: the cross-harness fresh-prompt volume.
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageCoverage {
    /// Tasks the run durably holds.
    pub tasks: usize,
    /// Member tasks with at least one attempt that contributed a figure to
    /// these sums. An attempt that reported usage the declared mapping could
    /// read nothing out of does not make its task one of these — see
    /// [`UsageCoverage::attempts_reported_without_figures`].
    pub tasks_with_reported_usage: usize,
    /// Member tasks the ledger holds no attempt for at all. Every successfully
    /// completed scrape writes an attestation even for empty captures, but an
    /// append can fail and old attestations can age out of retention. Either
    /// way these tasks are invisible to the sums and are counted rather than
    /// dropped.
    pub tasks_without_attestation: usize,
    /// Expected logical attempts from independently known task ceilings.
    /// Tasks whose ceiling is unavailable contribute zero here and are counted
    /// separately in [`Self::tasks_with_unknown_attempt_ceiling`].
    pub attempts_expected: usize,
    /// Distinct logical `(task, attempt)` identities with a selected
    /// attestation. For a known ceiling this includes only `1..=ceiling`; for
    /// an unknown ceiling, readable positive attempt numbers remain visible
    /// while the unknown-ceiling caveat prevents a completeness claim.
    pub attempts_attested: usize,
    /// Expected attempts with no selected attestation.
    ///
    /// The public name says which evidence is absent. `attemptsMissing` was
    /// emitted briefly during development and remains an input alias only.
    #[serde(rename = "attemptsMissingAttestation", alias = "attemptsMissing")]
    pub attempts_missing: usize,
    /// The first bounded set of missing identities, ordered by task UUID then
    /// attempt. Compare its length with `attemptsMissingAttestation` to detect
    /// truncation.
    pub missing_attempts: Vec<UsageAttemptIdentity>,
    /// Member tasks for which neither durable row detail nor the independent
    /// canonical witness chain supplied an attempt ceiling.
    pub tasks_with_unknown_attempt_ceiling: usize,
    /// Expected attempts that appeared under more than one distinct lease.
    /// They still count and contribute once, using the last verified ledger
    /// record for the logical attempt.
    pub attempts_with_duplicate_leases: usize,
    /// Distinct member `(task, attempt)` identities that cannot belong to the
    /// independent roster, including positive attempts above a known ceiling.
    pub attempts_unexpected: usize,
    /// Distinct `(task, attempt, leaseEpoch)` triples found for member tasks.
    ///
    /// This is the pre-#402 physical-observation counter retained for wire
    /// compatibility. It is not a completeness denominator; use
    /// `attemptsExpected`, `attemptsAttested`, and
    /// `attemptsMissingAttestation`.
    pub attempts_observed: usize,
    /// Attempts whose attestation carries a `reported` usage record.
    pub attempts_reported: usize,
    /// Declaration-aware attempt counts for every logical usage field known to
    /// this schema. These, not reported shape, are the completeness
    /// denominators. A zero `attemptsDeclared` means the field was outside the
    /// adapters' promised surface and therefore cannot make the rollup
    /// partial.
    #[serde(default)]
    pub field_coverage: BTreeMap<String, UsageFieldCoverage>,
    /// Sparse public projection of declaration denominators. Only fields with
    /// at least one declaration are present.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub declared_by_field: BTreeMap<String, usize>,
    /// Sparse public projection of exact reports for the same keys as
    /// [`Self::declared_by_field`]. A declared-but-missing field is present
    /// with value zero.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reported_by_field: BTreeMap<String, usize>,
    /// Declared fields for which at least one selected attempt supplied no
    /// exact value. Ordered by the logical usage schema so a public consumer
    /// can name drift directly without comparing the two census maps.
    #[serde(default)]
    pub missing_declared_fields: Vec<String>,
    /// The subset of [`UsageCoverage::attempts_reported`] that contributed
    /// nothing: the harness reported usage and no figure this rollup sums
    /// survived the adapter's declared mapping.
    ///
    /// This is **total** drift — every declared path resolved to absent, which
    /// is not the same as unreadable, so nothing lands in `unreadableFields`
    /// and [`crate::usage::observe`] still returns `Reported`, with
    /// [`crate::usage::UsageShape::Unmapped`]. Counting that attempt as covered
    /// would let a run whose mapping resolved nothing read as complete and
    /// costless, so it is counted here and raises
    /// [`UsageRollupCaveat::ReportedWithoutFigures`] instead.
    ///
    /// Drift in **one** key is at least as ordinary, and it does not land here:
    /// that attempt did contribute, just not everything it was supposed to.
    /// [`UsageRollupCaveat::PartialComponents`] owns that case, and it is the
    /// one that catches a single renamed key silently deleting a component from
    /// the run total.
    pub attempts_reported_without_figures: usize,
    /// Deprecated wire-compatibility projection: reported attempts whose
    /// durable evidence declared at least one token component. It is computed
    /// only from declarations and never drives completeness or caveats; use
    /// `fieldCoverage` and each field's
    /// `attemptsDeclared`/`attemptsReported` counts.
    pub attempts_reported_with_components: usize,
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
    /// Attempts carrying the pre-schema raw-only contract. Their observation
    /// remains visible on an individual job, but is not a per-attempt charge
    /// and contributes nothing to confident sums.
    pub attempts_legacy_usage: usize,
    /// Attempts whose declared values could not all be reduced to exact
    /// per-attempt accounting. Exact fields inside a partial record still
    /// contribute; the count and caveat keep that floor from reading as a
    /// complete bill.
    pub attempts_accounting_unavailable: usize,
    /// Whether the advisory attestation chain verified. When false nothing was
    /// summed.
    pub ledger_verified: bool,
}

/// One expected attempt for which no advisory scrape attestation was found.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UsageAttemptIdentity {
    pub task_uuid: String,
    pub attempt: u32,
}

/// Named reasons a reader must not treat these sums as a complete bill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageRollupCaveat {
    /// The advisory attestation chain did not verify. Nothing was summed.
    LedgerUnverified,
    /// One or more independently expected attempts has no selected scrape
    /// attestation.
    AttemptsMissingAttestation,
    /// At least one member task has no independent durable or canonical
    /// attempt ceiling, so its denominator is unknown.
    AttemptCounterUnavailable,
    /// An expected logical attempt appeared under multiple lease epochs. The
    /// last verified ledger record was selected and the attempt was summed
    /// once.
    DuplicateAttemptLeases,
    /// An attestation names an attempt outside the independent roster. It is
    /// retained as a caveat but cannot enlarge the denominator or the sums.
    UnexpectedAttestation,
    /// Some member task has no attestation at all, so its usage — if any —
    /// is not in these sums.
    MembersWithoutAttestation,
    /// Some observed attempt reported no usage, by either typed absence.
    AttemptsWithoutUsage,
    /// Some attempt reported usage that yielded no figure at all — the
    /// adapter's declared mapping resolved nothing out of what the harness
    /// emitted. The sums are missing that attempt entirely.
    ReportedWithoutFigures,
    /// A declared logical token field was exactly reported by fewer attempts
    /// than declared it. Undeclared fields do not participate: a cache-less
    /// adapter declaring only input and output is complete when those arrive.
    ///
    /// This is what a single renamed harness key looks like. Every other
    /// declared path still resolves, so the attempt contributes and is not in
    /// `attemptsReportedWithoutFigures`; the one component silently drops out
    /// of the total and, on a real claude-code capture, takes 97% of the run's
    /// tokens with it. Both input conventions, cache read/write, output, and
    /// reasoning are each checked only where declared. `reasoningTokens`
    /// therefore participates for Codex but not for claude-code. Total-only
    /// and cost-only adapters declared no token fields and are not compared
    /// against this threshold at all.
    PartialComponents,
    /// A declared harness `totalTokens` mapping was exactly reported by fewer
    /// attempts than declared it. A derived total may remain visible as a
    /// floor, but it is not evidence that the promised total arrived.
    PartialTotal,
    /// Some attestation carries no readable usage record.
    UnreadableUsageRecord,
    /// Some attestation predates `usageEvidence`; its raw observation is not
    /// assumed fresh and is excluded from the sums.
    LegacyUsageContract,
    /// A legacy usage record has no durable declaration, so its reported
    /// shape cannot establish which fields the adapter promised.
    DeclaredSurfaceUnknown,
    /// At least one legacy reported-shape is an ambiguous total-only record
    /// beside an observation that reported components. This diagnoses the
    /// legacy ambiguity only and never exempts declared-field grading.
    TotalOnlyAttempts,
    /// Some declared field could not be reduced to an exact per-attempt value.
    AccountingUnavailable,
    /// A session-cumulative checkpoint could not be reduced because its exact
    /// predecessor baseline was absent or was a legacy record.
    CumulativeBaselineMissing,
    /// An attempt named a declared field its harness emitted in an unusable
    /// shape, so that field's sum is missing that attempt's share.
    UnreadableFields,
    /// The total mixes harness-stated totals with tally-derived ones.
    MixedTotalAuthority,
    /// An attempt stated a total its own components do not sum to. Neither
    /// number was corrected; the attempt's own total is what was summed.
    TotalComponentMismatch,
    /// An attempt reported some, but not all, of its declared fresh-input
    /// formula. Cache write participates only where it was declared; an
    /// input-only adapter therefore has a complete fresh-input value rather
    /// than a permanent caveat. An entirely absent formula is already named
    /// by `partial-components` and `missingDeclaredFields`.
    PartialFreshInput,
    /// Cost was exactly reported by fewer attempts than declared `costUsd`.
    /// Attempts from adapters that never declared cost are outside this
    /// denominator.
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
    /// Public completeness projection. This is serialized rather than making
    /// every wire consumer reconstruct the caveat policy.
    #[serde(default)]
    pub is_complete: bool,
}

impl UsageRollup {
    /// Whether these sums cover every independently expected attempt.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.caveats.is_empty()
    }
}

/// One attestation's usage, as the ledger holds it.
enum LedgerUsage {
    Accounted(Box<UsageEvidence>),
    /// A pre-schema raw observation, or a schema record with no usable field
    /// declaration. It stays visible on job detail but has no trustworthy
    /// declaration/accounting meaning for a run sum.
    Legacy(Box<UsageObservation>),
    /// The payload predates the usage record, or carries one this build cannot
    /// read.
    NoRecord,
}

/// The last verified ledger record selected for one logical attempt. Keeping
/// the lease beside the payload preserves which physical execution supplied
/// the accounted figures even though duplicate leases contribute only once.
struct SelectedLedgerUsage {
    sequence: u64,
    lease_epoch: Option<u64>,
    usage: LedgerUsage,
}

const COMPONENT_FIELDS: [&str; 6] = [
    FIELD_INPUT_TOKENS,
    FIELD_INPUT_TOKENS_WITH_CACHE_READ,
    FIELD_CACHE_READ_TOKENS,
    FIELD_CACHE_WRITE_TOKENS,
    FIELD_OUTPUT_TOKENS,
    FIELD_REASONING_TOKENS,
];

fn gradeable_declaration(evidence: &UsageEvidence) -> bool {
    let fields = evidence
        .declared_fields
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let canonical = fields.len() == evidence.declared_fields.len()
        && fields.iter().all(|field| USAGE_FIELDS.contains(field));
    let normalizable_input = !fields.contains(FIELD_INPUT_TOKENS_WITH_CACHE_READ)
        || fields.contains(FIELD_CACHE_READ_TOKENS);
    canonical
        && normalizable_input
        && if matches!(evidence.accounting.usage, UsageObservation::NotDeclared) {
            fields.is_empty()
        } else {
            !fields.is_empty()
        }
}

fn field_is_partial(coverage: &UsageCoverage, field: &str) -> bool {
    coverage
        .field_coverage
        .get(field)
        .is_some_and(|field| field.attempts_reported < field.attempts_declared)
}

fn readable_legacy_usage(payload: &Value) -> Option<UsageObservation> {
    payload
        .get("usage")
        .and_then(|value| serde_json::from_value::<UsageObservation>(value.clone()).ok())
}

fn reports_component_shape(observation: &UsageObservation) -> bool {
    observation.breakdown().is_some_and(|breakdown| {
        COMPONENT_FIELDS
            .iter()
            .any(|field| breakdown_reports_field(breakdown, field))
    })
}

fn reports_legacy_total_only_shape(observation: &UsageObservation) -> bool {
    observation.breakdown().is_some_and(|breakdown| {
        !reports_component_shape(observation)
            && breakdown
                .total_tokens
                .is_some_and(|total| total.source == UsageTotalSource::HarnessReported)
    })
}

/// Lineage carried by the public checkpoint projection. It is deliberately
/// checked against the payload adapter rather than used as an adapter-name
/// heuristic: the declaration and derivation remain the accounting contract.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PublicUsageLineage {
    adapter: String,
    session_ref: String,
}

/// An exact public-ledger reference to the cumulative checkpoint used as a
/// baseline. Sequence and hash bind the logical attempt identity to one
/// verified ledger record instead of merely naming an attempt that may have
/// been re-scraped.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PublicUsagePredecessor {
    task_uuid: String,
    attempt: u32,
    lease_epoch: u64,
    sequence: u64,
    hash: String,
}

fn public_usage_lineage(
    payload: &Value,
    evidence: &serde_json::Map<String, Value>,
) -> Option<PublicUsageLineage> {
    let lineage =
        serde_json::from_value::<PublicUsageLineage>(evidence.get("lineage")?.clone()).ok()?;
    (payload.get("adapter").and_then(Value::as_str) == Some(lineage.adapter.as_str())
        && !lineage.session_ref.is_empty())
    .then_some(lineage)
}

fn public_predecessor_is_bound(
    current: &AttestationRecord,
    predecessor: &PublicUsagePredecessor,
    lineage: &PublicUsageLineage,
    declared_fields: &[String],
    counter_scope: UsageCounterScope,
    records_by_sequence: &BTreeMap<u64, &AttestationRecord>,
) -> bool {
    if predecessor.sequence >= current.seq {
        return false;
    }
    let Some(previous) = records_by_sequence.get(&predecessor.sequence) else {
        return false;
    };
    if previous.hash != predecessor.hash {
        return false;
    }
    let payload = &previous.payload;
    if payload.get("taskUuid").and_then(Value::as_str) != Some(predecessor.task_uuid.as_str())
        || payload.get("attempt").and_then(Value::as_u64) != Some(u64::from(predecessor.attempt))
        || payload.get("leaseEpoch").and_then(Value::as_u64) != Some(predecessor.lease_epoch)
        || payload.get("adapter").and_then(Value::as_str) != Some(lineage.adapter.as_str())
    {
        return false;
    }

    let Some(evidence) = payload.get("usageEvidence").and_then(Value::as_object) else {
        return false;
    };
    let previous_declarations = evidence
        .get("declaredFields")
        .and_then(|value| serde_json::from_value::<Vec<String>>(value.clone()).ok());
    let previous_scope = evidence
        .get("counterScope")
        .and_then(|value| serde_json::from_value::<UsageCounterScope>(value.clone()).ok());
    let previous_lineage = public_usage_lineage(payload, evidence);
    let has_exact_contribution = matches!(
        evidence.get("derivation").and_then(Value::as_str),
        Some("fresh-zero" | "delta")
    ) && evidence
        .get("contribution")
        .is_some_and(|value| serde_json::from_value::<UsageObservation>(value.clone()).is_ok());

    previous_declarations.as_deref() == Some(declared_fields)
        && previous_scope == Some(counter_scope)
        && previous_lineage.as_ref() == Some(lineage)
        && has_exact_contribution
}

/// Decode the durable schema-1 object and its public projections. Those keep
/// independently produced attestations usable at the rollup boundary without
/// weakening cumulative accounting: declarations and an explicit accounting
/// result are still required, and a public delta's sequence/hash predecessor
/// must resolve inside the verified ledger.
fn accounted_usage(
    record: &AttestationRecord,
    records_by_sequence: &BTreeMap<u64, &AttestationRecord>,
) -> Option<UsageEvidence> {
    let payload = &record.payload;
    if let Some(value) = payload.get("usageEvidence") {
        if let Ok(evidence) = serde_json::from_value::<UsageEvidence>(value.clone()) {
            if evidence.schema_version == USAGE_EVIDENCE_SCHEMA_VERSION
                && gradeable_declaration(&evidence)
            {
                return Some(evidence);
            }
        }
    }

    let nested = payload.get("usageEvidence").and_then(Value::as_object);
    let schema_version = nested
        .and_then(|evidence| evidence.get("schemaVersion"))
        .and_then(Value::as_u64)
        .map(u32::try_from)
        .transpose()
        .ok()?
        .unwrap_or(USAGE_EVIDENCE_SCHEMA_VERSION);
    if schema_version != USAGE_EVIDENCE_SCHEMA_VERSION {
        return None;
    }

    let declarations = nested
        .and_then(|evidence| evidence.get("declaredFields"))
        .or_else(|| payload.get("usageDeclaredFields"))
        .or_else(|| payload.get("declaredUsageFields"))
        .or_else(|| payload.get("declaredFields"))?;
    let declared_fields = serde_json::from_value::<Vec<String>>(declarations.clone()).ok()?;
    let observed = nested
        .and_then(|evidence| evidence.get("observed"))
        .or_else(|| payload.get("usage"))
        .and_then(|value| serde_json::from_value::<UsageObservation>(value.clone()).ok())?;
    let counter_scope = nested
        .and_then(|evidence| evidence.get("counterScope"))
        .or_else(|| payload.get("usageCounterScope"))
        .map_or(Some(UsageCounterScope::Attempt), |value| {
            serde_json::from_value::<UsageCounterScope>(value.clone()).ok()
        })?;

    let accounting = if let Some(derivation) = nested
        .and_then(|evidence| evidence.get("derivation"))
        .and_then(Value::as_str)
    {
        let evidence = nested?;
        match derivation {
            "attempt" if counter_scope == UsageCounterScope::Attempt => {
                let contribution = evidence.get("contribution").and_then(|value| {
                    serde_json::from_value::<UsageObservation>(value.clone()).ok()
                })?;
                account_fresh(&declared_fields, &contribution)
            }
            "fresh-zero" if counter_scope == UsageCounterScope::SessionCumulative => {
                public_usage_lineage(payload, evidence)?;
                if evidence.get("predecessor").is_some() {
                    return None;
                }
                let contribution = evidence.get("contribution").and_then(|value| {
                    serde_json::from_value::<UsageObservation>(value.clone()).ok()
                })?;
                account_fresh(&declared_fields, &contribution)
            }
            "delta" if counter_scope == UsageCounterScope::SessionCumulative => {
                let lineage = public_usage_lineage(payload, evidence)?;
                let predecessor = evidence.get("predecessor").and_then(|value| {
                    serde_json::from_value::<PublicUsagePredecessor>(value.clone()).ok()
                })?;
                if !public_predecessor_is_bound(
                    record,
                    &predecessor,
                    &lineage,
                    &declared_fields,
                    counter_scope,
                    records_by_sequence,
                ) {
                    return None;
                }
                let contribution = evidence.get("contribution").and_then(|value| {
                    serde_json::from_value::<UsageObservation>(value.clone()).ok()
                })?;
                let mut accounting = account_fresh(&declared_fields, &contribution);
                accounting.basis = UsageAccountingBasis::Delta;
                accounting.predecessor = Some(UsagePredecessor::new(
                    predecessor.task_uuid,
                    predecessor.attempt,
                    predecessor.lease_epoch,
                ));
                accounting
            }
            "baseline-missing" if counter_scope == UsageCounterScope::SessionCumulative => {
                public_usage_lineage(payload, evidence)?;
                if evidence.get("predecessor").is_some() || evidence.get("contribution").is_some() {
                    return None;
                }
                unavailable_delta(
                    &observed,
                    None,
                    &declared_fields,
                    UsageAccountingReason::MissingPredecessor,
                )
            }
            _ => return None,
        }
    } else {
        nested
            .and_then(|evidence| evidence.get("accounting"))
            .or_else(|| payload.get("usageAccounting"))
            .and_then(|value| serde_json::from_value::<UsageAccounting>(value.clone()).ok())
            .or_else(|| {
                let basis = nested
                    .and_then(|evidence| evidence.get("basis"))
                    .or_else(|| payload.get("usageAccountingBasis"))
                    .and_then(Value::as_str);
                (counter_scope == UsageCounterScope::Attempt
                    || matches!(basis, Some("fresh" | "zero-baseline")))
                .then(|| account_fresh(&declared_fields, &observed))
            })?
    };

    let evidence = UsageEvidence {
        schema_version,
        declared_fields,
        counter_scope,
        observed,
        accounting,
    };
    gradeable_declaration(&evidence).then_some(evidence)
}

/// Sum usage against an independently derived expected-attempt roster.
///
/// Attestations can satisfy that roster, but can neither create its tasks nor
/// increase a known attempt ceiling. Records for any other task are ignored:
/// a rollup is a run's answer, not the daemon's.
#[must_use]
pub fn roll_up(roster: &ExpectedUsageRoster, evidence: &AttestationEvidence<'_>) -> UsageRollup {
    let mut coverage = UsageCoverage {
        tasks: roster.tasks.len(),
        attempts_expected: roster
            .tasks
            .values()
            .filter_map(|ceiling| *ceiling)
            .fold(0_usize, |total, ceiling| {
                total.saturating_add(ceiling.get() as usize)
            }),
        tasks_with_unknown_attempt_ceiling: roster
            .tasks
            .values()
            .filter(|ceiling| ceiling.is_none())
            .count(),
        ledger_verified: evidence.verified,
        field_coverage: USAGE_FIELDS
            .into_iter()
            .map(|field| (field.to_owned(), UsageFieldCoverage::default()))
            .collect(),
        ..UsageCoverage::default()
    };
    let mut caveats = BTreeSet::new();
    if !evidence.verified {
        caveats.insert(UsageRollupCaveat::LedgerUnverified);
    }
    if coverage.tasks_with_unknown_attempt_ceiling > 0 {
        caveats.insert(UsageRollupCaveat::AttemptCounterUnavailable);
    }

    // One selected entry per logical attempt, highest sequence winning. A
    // re-scrape or second lease supersedes the earlier record rather than
    // being charged twice. Physical triples remain counted separately for the
    // compatibility `attemptsObserved` projection.
    let mut attempts: BTreeMap<(String, u32), SelectedLedgerUsage> = BTreeMap::new();
    let mut physical_attempts = BTreeSet::new();
    let mut expected_attempt_leases: BTreeMap<(String, u32), BTreeSet<Option<u64>>> =
        BTreeMap::new();
    let mut unexpected_attempts = BTreeSet::new();
    if evidence.verified {
        let records_by_sequence = evidence
            .records
            .iter()
            .map(|record| (record.seq, record))
            .collect::<BTreeMap<_, _>>();
        for record in evidence.records {
            let payload = &record.payload;
            if payload.get("kind").and_then(Value::as_str) != Some(ADAPTER_SCRAPE_KIND) {
                continue;
            }
            let Some(task) = payload.get("taskUuid").and_then(Value::as_str) else {
                continue;
            };
            let Some(ceiling) = roster.tasks.get(task) else {
                continue;
            };
            let raw_attempt = payload.get("attempt").and_then(Value::as_u64);
            let lease_epoch = payload.get("leaseEpoch").and_then(Value::as_u64);
            physical_attempts.insert((task.to_owned(), raw_attempt, lease_epoch));
            let Some(attempt) = raw_attempt.and_then(|attempt| u32::try_from(attempt).ok()) else {
                unexpected_attempts.insert((task.to_owned(), raw_attempt));
                continue;
            };
            if attempt == 0 || ceiling.is_some_and(|ceiling| attempt > ceiling.get()) {
                unexpected_attempts.insert((task.to_owned(), raw_attempt));
                continue;
            }
            let key = (task.to_owned(), attempt);
            if ceiling.is_some() {
                expected_attempt_leases
                    .entry(key.clone())
                    .or_default()
                    .insert(lease_epoch);
            }
            let usage = accounted_usage(record, &records_by_sequence)
                .map(Box::new)
                .map_or_else(
                    || {
                        if let Some(observed) = readable_legacy_usage(payload) {
                            LedgerUsage::Legacy(Box::new(observed))
                        } else {
                            LedgerUsage::NoRecord
                        }
                    },
                    LedgerUsage::Accounted,
                );
            let selected = SelectedLedgerUsage {
                sequence: record.seq,
                lease_epoch,
                usage,
            };
            match attempts.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(selected);
                }
                std::collections::btree_map::Entry::Occupied(mut entry)
                    if record.seq >= entry.get().sequence =>
                {
                    entry.insert(selected);
                }
                std::collections::btree_map::Entry::Occupied(_) => {}
            }
        }
    }
    coverage.attempts_observed = physical_attempts.len();
    coverage.attempts_attested = attempts.len();
    coverage.attempts_unexpected = unexpected_attempts.len();
    coverage.attempts_with_duplicate_leases = expected_attempt_leases
        .values()
        .filter(|leases| leases.len() > 1)
        .count();

    for (task, ceiling) in &roster.tasks {
        let Some(ceiling) = ceiling else {
            continue;
        };
        let attested = attempts
            .range((task.clone(), 1)..=(task.clone(), ceiling.get()))
            .count();
        coverage.attempts_missing = coverage
            .attempts_missing
            .saturating_add(ceiling.get() as usize - attested);
        if coverage.missing_attempts.len() == MAX_MISSING_ATTEMPT_IDENTITIES {
            continue;
        }
        for attempt in 1..=ceiling.get() {
            if coverage.missing_attempts.len() == MAX_MISSING_ATTEMPT_IDENTITIES {
                break;
            }
            if !attempts.contains_key(&(task.clone(), attempt)) {
                coverage.missing_attempts.push(UsageAttemptIdentity {
                    task_uuid: task.clone(),
                    attempt,
                });
            }
        }
    }
    if coverage.attempts_missing > 0 {
        caveats.insert(UsageRollupCaveat::AttemptsMissingAttestation);
    }
    if coverage.attempts_with_duplicate_leases > 0 {
        caveats.insert(UsageRollupCaveat::DuplicateAttemptLeases);
    }
    if coverage.attempts_unexpected > 0 {
        caveats.insert(UsageRollupCaveat::UnexpectedAttestation);
    }

    let mut tokens = UsageTokenRollup::default();
    let mut cost = UsageCostRollup::default();
    let mut cost_amount = 0.0_f64;
    let mut tasks_with_usage = BTreeSet::new();
    let mut total_value = 0_u64;
    let mut total_attempts = 0_usize;
    let mut saw_harness_total = false;
    let mut saw_derived_total = false;
    let mut legacy_total_only_shapes = 0_usize;
    let mut reported_component_shapes = 0_usize;
    let mut saturated = false;

    for ((task, _), selected) in &attempts {
        // The selected lease is deliberately retained beside the record as
        // attempt provenance, even though the aggregate has no per-attempt
        // evidence array to publish it in.
        let _selected_lease_epoch = selected.lease_epoch;
        let usage_evidence = match &selected.usage {
            LedgerUsage::NoRecord => {
                coverage.attempts_without_usage_record += 1;
                caveats.insert(UsageRollupCaveat::UnreadableUsageRecord);
                continue;
            }
            LedgerUsage::Legacy(observed) => {
                coverage.attempts_legacy_usage += 1;
                caveats.insert(UsageRollupCaveat::LegacyUsageContract);
                caveats.insert(UsageRollupCaveat::DeclaredSurfaceUnknown);
                if reports_legacy_total_only_shape(observed) {
                    legacy_total_only_shapes += 1;
                }
                if reports_component_shape(observed) {
                    reported_component_shapes += 1;
                }
                continue;
            }
            LedgerUsage::Accounted(evidence) => evidence,
        };
        if usage_evidence.accounting.state != UsageAccountingState::Exact {
            coverage.attempts_accounting_unavailable += 1;
            caveats.insert(UsageRollupCaveat::AccountingUnavailable);
        }
        if usage_evidence.accounting.basis == UsageAccountingBasis::Delta
            && matches!(
                usage_evidence.accounting.reason,
                Some(
                    UsageAccountingReason::MissingPredecessor
                        | UsageAccountingReason::LegacyPredecessor
                )
            )
        {
            caveats.insert(UsageRollupCaveat::CumulativeBaselineMissing);
        }

        let declarations = usage_evidence
            .declared_fields
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let raw_unreadable = usage_evidence
            .observed
            .breakdown()
            .map_or(&[][..], |breakdown| breakdown.unreadable_fields.as_slice());
        let accounting_unavailable = &usage_evidence.accounting.unavailable_fields;
        let accounting_breakdown = usage_evidence.accounting.usage.breakdown();
        for field in &declarations {
            let stats = coverage
                .field_coverage
                .get_mut(*field)
                .expect("gradeable declarations contain only known usage fields");
            stats.attempts_declared += 1;
            if raw_unreadable.iter().any(|unreadable| unreadable == field) {
                stats.attempts_unreadable += 1;
                caveats.insert(UsageRollupCaveat::UnreadableFields);
            }
            let unavailable = accounting_unavailable
                .iter()
                .any(|unavailable| unavailable == field);
            if unavailable {
                stats.attempts_accounting_unavailable += 1;
            }
            if !unavailable
                && accounting_breakdown
                    .is_some_and(|breakdown| breakdown_reports_field(breakdown, field))
            {
                stats.attempts_reported += 1;
            }
        }

        let observation = &usage_evidence.accounting.usage;
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
        if reports_component_shape(observation) {
            reported_component_shapes += 1;
        }

        let exact = |field: &str| {
            declarations.contains(field)
                && !accounting_unavailable
                    .iter()
                    .any(|unavailable| unavailable == field)
                && breakdown_reports_field(breakdown, field)
        };
        let direct_input_exact = exact(FIELD_INPUT_TOKENS);
        let inclusive_input_exact =
            exact(FIELD_INPUT_TOKENS_WITH_CACHE_READ) && exact(FIELD_CACHE_READ_TOKENS);
        let input = (direct_input_exact || inclusive_input_exact)
            .then_some(breakdown.input_tokens)
            .flatten();
        let cache_read = exact(FIELD_CACHE_READ_TOKENS)
            .then_some(breakdown.cache_read_tokens)
            .flatten();
        let cache_write = exact(FIELD_CACHE_WRITE_TOKENS)
            .then_some(breakdown.cache_write_tokens)
            .flatten();
        let output = exact(FIELD_OUTPUT_TOKENS)
            .then_some(breakdown.output_tokens)
            .flatten();
        let reasoning = exact(FIELD_REASONING_TOKENS)
            .then_some(breakdown.reasoning_tokens)
            .flatten();
        let cost_exact = exact(FIELD_COST_USD);
        let total_exact = exact(FIELD_TOTAL_TOKENS);

        // A reported observation contributes only through fields its durable
        // declaration promised. This makes a declarationless schema record a
        // legacy record rather than inviting shape inference, and prevents an
        // undeclared incidental value from enlarging either sums or coverage.
        let contributed = declarations.iter().any(|field| exact(field));
        if contributed {
            tasks_with_usage.insert(task.clone());
        } else {
            coverage.attempts_reported_without_figures += 1;
            caveats.insert(UsageRollupCaveat::ReportedWithoutFigures);
        }

        // Deprecated wire projection, computed once from declarations. It is
        // never a threshold or caveat input.
        if COMPONENT_FIELDS
            .iter()
            .any(|field| declarations.contains(field))
        {
            coverage.attempts_reported_with_components += 1;
        }

        saturated |= !tokens.input_tokens.add(input);
        saturated |= !tokens.cache_read_tokens.add(cache_read);
        saturated |= !tokens.cache_write_tokens.add(cache_write);
        saturated |= !tokens.output_tokens.add(output);
        saturated |= !tokens.reasoning_tokens.add(reasoning);

        let declares_direct_input = declarations.contains(FIELD_INPUT_TOKENS);
        let declares_inclusive_input = declarations.contains(FIELD_INPUT_TOKENS_WITH_CACHE_READ);
        let declares_cache_write = declarations.contains(FIELD_CACHE_WRITE_TOKENS);
        if declares_direct_input || declares_inclusive_input || declares_cache_write {
            let input_field_exact = direct_input_exact
                || (declares_inclusive_input && exact(FIELD_INPUT_TOKENS_WITH_CACHE_READ));
            let cache_write_exact = exact(FIELD_CACHE_WRITE_TOKENS);
            let input_complete = (!declares_direct_input || direct_input_exact)
                && (!declares_inclusive_input || inclusive_input_exact);
            let cache_write_complete = !declares_cache_write || cache_write_exact;
            if input_complete && cache_write_complete {
                tokens.fresh_input_tokens.attempts_complete += 1;
            } else if input_field_exact || cache_write_exact {
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

        let component_values = [input, cache_read, cache_write, output];
        let derived_total = if component_values.iter().all(Option::is_none) {
            None
        } else {
            let mut value = 0_u64;
            for component in component_values.into_iter().flatten() {
                match value.checked_add(component) {
                    Some(sum) => value = sum,
                    None => {
                        value = u64::MAX;
                        saturated = true;
                        break;
                    }
                }
            }
            Some(value)
        };
        let harness_total = total_exact.then(|| {
            breakdown
                .total_tokens
                .filter(|total| total.source == UsageTotalSource::HarnessReported)
                .expect("an exact declared total is harness-reported")
                .value
        });
        let attempt_total = harness_total
            .map(|value| (value, UsageTotalSource::HarnessReported))
            .or_else(|| {
                derived_total.map(|value| (value, UsageTotalSource::DerivedFromComponents))
            });
        if let Some((value, source)) = attempt_total {
            total_attempts += 1;
            match source {
                UsageTotalSource::HarnessReported => saw_harness_total = true,
                UsageTotalSource::DerivedFromComponents => saw_derived_total = true,
            }
            match total_value.checked_add(value) {
                Some(value) => total_value = value,
                None => {
                    total_value = u64::MAX;
                    saturated = true;
                }
            }
        }
        if total_exact
            && matches!(
                breakdown.reconciliation(),
                UsageReconciliation::Mismatch { .. }
            )
        {
            caveats.insert(UsageRollupCaveat::TotalComponentMismatch);
        }
        if cost_exact {
            let reported = breakdown
                .cost
                .as_ref()
                .expect("an exact declared cost has a value");
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
    coverage.tasks_without_attestation = roster
        .tasks
        .keys()
        .filter(|task| !attempts.keys().any(|(held, _)| held == *task))
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

    coverage.declared_by_field = coverage
        .field_coverage
        .iter()
        .filter(|(_, field)| field.attempts_declared > 0)
        .map(|(name, field)| (name.clone(), field.attempts_declared))
        .collect();
    coverage.reported_by_field = coverage
        .field_coverage
        .iter()
        .filter(|(_, field)| field.attempts_declared > 0)
        .map(|(name, field)| (name.clone(), field.attempts_reported))
        .collect();
    coverage.missing_declared_fields = USAGE_FIELDS
        .into_iter()
        .filter(|field| field_is_partial(&coverage, field))
        .map(str::to_owned)
        .collect();

    if coverage.tasks_without_attestation > 0 {
        caveats.insert(UsageRollupCaveat::MembersWithoutAttestation);
    }
    if coverage.attempts_not_reported > 0 || coverage.attempts_not_declared > 0 {
        caveats.insert(UsageRollupCaveat::AttemptsWithoutUsage);
    }
    // Every logical field is graded against exactly the attempts whose durable
    // evidence declared it. An undeclared field has a zero denominator and is
    // intentionally absent; a declared field with fewer exact reports is a
    // partial sum, whether the key was absent, unreadable, or unavailable for
    // checked accounting.
    let components_partial = COMPONENT_FIELDS
        .iter()
        .any(|field| field_is_partial(&coverage, field));
    if components_partial {
        caveats.insert(UsageRollupCaveat::PartialComponents);
    }
    if field_is_partial(&coverage, FIELD_TOTAL_TOKENS) {
        caveats.insert(UsageRollupCaveat::PartialTotal);
    }
    if field_is_partial(&coverage, FIELD_COST_USD) {
        caveats.insert(UsageRollupCaveat::PartialCost);
    }
    if legacy_total_only_shapes > 0 && reported_component_shapes > 0 {
        caveats.insert(UsageRollupCaveat::TotalOnlyAttempts);
    }
    if saw_harness_total && saw_derived_total {
        caveats.insert(UsageRollupCaveat::MixedTotalAuthority);
    }
    if saturated {
        caveats.insert(UsageRollupCaveat::SumSaturated);
    }

    let caveats = caveats.into_iter().collect::<Vec<_>>();
    UsageRollup {
        authority: FactAuthority::AdvisoryProviderCapture,
        provenance: ROLLUP_PROVENANCE.to_owned(),
        composition: ROLLUP_COMPOSITION.to_owned(),
        coverage,
        tokens,
        cost,
        is_complete: caveats.is_empty(),
        caveats,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::adapters::{AdapterConfig, AdapterEngine, ScrapeCapture, ScrapeMode, ScrapeStream};
    use crate::usage::{
        declared_usage_fields, observe, UsageBreakdown, UsageCost, UsageShape, UsageTotalTokens,
    };

    /// The verbatim real claude-code capture `usage::tests` pins the shipped
    /// preset's field map against. Used here so the completeness threshold is
    /// exercised through the real `observe`, over the real declared mapping,
    /// rather than over a breakdown this module hand-authored to suit itself.
    const CLAUDE_STREAM: &str = include_str!("../../../test/fixtures/usage/claude-code.jsonl");

    /// The `claude-code` preset's own field maps, byte-identical to the mirror
    /// in `usage::tests` that the evaluated-configuration check in `flake.nix`
    /// pins against `nix/lib/adapters.nix`.
    const CLAUDE_USAGE_FIELDS: &str = r#"{"cacheReadTokens":["cache_read_input_tokens"],"cacheWriteTokens":["cache_creation_input_tokens"],"inputTokens":["input_tokens"],"outputTokens":["output_tokens"]}"#;
    const CLAUDE_COST_FIELDS: &str = r#"{"costUsd":["$"]}"#;

    const CODEX_STREAM: &str = include_str!("../../../test/fixtures/usage/codex.jsonl");
    const CODEX_RESUME_FRESH_STREAM: &str =
        include_str!("../../../test/fixtures/usage/codex-resume-fresh.jsonl");
    const CODEX_RESUME_CUMULATIVE_STREAM: &str =
        include_str!("../../../test/fixtures/usage/codex-resume-cumulative.jsonl");
    const CODEX_USAGE_FIELDS: &str = r#"{"cacheReadTokens":["cached_input_tokens"],"cacheWriteTokens":["cache_write_input_tokens"],"inputTokensWithCacheRead":["input_tokens"],"outputTokens":["output_tokens"],"reasoningTokens":["reasoning_output_tokens"]}"#;

    fn roster<'a>(tasks: impl IntoIterator<Item = &'a str>) -> ExpectedUsageRoster {
        roster_with_attempts(tasks.into_iter().map(|task| (task, 1)))
    }

    fn roster_with_attempts<'a>(
        tasks: impl IntoIterator<Item = (&'a str, u32)>,
    ) -> ExpectedUsageRoster {
        ExpectedUsageRoster::new(tasks.into_iter().map(|(task, attempt)| {
            ExpectedUsageTask::known(task, NonZeroU32::new(attempt).expect("positive attempt"))
        }))
    }

    fn roster_with_unknown(task: &str) -> ExpectedUsageRoster {
        ExpectedUsageRoster::new([ExpectedUsageTask::unknown(task)])
    }

    fn scrape_capture(mode: ScrapeMode, pattern: &str, fields: &str) -> ScrapeCapture {
        ScrapeCapture {
            stream: ScrapeStream::Stdout,
            mode,
            pattern: pattern.to_owned(),
            counter_scope: None,
            fields: serde_json::from_str(fields).expect("declared fields parse"),
        }
    }

    fn claude_preset() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["claude".to_owned()],
            scrape: BTreeMap::from([
                (
                    "usage".to_owned(),
                    scrape_capture(ScrapeMode::JsonPath, "$..usage", CLAUDE_USAGE_FIELDS),
                ),
                (
                    "usageCost".to_owned(),
                    scrape_capture(
                        ScrapeMode::JsonPathLast,
                        "$[?@.type == 'result'].total_cost_usd",
                        CLAUDE_COST_FIELDS,
                    ),
                ),
            ]),
            ..Default::default()
        }
    }

    fn codex_preset() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["codex".to_owned()],
            usage_counter_scope: crate::adapters::UsageCounterScope::SessionCumulative,
            scrape: BTreeMap::from([(
                "usage".to_owned(),
                scrape_capture(ScrapeMode::JsonPath, "$..usage", CODEX_USAGE_FIELDS),
            )]),
            ..Default::default()
        }
    }

    /// Scrape and normalize one real stream through the real code path.
    fn observed_with(adapter: &AdapterConfig, name: &str, stream: &str) -> Value {
        let adapters = BTreeMap::from([(name.to_owned(), adapter.clone())]);
        let captures = AdapterEngine::new(&adapters)
            .scrape_text(name, stream, "")
            .expect("fixture stream scrapes");
        serde_json::to_value(observe(adapter, &captures)).expect("observation serializes")
    }

    fn observed(stream: &str) -> Value {
        observed_with(&claude_preset(), "claude-code", stream)
    }

    fn attestation_with_fields(
        seq: u64,
        task: &str,
        attempt: u64,
        usage: Value,
        declared_fields: impl IntoIterator<Item = impl ToString>,
    ) -> AttestationRecord {
        let observed: UsageObservation =
            serde_json::from_value(usage.clone()).expect("test usage is readable");
        let usage_evidence = UsageEvidence {
            schema_version: USAGE_EVIDENCE_SCHEMA_VERSION,
            declared_fields: declared_fields
                .into_iter()
                .map(|field| field.to_string())
                .collect(),
            counter_scope: crate::adapters::UsageCounterScope::Attempt,
            observed: observed.clone(),
            accounting: crate::usage::UsageAccounting {
                state: UsageAccountingState::Exact,
                basis: crate::usage::UsageAccountingBasis::Fresh,
                predecessor: None,
                usage: observed,
                unavailable_fields: Vec::new(),
                reason: None,
            },
        };
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
                "usageEvidence": usage_evidence,
                "usageAuthority": "advisory-only",
            }),
            seq,
            prev_hash: "sha256:prev".to_owned(),
            hash: "sha256:hash".to_owned(),
        }
    }

    fn public_declared_attestation(
        seq: u64,
        task: &str,
        usage: Value,
        declared_fields: impl IntoIterator<Item = impl ToString>,
    ) -> AttestationRecord {
        let declared_fields = declared_fields
            .into_iter()
            .map(|field| field.to_string())
            .collect::<Vec<_>>();
        AttestationRecord {
            observed_at: "2026-08-09T00:00:00.000Z".to_owned(),
            payload: json!({
                "kind": "adapter-scrape",
                "taskUuid": task,
                "jobId": task,
                "adapter": "public-adapter",
                "attempt": 1,
                "leaseEpoch": 1,
                "captures": {},
                "usage": usage.clone(),
                "usageEvidence": {
                    "schemaVersion": 1,
                    "declaredFields": declared_fields,
                    "counterScope": "attempt",
                    "derivation": "attempt",
                    "contribution": usage,
                },
                "usageAuthority": "advisory-only",
            }),
            seq,
            prev_hash: "sha256:prev".to_owned(),
            hash: "sha256:hash".to_owned(),
        }
    }

    fn legacy_attestation(seq: u64, task: &str, usage: Value) -> AttestationRecord {
        AttestationRecord {
            observed_at: "2026-08-09T00:00:00.000Z".to_owned(),
            payload: json!({
                "kind": "adapter-scrape",
                "taskUuid": task,
                "jobId": task,
                "adapter": "legacy-adapter",
                "attempt": 1,
                "leaseEpoch": 1,
                "usage": usage,
                "usageAuthority": "advisory-only",
            }),
            seq,
            prev_hash: "sha256:prev".to_owned(),
            hash: "sha256:hash".to_owned(),
        }
    }

    fn attestation(seq: u64, task: &str, attempt: u64, usage: Value) -> AttestationRecord {
        attestation_with_fields(
            seq,
            task,
            attempt,
            usage,
            [
                FIELD_CACHE_READ_TOKENS,
                FIELD_CACHE_WRITE_TOKENS,
                FIELD_INPUT_TOKENS_WITH_CACHE_READ,
                FIELD_OUTPUT_TOKENS,
                FIELD_REASONING_TOKENS,
            ],
        )
    }

    fn attestation_for_adapter(
        seq: u64,
        task: &str,
        attempt: u64,
        adapter: &AdapterConfig,
        usage: Value,
    ) -> AttestationRecord {
        attestation_with_fields(seq, task, attempt, usage, declared_usage_fields(adapter))
    }

    fn claude_attestation(seq: u64, task: &str, attempt: u64, usage: Value) -> AttestationRecord {
        attestation_for_adapter(seq, task, attempt, &claude_preset(), usage)
    }

    fn accounted_attestation(
        seq: u64,
        task: &str,
        attempt: u64,
        lease_epoch: u64,
        captures: &crate::adapters::ScrapeResult,
        evidence: &UsageEvidence,
    ) -> AttestationRecord {
        AttestationRecord {
            observed_at: "2026-08-08T00:00:00.000Z".to_owned(),
            payload: json!({
                "kind": "adapter-scrape",
                "taskUuid": task,
                "jobId": task,
                "adapter": "codex",
                "attempt": attempt,
                "leaseEpoch": lease_epoch,
                "captures": captures.captures,
                "usage": evidence.observed,
                "usageEvidence": evidence,
                "usageAuthority": "advisory-only",
            }),
            seq,
            prev_hash: "sha256:prev".to_owned(),
            hash: "sha256:hash".to_owned(),
        }
    }

    fn checkpoint_usage(input: u64, cache_read: u64, output: u64) -> Value {
        serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Components,
            input_tokens: Some(input),
            input_tokens_as_reported: Some(input + cache_read),
            cache_read_tokens: Some(cache_read),
            cache_write_tokens: Some(0),
            output_tokens: Some(output),
            reasoning_tokens: Some(0),
            total_tokens: Some(UsageTotalTokens {
                value: input + cache_read + output,
                source: UsageTotalSource::DerivedFromComponents,
            }),
            cost: None,
            unreadable_fields: Vec::new(),
        }))
        .expect("checkpoint serializes")
    }

    fn public_checkpoint_attestation(
        seq: u64,
        task: &str,
        attempt: u64,
        lease_epoch: u64,
        observed: Value,
        usage_evidence: Value,
    ) -> AttestationRecord {
        AttestationRecord {
            observed_at: "2026-08-08T00:00:00.000Z".to_owned(),
            payload: json!({
                "kind": "adapter-scrape",
                "taskUuid": task,
                "jobId": task,
                "adapter": "codex",
                "attempt": attempt,
                "leaseEpoch": lease_epoch,
                "captures": {},
                "usage": observed,
                "usageEvidence": usage_evidence,
                "usageAuthority": "advisory-only",
            }),
            seq,
            prev_hash: "sha256:prev".to_owned(),
            hash: format!("sha256:{seq:064x}"),
        }
    }

    /// Exercise the whole scrape -> observe -> accounting -> ledger path for a
    /// fresh attempt, retaining the adapter's real declared field set.
    fn fresh_scrape_attestation(
        seq: u64,
        task: &str,
        attempt: u64,
        adapter_name: &str,
        adapter: &AdapterConfig,
        stream: &str,
    ) -> AttestationRecord {
        let adapters = BTreeMap::from([(adapter_name.to_owned(), adapter.clone())]);
        let engine = AdapterEngine::new(&adapters);
        engine.validate_all().expect("test adapter validates");
        let captures = engine
            .scrape_text(adapter_name, stream, "")
            .expect("test stream scrapes");
        let evidence = crate::usage::account_usage(
            adapter_name,
            adapter,
            &captures,
            &crate::usage::UsageAccountingMode::Fresh,
            None,
        );
        accounted_attestation(seq, task, attempt, 1, &captures, &evidence)
    }

    #[test]
    fn real_codex_resume_rolls_up_the_delta_not_the_cumulative_reading() {
        let mut adapter = codex_preset();
        adapter.scrape.insert(
            "sessionRef".to_owned(),
            scrape_capture(ScrapeMode::JsonPath, "$..thread_id", "{}"),
        );
        let adapters = BTreeMap::from([("codex".to_owned(), adapter.clone())]);
        let engine = AdapterEngine::new(&adapters);
        let fresh_captures = engine
            .scrape_text("codex", CODEX_RESUME_FRESH_STREAM, "")
            .unwrap();
        let fresh = crate::usage::account_usage(
            "codex",
            &adapter,
            &fresh_captures,
            &crate::usage::UsageAccountingMode::Fresh,
            None,
        );
        let task = "00000000-0000-4000-8000-000000000403";
        let fresh_record = accounted_attestation(1, task, 1, 7, &fresh_captures, &fresh);
        let resumed_captures = engine
            .scrape_text("codex", CODEX_RESUME_CUMULATIVE_STREAM, "")
            .unwrap();
        let resumed = crate::usage::account_usage(
            "codex",
            &adapter,
            &resumed_captures,
            &crate::usage::UsageAccountingMode::Resume {
                predecessor: Some(crate::usage::UsagePredecessor::new(task, 1, 7)),
            },
            Some(&fresh_record.payload),
        );
        let resumed_record = accounted_attestation(2, task, 2, 8, &resumed_captures, &resumed);
        let rollup = roll_up(
            &roster_with_attempts([(task, 2)]),
            &AttestationEvidence::new(true, &[fresh_record, resumed_record]),
        );
        assert_eq!(rollup.coverage.attempts_reported, 2);
        assert_eq!(
            rollup.tokens.total_tokens.map(|total| total.value),
            Some(32_845)
        );
        assert_eq!(rollup.tokens.input_tokens.value, 6_722);
        assert_eq!(rollup.tokens.cache_read_tokens.value, 26_112);
        assert_eq!(rollup.tokens.output_tokens.value, 11);
        assert!(
            rollup.is_complete(),
            "unexpected caveats: {:?}",
            rollup.caveats
        );
    }

    #[test]
    fn public_codex_checkpoints_roll_up_zero_baseline_plus_verified_delta() {
        let task = "00000000-0000-4000-8000-000000000403";
        let fresh = checkpoint_usage(5_042, 11_008, 5);
        let cumulative = checkpoint_usage(10_101, 22_016, 11);
        let delta = checkpoint_usage(5_059, 11_008, 6);
        let declared_fields = json!([
            FIELD_CACHE_READ_TOKENS,
            FIELD_CACHE_WRITE_TOKENS,
            FIELD_INPUT_TOKENS_WITH_CACHE_READ,
            FIELD_OUTPUT_TOKENS,
            FIELD_REASONING_TOKENS,
        ]);
        let lineage = json!({
            "adapter": "codex",
            "sessionRef": "00000000-0000-4000-8000-000000000403",
        });
        let records = [
            public_checkpoint_attestation(
                1,
                task,
                1,
                1,
                fresh.clone(),
                json!({
                    "schemaVersion": 1,
                    "declaredFields": declared_fields,
                    "counterScope": "session-cumulative",
                    "derivation": "fresh-zero",
                    "lineage": lineage,
                    "contribution": fresh,
                }),
            ),
            public_checkpoint_attestation(
                2,
                task,
                2,
                1,
                cumulative,
                json!({
                    "schemaVersion": 1,
                    "declaredFields": declared_fields,
                    "counterScope": "session-cumulative",
                    "derivation": "delta",
                    "lineage": lineage,
                    "predecessor": {
                        "taskUuid": task,
                        "attempt": 1,
                        "leaseEpoch": 1,
                        "sequence": 1,
                        "hash": format!("sha256:{:064x}", 1),
                    },
                    "contribution": delta,
                }),
            ),
        ];
        let rollup = roll_up(
            &roster_with_attempts([(task, 2)]),
            &AttestationEvidence::new(true, &records),
        );

        assert_eq!(rollup.coverage.attempts_legacy_usage, 0);
        assert_eq!(rollup.tokens.input_tokens.value, 10_101);
        assert_eq!(rollup.tokens.cache_read_tokens.value, 22_016);
        assert_eq!(rollup.tokens.output_tokens.value, 11);
        assert_eq!(
            rollup.tokens.total_tokens.map(|total| total.value),
            Some(32_128)
        );
        assert!(rollup.is_complete(), "unexpected: {:?}", rollup.caveats);
        assert!(serde_json::to_value(&rollup).unwrap()["tokens"]["totalTokens"].is_object());

        let records_by_sequence = records
            .iter()
            .map(|record| (record.seq, record))
            .collect::<BTreeMap<_, _>>();
        let resumed = accounted_usage(&records[1], &records_by_sequence)
            .expect("the bound public delta is accounted");
        let resumed = resumed
            .accounting
            .usage
            .breakdown()
            .expect("the contribution is reported");
        assert_eq!(resumed.input_tokens_as_reported, Some(16_067));
        let accounted_sum = 16_050 + resumed.input_tokens_as_reported.unwrap();
        let forbidden_raw_sum = 16_050 + 32_117;
        assert_eq!(accounted_sum, 32_117);
        assert_eq!(forbidden_raw_sum, 48_167);
        assert_ne!(accounted_sum, forbidden_raw_sum);

        let mut unbound = records.clone();
        unbound[1].payload["usageEvidence"]["predecessor"]["hash"] =
            json!("sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff");
        let rejected = roll_up(
            &roster_with_attempts([(task, 2)]),
            &AttestationEvidence::new(true, &unbound),
        );
        assert_eq!(rejected.coverage.attempts_legacy_usage, 1);
        assert_eq!(
            rejected.tokens.total_tokens.map(|total| total.value),
            Some(16_055),
            "an unbound raw cumulative checkpoint must not be charged"
        );
        assert!(rejected
            .caveats
            .contains(&UsageRollupCaveat::LegacyUsageContract));
    }

    #[test]
    fn a_cumulative_checkpoint_without_its_predecessor_names_the_missing_baseline() {
        let task = "00000000-0000-4000-8000-000000000403";
        let record = public_checkpoint_attestation(
            1,
            task,
            1,
            1,
            checkpoint_usage(42, 100, 8),
            json!({
                "schemaVersion": 1,
                "declaredFields": [
                    FIELD_CACHE_READ_TOKENS,
                    FIELD_CACHE_WRITE_TOKENS,
                    FIELD_INPUT_TOKENS_WITH_CACHE_READ,
                    FIELD_OUTPUT_TOKENS,
                    FIELD_REASONING_TOKENS,
                ],
                "counterScope": "session-cumulative",
                "derivation": "baseline-missing",
                "lineage": {
                    "adapter": "codex",
                    "sessionRef": "missing-predecessor-fixture",
                },
            }),
        );
        let rollup = roll_up(&roster([task]), &AttestationEvidence::new(true, &[record]));

        assert_eq!(rollup.coverage.attempts_legacy_usage, 0);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::CumulativeBaselineMissing));
        assert!(!rollup.is_complete());
        let public = serde_json::to_value(&rollup).unwrap();
        assert!(public["caveats"]
            .as_array()
            .unwrap()
            .contains(&json!("cumulative-baseline-missing")));
        assert_eq!(public["isComplete"], json!(false));
    }

    #[test]
    fn legacy_raw_usage_is_visible_evidence_but_not_a_confident_rollup_charge() {
        let mut legacy = attestation(1, "task", 1, codex_usage());
        legacy
            .payload
            .as_object_mut()
            .unwrap()
            .remove("usageEvidence");
        let rollup = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(true, &[legacy]),
        );
        assert_eq!(rollup.coverage.attempts_legacy_usage, 1);
        assert_eq!(rollup.tokens.total_tokens, None);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::LegacyUsageContract));
        assert!(!rollup.is_complete());
    }

    #[test]
    fn a_schema_record_without_a_declaration_is_legacy_not_shape_inferred() {
        let mut legacy = attestation(1, "task", 1, codex_usage());
        legacy.payload["usageEvidence"]
            .as_object_mut()
            .unwrap()
            .remove("declaredFields");
        let rollup = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(true, &[legacy]),
        );

        assert_eq!(rollup.coverage.attempts_legacy_usage, 1);
        assert_eq!(rollup.coverage.attempts_without_usage_record, 0);
        assert!(rollup
            .coverage
            .field_coverage
            .values()
            .all(|field| *field == UsageFieldCoverage::default()));
        assert_eq!(rollup.tokens.total_tokens, None);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::LegacyUsageContract));
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::DeclaredSurfaceUnknown));
    }

    #[test]
    fn two_ambiguous_legacy_total_only_attempts_retain_the_reported_shape_diagnosis() {
        let records = [
            legacy_attestation(1, "component", codex_usage()),
            legacy_attestation(2, "total-a", declared_total_usage()),
            legacy_attestation(3, "total-b", declared_total_usage()),
        ];
        let rollup = roll_up(
            &roster(["component", "total-a", "total-b"]),
            &AttestationEvidence::new(true, &records),
        );

        assert_eq!(rollup.coverage.attempts_legacy_usage, 3);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::LegacyUsageContract));
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::DeclaredSurfaceUnknown));
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::TotalOnlyAttempts));
        assert!(!rollup.is_complete());
        assert!(rollup.provenance.contains("reported-shape"));
        assert!(rollup.provenance.contains("declared-surface-unknown"));
        let public = serde_json::to_value(&rollup).unwrap();
        let caveats = public["caveats"].as_array().unwrap();
        for expected in [
            "legacy-usage-contract",
            "declared-surface-unknown",
            "total-only-attempts",
        ] {
            assert!(caveats.contains(&json!(expected)), "missing {expected}");
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
    /// and nothing else — legal, since `totalTokens` is a declarable field and
    /// nothing obliges an adapter to declare components beside it.
    ///
    /// Produced by the real [`observe`] over that real declared map, not
    /// hand-authored. The first draft of this fixture asserted `shape:
    /// components` with all four components set, which the real path cannot
    /// produce for a total-only mapping: it produces a `lump` with every
    /// component absent. That gap is what let a completeness regression against
    /// this exact configuration pass a green suite.
    fn declared_total_adapter() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["declared-total-agent".to_owned()],
            scrape: BTreeMap::from([(
                "usage".to_owned(),
                scrape_capture(
                    ScrapeMode::JsonPath,
                    "$..usage",
                    r#"{"totalTokens":["total_tokens"]}"#,
                ),
            )]),
            ..Default::default()
        }
    }

    fn declared_total_usage() -> Value {
        let adapter = declared_total_adapter();
        observed_with(
            &adapter,
            "declared-total",
            r#"{"type":"turn.completed","usage":{"total_tokens":120}}"#,
        )
    }

    #[test]
    fn a_fully_reporting_input_output_only_adapter_is_complete_for_its_declaration() {
        let adapter = AdapterConfig {
            argv: vec!["cacheless-agent".to_owned()],
            scrape: BTreeMap::from([(
                "usage".to_owned(),
                scrape_capture(
                    ScrapeMode::JsonPath,
                    "$..usage",
                    r#"{"inputTokens":["input_tokens"],"outputTokens":["output_tokens"]}"#,
                ),
            )]),
            ..Default::default()
        };
        let record = fresh_scrape_attestation(
            1,
            "task",
            1,
            "cacheless",
            &adapter,
            r#"{"usage":{"input_tokens":100,"output_tokens":20}}"#,
        );
        let rollup = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(true, &[record]),
        );

        assert!(rollup.is_complete(), "unexpected: {:?}", rollup.caveats);
        assert_eq!(rollup.tokens.total_tokens.unwrap().value, 120);
        assert_eq!(rollup.tokens.fresh_input_tokens.value, 100);
        assert_eq!(rollup.tokens.fresh_input_tokens.attempts_complete, 1);
        assert_eq!(
            rollup.coverage.field_coverage[FIELD_INPUT_TOKENS].attempts_declared,
            1
        );
        assert_eq!(
            rollup.coverage.field_coverage[FIELD_CACHE_READ_TOKENS],
            UsageFieldCoverage::default(),
            "an undeclared cache field is intentionally absent, never partial"
        );
    }

    #[test]
    fn public_declared_and_reported_field_census_exposes_heterogeneous_drift() {
        let honest = serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Components,
            input_tokens: Some(7),
            input_tokens_as_reported: Some(7),
            cache_read_tokens: None,
            cache_write_tokens: None,
            output_tokens: Some(3),
            reasoning_tokens: None,
            total_tokens: Some(UsageTotalTokens {
                value: 10,
                source: UsageTotalSource::DerivedFromComponents,
            }),
            cost: None,
            unreadable_fields: Vec::new(),
        }))
        .unwrap();
        let cost_only = serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Lump,
            input_tokens: None,
            input_tokens_as_reported: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            output_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost: Some(UsageCost {
                amount: serde_json::Number::from_f64(1.25).unwrap(),
                currency: "USD".to_owned(),
            }),
            unreadable_fields: Vec::new(),
        }))
        .unwrap();
        let total_only = serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Lump,
            input_tokens: None,
            input_tokens_as_reported: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            output_tokens: None,
            reasoning_tokens: None,
            total_tokens: Some(UsageTotalTokens {
                value: 12,
                source: UsageTotalSource::HarnessReported,
            }),
            cost: None,
            unreadable_fields: Vec::new(),
        }))
        .unwrap();
        let drifted = serde_json::to_value(UsageObservation::Reported(UsageBreakdown {
            shape: UsageShape::Lump,
            input_tokens: None,
            input_tokens_as_reported: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            output_tokens: None,
            reasoning_tokens: None,
            total_tokens: Some(UsageTotalTokens {
                value: 100,
                source: UsageTotalSource::HarnessReported,
            }),
            cost: None,
            unreadable_fields: Vec::new(),
        }))
        .unwrap();
        let records = [
            public_declared_attestation(
                1,
                "honest",
                honest,
                [FIELD_INPUT_TOKENS, FIELD_OUTPUT_TOKENS],
            ),
            public_declared_attestation(2, "cost-only", cost_only, [FIELD_COST_USD]),
            public_declared_attestation(3, "total-only", total_only, [FIELD_TOTAL_TOKENS]),
            public_declared_attestation(
                4,
                "drifted",
                drifted,
                [
                    FIELD_INPUT_TOKENS,
                    FIELD_CACHE_READ_TOKENS,
                    FIELD_CACHE_WRITE_TOKENS,
                    FIELD_OUTPUT_TOKENS,
                    FIELD_TOTAL_TOKENS,
                ],
            ),
        ];
        let rollup = roll_up(
            &roster(["honest", "cost-only", "total-only", "drifted"]),
            &AttestationEvidence::new(true, &records),
        );

        assert_eq!(
            rollup.coverage.declared_by_field,
            BTreeMap::from([
                (FIELD_CACHE_READ_TOKENS.to_owned(), 1),
                (FIELD_CACHE_WRITE_TOKENS.to_owned(), 1),
                (FIELD_COST_USD.to_owned(), 1),
                (FIELD_INPUT_TOKENS.to_owned(), 2),
                (FIELD_OUTPUT_TOKENS.to_owned(), 2),
                (FIELD_TOTAL_TOKENS.to_owned(), 2),
            ])
        );
        assert_eq!(
            rollup.coverage.reported_by_field,
            BTreeMap::from([
                (FIELD_CACHE_READ_TOKENS.to_owned(), 0),
                (FIELD_CACHE_WRITE_TOKENS.to_owned(), 0),
                (FIELD_COST_USD.to_owned(), 1),
                (FIELD_INPUT_TOKENS.to_owned(), 1),
                (FIELD_OUTPUT_TOKENS.to_owned(), 1),
                (FIELD_TOTAL_TOKENS.to_owned(), 2),
            ])
        );
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialComponents));
        assert!(!rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialFreshInput));
        assert_eq!(rollup.tokens.fresh_input_tokens.attempts_complete, 1);
        assert_eq!(rollup.tokens.fresh_input_tokens.attempts_partial, 0);
        assert_eq!(
            rollup.coverage.missing_declared_fields,
            [
                FIELD_INPUT_TOKENS,
                FIELD_CACHE_READ_TOKENS,
                FIELD_CACHE_WRITE_TOKENS,
                FIELD_OUTPUT_TOKENS,
            ]
        );
        assert!(!rollup.is_complete());
        let public = serde_json::to_value(&rollup).unwrap();
        assert_eq!(
            public["coverage"]["declaredByField"],
            json!({
                "cacheReadTokens": 1,
                "cacheWriteTokens": 1,
                "costUsd": 1,
                "inputTokens": 2,
                "outputTokens": 2,
                "totalTokens": 2,
            })
        );
        assert_eq!(
            public["coverage"]["reportedByField"],
            json!({
                "cacheReadTokens": 0,
                "cacheWriteTokens": 0,
                "costUsd": 1,
                "inputTokens": 1,
                "outputTokens": 1,
                "totalTokens": 2,
            })
        );
        assert_eq!(
            public["coverage"]["missingDeclaredFields"],
            json!([
                "inputTokens",
                "cacheReadTokens",
                "cacheWriteTokens",
                "outputTokens",
            ])
        );
    }

    #[test]
    fn a_cost_only_adapter_is_complete_for_the_surface_it_declared() {
        let adapter = AdapterConfig {
            argv: vec!["cost-agent".to_owned()],
            scrape: BTreeMap::from([(
                "usageCost".to_owned(),
                scrape_capture(ScrapeMode::JsonPathLast, "$..cost", r#"{"costUsd":["$"]}"#),
            )]),
            ..Default::default()
        };
        let record =
            fresh_scrape_attestation(1, "task", 1, "cost-only", &adapter, r#"{"cost":1.25}"#);
        let rollup = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(true, &[record]),
        );

        assert!(rollup.is_complete(), "unexpected: {:?}", rollup.caveats);
        assert_eq!(rollup.tokens.total_tokens, None);
        assert_eq!(rollup.cost.amount_usd, Some(1.25));
        assert_eq!(rollup.cost.attempts, 1);
        assert_eq!(
            rollup.coverage.field_coverage[FIELD_COST_USD],
            UsageFieldCoverage {
                attempts_declared: 1,
                attempts_reported: 1,
                attempts_unreadable: 0,
                attempts_accounting_unavailable: 0,
            }
        );
    }

    #[test]
    fn field_coverage_distinguishes_declared_absence_from_unreadable_accounting() {
        let adapter = AdapterConfig {
            argv: vec!["two-field-agent".to_owned()],
            scrape: BTreeMap::from([(
                "usage".to_owned(),
                scrape_capture(
                    ScrapeMode::JsonPath,
                    "$..usage",
                    r#"{"inputTokens":["input_tokens"],"outputTokens":["output_tokens"]}"#,
                ),
            )]),
            ..Default::default()
        };
        let absent = fresh_scrape_attestation(
            1,
            "absent",
            1,
            "two-field",
            &adapter,
            r#"{"usage":{"output_tokens":5}}"#,
        );
        let absent_rollup = roll_up(
            &roster(["absent"]),
            &AttestationEvidence::new(true, &[absent]),
        );
        assert_eq!(
            absent_rollup.coverage.field_coverage[FIELD_INPUT_TOKENS],
            UsageFieldCoverage {
                attempts_declared: 1,
                attempts_reported: 0,
                attempts_unreadable: 0,
                attempts_accounting_unavailable: 0,
            }
        );
        assert!(absent_rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialComponents));
        assert!(!absent_rollup
            .caveats
            .contains(&UsageRollupCaveat::UnreadableFields));

        let unreadable = fresh_scrape_attestation(
            2,
            "unreadable",
            1,
            "two-field",
            &adapter,
            r#"{"usage":{"input_tokens":"many","output_tokens":5}}"#,
        );
        let unreadable_rollup = roll_up(
            &roster(["unreadable"]),
            &AttestationEvidence::new(true, &[unreadable]),
        );
        assert_eq!(
            unreadable_rollup.coverage.field_coverage[FIELD_INPUT_TOKENS],
            UsageFieldCoverage {
                attempts_declared: 1,
                attempts_reported: 0,
                attempts_unreadable: 1,
                attempts_accounting_unavailable: 1,
            }
        );
        assert!(unreadable_rollup
            .caveats
            .contains(&UsageRollupCaveat::UnreadableFields));
        assert!(unreadable_rollup
            .caveats
            .contains(&UsageRollupCaveat::AccountingUnavailable));

        let mut cumulative = adapter;
        cumulative.usage_counter_scope = crate::adapters::UsageCounterScope::SessionCumulative;
        let adapters = BTreeMap::from([("two-field".to_owned(), cumulative.clone())]);
        let engine = AdapterEngine::new(&adapters);
        engine.validate_all().unwrap();
        let captures = engine
            .scrape_text(
                "two-field",
                r#"{"usage":{"input_tokens":10,"output_tokens":5}}"#,
                "",
            )
            .unwrap();
        let unavailable = crate::usage::account_usage(
            "two-field",
            &cumulative,
            &captures,
            &crate::usage::UsageAccountingMode::Resume {
                predecessor: Some(crate::usage::UsagePredecessor::new("prior", 1, 7)),
            },
            None,
        );
        let unavailable = accounted_attestation(3, "unavailable", 1, 1, &captures, &unavailable);
        let unavailable_rollup = roll_up(
            &roster(["unavailable"]),
            &AttestationEvidence::new(true, &[unavailable]),
        );
        assert_eq!(
            unavailable_rollup.coverage.field_coverage[FIELD_INPUT_TOKENS],
            UsageFieldCoverage {
                attempts_declared: 1,
                attempts_reported: 0,
                attempts_unreadable: 0,
                attempts_accounting_unavailable: 1,
            },
            "a readable cumulative value can still be unavailable as an attempt delta"
        );
    }

    #[test]
    fn a_two_harness_run_sums_every_component_and_grades_both_totals_derived() {
        let records = [
            attestation(1, "task-codex", 1, codex_usage()),
            claude_attestation(2, "task-claude", 1, claude_usage()),
        ];
        let rollup = roll_up(
            &roster(["task-codex", "task-claude"]),
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

        // Cost is where a harness declared and reported it: claude's one
        // attempt. Codex did not declare cost, so it is outside the denominator
        // rather than making the mixed run permanently partial.
        assert_eq!(rollup.cost.attempts, 1);
        assert_eq!(rollup.cost.amount_usd, Some(8.755_705));
        assert!(!rollup.caveats.contains(&UsageRollupCaveat::PartialCost));
        assert_eq!(
            rollup.coverage.field_coverage[FIELD_COST_USD],
            UsageFieldCoverage {
                attempts_declared: 1,
                attempts_reported: 1,
                attempts_unreadable: 0,
                attempts_accounting_unavailable: 0,
            }
        );
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
        // The real path's shape for a total-only mapping: a lump, with every
        // component absent because none was ever declared.
        let observation = declared_total_usage();
        assert_eq!(observation["breakdown"]["shape"], "lump");
        assert!(observation["breakdown"]["inputTokens"].is_null());

        let declared = [attestation_for_adapter(
            1,
            "declared",
            1,
            &declared_total_adapter(),
            observation,
        )];
        let harness = roll_up(
            &roster(["declared"]),
            &AttestationEvidence::new(true, &declared),
        );
        assert_eq!(
            harness.tokens.total_tokens.unwrap().source,
            UsageRollupTotalSource::HarnessReported
        );
        // Nothing about this run is partial: it reported every figure it
        // declared. Judging it against a component threshold it declared no
        // components for would tell an operator forever that a complete run is
        // incomplete, which is the caveat's own meaning inverted.
        assert_eq!(harness.coverage.attempts_reported, 1);
        assert_eq!(harness.coverage.attempts_reported_with_components, 0);
        assert!(harness.is_complete(), "{:?}", harness.caveats);
        // A run mixing that adapter with a preset reaches the mixed total grade.
        // Each field still covers every attempt that declared it, so neither
        // adapter's observed shape invents a missing component.
        let records = [
            attestation_for_adapter(
                1,
                "declared",
                1,
                &declared_total_adapter(),
                declared_total_usage(),
            ),
            attestation(2, "preset", 1, codex_usage()),
        ];
        let mixed = roll_up(
            &roster(["declared", "preset"]),
            &AttestationEvidence::new(true, &records),
        );
        let total = mixed.tokens.total_tokens.expect("both attempts total");
        assert_eq!(total.value, 120 + 7_093_008);
        assert_eq!(total.source, UsageRollupTotalSource::Mixed);
        assert_eq!(mixed.coverage.attempts_reported, 2);
        assert_eq!(mixed.coverage.attempts_reported_with_components, 1);
        assert!(mixed
            .caveats
            .contains(&UsageRollupCaveat::MixedTotalAuthority));
        assert!(!mixed
            .caveats
            .contains(&UsageRollupCaveat::PartialComponents));
        // A total-only neighbor hides nothing from the declared threshold: a
        // drifted preset attempt beside it is still caught.
        let drifted = CODEX_STREAM.replace("cached_input_tokens", "cached_input_tokens_v2");
        let records = [
            attestation_for_adapter(
                1,
                "declared",
                1,
                &declared_total_adapter(),
                declared_total_usage(),
            ),
            attestation_for_adapter(
                2,
                "preset",
                1,
                &codex_preset(),
                observed_with(&codex_preset(), "codex", &drifted),
            ),
        ];
        let hidden = roll_up(
            &roster(["declared", "preset"]),
            &AttestationEvidence::new(true, &records),
        );
        assert_eq!(hidden.coverage.attempts_reported_with_components, 1);
        assert!(hidden
            .caveats
            .contains(&UsageRollupCaveat::PartialComponents));
    }

    /// An adapter that declares components **and** a total, whose harness then
    /// renames every component key at once.
    ///
    /// The drifted report has the same shape as a legal total-only adapter, but
    /// its durable declaration does not: every component remains in its own
    /// denominator and is reported by only the honest attempt. This is the
    /// uniform-drift case reported shape could not distinguish.
    #[test]
    fn an_all_component_drift_behind_a_surviving_total_is_not_a_complete_run() {
        let adapter = AdapterConfig {
            argv: vec!["components-and-total-agent".to_owned()],
            scrape: BTreeMap::from([(
                "usage".to_owned(),
                scrape_capture(
                    ScrapeMode::JsonPath,
                    "$..usage",
                    r#"{"inputTokens":["input_tokens"],"cacheReadTokens":["cache_read_input_tokens"],"cacheWriteTokens":["cache_creation_input_tokens"],"outputTokens":["output_tokens"],"totalTokens":["total_tokens"]}"#,
                ),
            )]),
            ..Default::default()
        };
        let stream = concat!(
            r#"{"type":"turn.completed","usage":{"input_tokens":100,"#,
            r#""cache_read_input_tokens":900000,"cache_creation_input_tokens":5000,"#,
            r#""output_tokens":20,"total_tokens":905120}}"#
        );
        let drifted = [
            "input_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "output_tokens",
        ]
        .iter()
        .fold(stream.to_owned(), |stream, key| {
            stream.replace(&format!("\"{key}\""), &format!("\"{key}_v2\""))
        });

        let honest = observed_with(&adapter, "components-and-total", stream);
        assert_eq!(honest["breakdown"]["shape"], "components");
        let after = observed_with(&adapter, "components-and-total", &drifted);
        // The drifted attempt is indistinguishable, from its report alone,
        // from a legal total-only adapter's attempt.
        assert_eq!(after["breakdown"]["shape"], "lump");
        assert_eq!(
            after["breakdown"]["totalTokens"]["source"],
            "harness-reported"
        );

        let records = [
            attestation_for_adapter(1, "task", 1, &adapter, honest),
            attestation_for_adapter(2, "task", 2, &adapter, after.clone()),
            attestation_for_adapter(3, "task", 3, &adapter, after),
        ];
        let rollup = roll_up(
            &roster_with_attempts([("task", 3)]),
            &AttestationEvidence::new(true, &records),
        );

        assert_eq!(rollup.coverage.attempts_reported, 3);
        assert_eq!(
            rollup.coverage.attempts_reported_with_components, 3,
            "all three attempts declared components whatever shape they reported"
        );
        assert_eq!(rollup.coverage.attempts_reported_without_figures, 0);
        // Two thirds of the run's tokens are absent from every component sum...
        assert_eq!(
            rollup.tokens.input_tokens,
            UsageSum {
                value: 100,
                attempts: 1
            }
        );
        assert_eq!(
            rollup.tokens.cache_read_tokens,
            UsageSum {
                value: 900_000,
                attempts: 1
            }
        );
        assert_eq!(rollup.tokens.total_tokens.unwrap().value, 905_120 * 3);
        // ...and each component's declared denominator exposes that strict
        // subset directly, including when more than one drifted observation
        // has the same lump shape as a legal total-only adapter.
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialComponents));
        assert_eq!(
            rollup.coverage.field_coverage[FIELD_INPUT_TOKENS],
            UsageFieldCoverage {
                attempts_declared: 3,
                attempts_reported: 1,
                attempts_unreadable: 0,
                attempts_accounting_unavailable: 0,
            }
        );
        assert!(!rollup.is_complete());
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
            &roster(["task"]),
            &AttestationEvidence::new(
                true,
                &[attestation_with_fields(
                    1,
                    "task",
                    1,
                    unmapped,
                    [FIELD_INPUT_TOKENS, FIELD_OUTPUT_TOKENS],
                )],
            ),
        );
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(rollup.coverage.attempts_reported_without_figures, 1);
        assert_eq!(
            rollup.coverage.tasks_with_reported_usage, 0,
            "an attempt that contributed nothing does not make its task covered"
        );
        assert_eq!(rollup.tokens.total_tokens, None);
        assert_eq!(rollup.tokens.input_tokens.attempts, 0);
        assert_eq!(rollup.tokens.output_tokens.attempts, 0);
        assert_eq!(rollup.tokens.fresh_input_tokens.attempts_partial, 0);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::ReportedWithoutFigures));
        assert!(!rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialFreshInput));
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
            &roster(["task"]),
            &AttestationEvidence::new(
                true,
                &[attestation_with_fields(
                    1,
                    "task",
                    1,
                    reasoning_only,
                    [FIELD_REASONING_TOKENS],
                )],
            ),
        );
        assert_eq!(partial.coverage.attempts_reported_without_figures, 0);
        assert_eq!(partial.coverage.tasks_with_reported_usage, 1);
        assert_eq!(partial.tokens.reasoning_tokens.value, 7);
        assert_eq!(partial.tokens.total_tokens, None);
    }

    /// A harness renaming **one** key must not read as a complete run.
    ///
    /// Driven end to end through the real declared mapping and the real
    /// `observe`, against the verbatim claude-code capture, because the whole
    /// defect is invisible to a hand-authored breakdown: every other declared
    /// path still resolves, so the attempt contributes, stays out of
    /// `attemptsReportedWithoutFigures`, and the component simply disappears
    /// from the total.
    #[test]
    fn one_renamed_harness_key_deletes_a_component_and_is_not_a_complete_run() {
        let honest = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(
                true,
                &[claude_attestation(1, "task", 1, observed(CLAUDE_STREAM))],
            ),
        );
        // The real capture, unmodified: every component the total sums was
        // reported by the one attempt that reported usage, so nothing fires.
        assert!(honest.is_complete(), "unexpected: {:?}", honest.caveats);
        assert_eq!(honest.tokens.total_tokens.unwrap().value, 11_380_648);
        assert_eq!(honest.tokens.cache_read_tokens.attempts, 1);
        assert_eq!(
            honest.tokens.reasoning_tokens.attempts, 0,
            "claude reports no reasoning figure, and that alone must never caveat a run"
        );

        // Now the harness renames exactly one key. The declared path stops
        // resolving; absence is not unreadability, so `unreadableFields` stays
        // empty and the observation is still `reported` with components.
        let drifted =
            CLAUDE_STREAM.replace("cache_read_input_tokens", "cache_read_input_tokens_v2");
        let rollup = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(
                true,
                &[claude_attestation(1, "task", 1, observed(&drifted))],
            ),
        );
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(
            rollup.coverage.attempts_reported_without_figures, 0,
            "the other declared keys still resolved, so this is not total drift"
        );
        assert_eq!(rollup.tokens.input_tokens.attempts, 1);
        assert_eq!(rollup.tokens.cache_read_tokens.attempts, 0);
        assert_eq!(
            rollup.coverage.field_coverage[FIELD_CACHE_READ_TOKENS],
            UsageFieldCoverage {
                attempts_declared: 1,
                attempts_reported: 0,
                attempts_unreadable: 0,
                attempts_accounting_unavailable: 0,
            },
            "a missing declared key is distinct from an unreadable value"
        );
        // 11,093,140 cache-read tokens left the total: it reports 287,508
        // where the truth for the same attempt is 11,380,648.
        assert_eq!(
            rollup.tokens.total_tokens.unwrap().value,
            83 + 265_127 + 22_298
        );
        assert!(
            rollup
                .caveats
                .contains(&UsageRollupCaveat::PartialComponents),
            "a component missing from every attempt must be stated: {:?}",
            rollup.caveats
        );
        assert!(!rollup.is_complete());

        // Every one of the four components the total is a sum of, one at a
        // time. Pinning only the cache-read arm would leave the other three
        // deletable from the threshold's list against a green suite — the same
        // "one component silently leaves" defect, one level up. The rename
        // carries the surrounding quotes so that renaming `input_tokens` does
        // not also rename `cache_read_input_tokens` and
        // `cache_creation_input_tokens`, which contain it as a substring: each
        // arm must drift exactly one declared key.
        for (key, component) in [
            ("input_tokens", "inputTokens"),
            ("cache_read_input_tokens", "cacheReadTokens"),
            ("cache_creation_input_tokens", "cacheWriteTokens"),
            ("output_tokens", "outputTokens"),
        ] {
            let drifted = CLAUDE_STREAM.replace(&format!("\"{key}\""), &format!("\"{key}_v2\""));
            assert_ne!(drifted, CLAUDE_STREAM, "{key} is not in the capture");
            let rollup = roll_up(
                &roster(["task"]),
                &AttestationEvidence::new(
                    true,
                    &[claude_attestation(1, "task", 1, observed(&drifted))],
                ),
            );
            let tokens = serde_json::to_value(rollup.tokens).unwrap();
            assert_eq!(
                tokens[component]["attempts"], 0,
                "{key}: the drifted component should have left the sums"
            );
            assert_eq!(
                rollup.coverage.attempts_reported_with_components, 1,
                "{key}: the attempt still reported components, so it is judged"
            );
            assert_eq!(
                rollup.coverage.attempts_reported_without_figures, 0,
                "{key}: one key drifted, not all of them"
            );
            assert!(
                rollup
                    .caveats
                    .contains(&UsageRollupCaveat::PartialComponents),
                "{key}: drift in this component is not stated: {:?}",
                rollup.caveats
            );
            assert!(!rollup.is_complete(), "{key}: graded complete");
        }
    }

    /// Codex's input declaration is cache-inclusive, so a missing declared
    /// cache-read value makes the exclusive input unaccountable. The rollup
    /// must not promote the inclusive provider figure to exclusive input.
    #[test]
    fn a_drifted_codex_cache_key_cannot_promote_inclusive_input() {
        let honest = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(
                true,
                &[attestation_for_adapter(
                    1,
                    "task",
                    1,
                    &codex_preset(),
                    observed_with(&codex_preset(), "codex", CODEX_STREAM),
                )],
            ),
        );
        assert!(honest.is_complete(), "unexpected: {:?}", honest.caveats);
        assert_eq!(honest.tokens.input_tokens.value, 262_086);
        assert_eq!(honest.tokens.total_tokens.unwrap().value, 7_093_008);

        let drifted = CODEX_STREAM.replace("cached_input_tokens", "cached_input_tokens_v2");
        let drifted_observation = observed_with(&codex_preset(), "codex", &drifted);
        assert!(drifted_observation["breakdown"]["inputTokens"].is_null());
        assert_eq!(
            drifted_observation["breakdown"]["inputTokensAsReported"],
            json!(7_060_166),
            "the provider's inclusive input arrived even though it cannot be normalized"
        );
        let rollup = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(
                true,
                &[attestation_for_adapter(
                    1,
                    "task",
                    1,
                    &codex_preset(),
                    drifted_observation,
                )],
            ),
        );
        assert_eq!(
            rollup.tokens.total_tokens.unwrap().value,
            32_842,
            "only the exact declared output remains in the derived floor"
        );
        assert_eq!(rollup.tokens.input_tokens.attempts, 0);
        assert_eq!(rollup.tokens.fresh_input_tokens.attempts_partial, 1);
        assert_eq!(rollup.tokens.cache_read_tokens.attempts, 0);
        assert_eq!(
            rollup.coverage.field_coverage[FIELD_CACHE_READ_TOKENS].attempts_reported,
            0
        );
        assert_eq!(
            rollup.coverage.field_coverage[FIELD_INPUT_TOKENS_WITH_CACHE_READ].attempts_reported, 1,
            "diagnostics name only the declared cache-read key that disappeared"
        );
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialComponents));
        assert!(!rollup.is_complete());
    }

    /// The `contributed` predicate counts cost, so an attempt whose token
    /// mapping drifted entirely but whose cost capture still resolves is not
    /// `reportedWithoutFigures` — and used to grade complete with no token
    /// figure at all.
    #[test]
    fn a_cost_only_attempt_is_not_a_complete_token_rollup() {
        // Every declared token path drifts; `total_cost_usd` carries no
        // `_tokens` and still resolves, so `observe` returns a `lump` of cost.
        let drifted = CLAUDE_STREAM.replace("_tokens", "_tokens_v2");
        let observation = observed(&drifted);
        assert_eq!(observation["state"], "reported");
        assert_eq!(observation["breakdown"]["shape"], "lump");
        assert!(observation["breakdown"]["cost"].is_object());

        let rollup = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(true, &[claude_attestation(1, "task", 1, observation)]),
        );
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(rollup.coverage.attempts_reported_without_figures, 0);
        assert_eq!(rollup.tokens.total_tokens, None);
        assert_eq!(rollup.tokens.input_tokens.attempts, 0);
        assert_eq!(rollup.tokens.fresh_input_tokens.attempts_partial, 0);
        assert_eq!(rollup.cost.attempts, 1);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialComponents));
        assert!(!rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialFreshInput));
        assert!(!rollup.is_complete());
    }

    #[test]
    fn an_independent_three_attempt_ceiling_exposes_two_missing_ledger_records() {
        // The only attestation may be the latest attempt. Its presence must
        // not make earlier holes invisible or shift the expected range.
        let records = [attestation(1, "task", 3, codex_usage())];
        let rollup = roll_up(
            &roster_with_attempts([("task", 3)]),
            &AttestationEvidence::new(true, &records),
        );

        assert_eq!(rollup.coverage.attempts_expected, 3);
        assert_eq!(rollup.coverage.attempts_attested, 1);
        assert_eq!(rollup.coverage.attempts_missing, 2);
        assert_eq!(
            rollup.coverage.missing_attempts,
            vec![
                UsageAttemptIdentity {
                    task_uuid: "task".to_owned(),
                    attempt: 1,
                },
                UsageAttemptIdentity {
                    task_uuid: "task".to_owned(),
                    attempt: 2,
                },
            ]
        );
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(rollup.tokens.output_tokens.attempts, 1);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::AttemptsMissingAttestation));
        assert!(!rollup.is_complete());
    }

    #[test]
    fn a_member_with_no_attestation_remains_an_expected_missing_attempt() {
        let rollup = roll_up(&roster(["task"]), &AttestationEvidence::new(true, &[]));

        assert_eq!(rollup.coverage.tasks, 1);
        assert_eq!(rollup.coverage.tasks_without_attestation, 1);
        assert_eq!(rollup.coverage.attempts_expected, 1);
        assert_eq!(rollup.coverage.attempts_attested, 0);
        assert_eq!(rollup.coverage.attempts_missing, 1);
        assert_eq!(rollup.coverage.missing_attempts[0].attempt, 1);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::MembersWithoutAttestation));
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::AttemptsMissingAttestation));
    }

    #[test]
    fn an_unknown_attempt_ceiling_keeps_the_task_and_attestation_but_never_grades_complete() {
        let records = [attestation(1, "task", 4, codex_usage())];
        let rollup = roll_up(
            &roster_with_unknown("task"),
            &AttestationEvidence::new(true, &records),
        );

        assert_eq!(rollup.coverage.tasks, 1);
        assert_eq!(rollup.coverage.tasks_with_unknown_attempt_ceiling, 1);
        assert_eq!(rollup.coverage.attempts_expected, 0);
        assert_eq!(rollup.coverage.attempts_attested, 1);
        assert_eq!(rollup.coverage.attempts_missing, 0);
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(rollup.tokens.output_tokens.attempts, 1);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::AttemptCounterUnavailable));
        assert!(!rollup.is_complete());
    }

    #[test]
    fn duplicate_leases_select_the_last_verified_record_and_charge_one_attempt() {
        let first = attestation(1, "task", 1, codex_usage());
        let mut last = attestation(
            2,
            "task",
            1,
            serde_json::to_value(UsageObservation::NotReported).unwrap(),
        );
        last.payload["leaseEpoch"] = json!(2);
        // Deliberately present the higher sequence first: selection follows
        // verified ledger sequence, not slice or lease ordering.
        let records = [last, first];
        let rollup = roll_up(&roster(["task"]), &AttestationEvidence::new(true, &records));

        assert_eq!(rollup.coverage.attempts_observed, 2);
        assert_eq!(rollup.coverage.attempts_attested, 1);
        assert_eq!(rollup.coverage.attempts_with_duplicate_leases, 1);
        assert_eq!(rollup.coverage.attempts_reported, 0);
        assert_eq!(rollup.coverage.attempts_not_reported, 1);
        assert_eq!(rollup.tokens, UsageTokenRollup::default());
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::DuplicateAttemptLeases));
        assert!(!rollup.is_complete());
    }

    #[test]
    fn an_over_ceiling_attestation_is_caveated_and_cannot_enlarge_or_charge_the_roster() {
        let records = [
            attestation(1, "task", 1, codex_usage()),
            attestation(2, "task", 2, codex_usage()),
        ];
        let rollup = roll_up(&roster(["task"]), &AttestationEvidence::new(true, &records));

        assert_eq!(rollup.coverage.attempts_expected, 1);
        assert_eq!(rollup.coverage.attempts_attested, 1);
        assert_eq!(rollup.coverage.attempts_missing, 0);
        assert_eq!(rollup.coverage.attempts_unexpected, 1);
        assert_eq!(rollup.coverage.attempts_observed, 2);
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(rollup.tokens.output_tokens.value, 32_842);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::UnexpectedAttestation));
        assert!(!rollup.is_complete());
    }

    #[test]
    fn missing_attempt_identities_are_bounded_without_shrinking_the_count() {
        let ceiling = u32::try_from(MAX_MISSING_ATTEMPT_IDENTITIES + 3).unwrap();
        let rollup = roll_up(
            &roster_with_attempts([("task", ceiling)]),
            &AttestationEvidence::new(true, &[]),
        );

        assert_eq!(rollup.coverage.attempts_missing, ceiling as usize);
        assert_eq!(
            rollup.coverage.missing_attempts.len(),
            MAX_MISSING_ATTEMPT_IDENTITIES
        );
        assert_eq!(
            rollup.coverage.missing_attempts.last().unwrap().attempt,
            u32::try_from(MAX_MISSING_ATTEMPT_IDENTITIES).unwrap()
        );
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
        let rollup = roll_up(
            &roster_with_attempts([("task", 3)]),
            &AttestationEvidence::new(true, &records),
        );
        assert_eq!(rollup.coverage.attempts_expected, 3);
        assert_eq!(rollup.coverage.attempts_attested, 3);
        assert_eq!(rollup.coverage.attempts_missing, 0);
        assert_eq!(rollup.coverage.attempts_observed, 3);
        assert_eq!(rollup.coverage.attempts_reported, 3);
        assert_eq!(rollup.tokens.output_tokens.value, 32_842 * 3);
        assert_eq!(rollup.tokens.output_tokens.attempts, 3);
        assert!(rollup.is_complete(), "unexpected: {:?}", rollup.caveats);
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
            attestation_with_fields(
                3,
                "silent",
                1,
                serde_json::to_value(UsageObservation::NotDeclared).unwrap(),
                std::iter::empty::<&str>(),
            ),
        ];
        let rollup = roll_up(
            &roster(["reported", "quiet", "silent", "never-scraped"]),
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
        record
            .payload
            .as_object_mut()
            .expect("payload is an object")
            .remove("usageEvidence");
        let rollup = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(true, &[record]),
        );
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
        let rollup = roll_up(
            &roster(["task"]),
            &AttestationEvidence::new(false, &records),
        );
        assert!(!rollup.coverage.ledger_verified);
        assert_eq!(rollup.coverage.attempts_observed, 0);
        assert_eq!(rollup.tokens, UsageTokenRollup::default());
        assert_eq!(rollup.cost.amount_usd, None);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::LedgerUnverified));
        assert_eq!(
            roll_up(&roster(["task"]), &AttestationEvidence::unavailable()).coverage,
            rollup.coverage
        );
    }

    #[test]
    fn an_attestation_for_a_task_the_run_does_not_hold_is_not_charged_to_it() {
        let records = [
            attestation(1, "mine", 1, codex_usage()),
            attestation(2, "someone-elses", 1, codex_usage()),
        ];
        let rollup = roll_up(&roster(["mine"]), &AttestationEvidence::new(true, &records));
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(rollup.tokens.output_tokens.value, 32_842);
    }

    #[test]
    fn a_complete_single_harness_run_carries_no_caveats() {
        let records = [claude_attestation(1, "task", 1, claude_usage())];
        let rollup = roll_up(&roster(["task"]), &AttestationEvidence::new(true, &records));
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
            &roster(["task"]),
            &AttestationEvidence::new(
                true,
                &[attestation_with_fields(
                    1,
                    "task",
                    1,
                    usage,
                    [
                        FIELD_INPUT_TOKENS,
                        FIELD_CACHE_WRITE_TOKENS,
                        FIELD_OUTPUT_TOKENS,
                    ],
                )],
            ),
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
            &roster(["task"]),
            &AttestationEvidence::new(
                true,
                &[attestation_with_fields(
                    1,
                    "task",
                    1,
                    usage,
                    [
                        FIELD_INPUT_TOKENS,
                        FIELD_CACHE_WRITE_TOKENS,
                        FIELD_OUTPUT_TOKENS,
                        FIELD_TOTAL_TOKENS,
                        FIELD_COST_USD,
                    ],
                )],
            ),
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
            claude_attestation(2, "task-claude", 1, claude_usage()),
        ];
        let rollup = roll_up(
            &roster(["task-codex", "task-claude"]),
            &AttestationEvidence::new(true, &records),
        );
        let encoded = serde_json::to_string(&rollup).unwrap();
        let decoded: UsageRollup = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, rollup);
        let value: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(value["authority"], json!("advisory-provider-capture"));
        assert_eq!(value["coverage"]["attemptsExpected"], json!(2));
        assert_eq!(value["coverage"]["attemptsAttested"], json!(2));
        assert_eq!(value["coverage"]["attemptsMissingAttestation"], json!(0));
        assert_eq!(value["isComplete"], json!(true));
        assert_eq!(value["coverage"]["missingAttempts"], json!([]));
        assert_eq!(
            value["coverage"]["tasksWithUnknownAttemptCeiling"],
            json!(0)
        );
        assert_eq!(value["coverage"]["attemptsWithDuplicateLeases"], json!(0));
        assert_eq!(value["coverage"]["attemptsUnexpected"], json!(0));
        assert_eq!(value["coverage"]["missingDeclaredFields"], json!([]));
        assert_eq!(
            value["coverage"]["fieldCoverage"]["cacheReadTokens"],
            json!({
                "attemptsDeclared": 2,
                "attemptsReported": 2,
                "attemptsUnreadable": 0,
                "attemptsAccountingUnavailable": 0,
            })
        );
        assert_eq!(
            value["coverage"]["fieldCoverage"]["reasoningTokens"]["attemptsDeclared"],
            json!(1),
            "claude's undeclared reasoning field is outside the denominator"
        );
        // The composition statement is on the wire, not only in the doc: a
        // consumer must not have to guess which token fields the total is over.
        assert!(value["composition"]
            .as_str()
            .unwrap()
            .contains("freshInputTokens = inputTokens + cacheWriteTokens"));
    }
}
