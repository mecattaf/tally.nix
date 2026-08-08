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
//! **1. Read accounted attempts from the attestation ledger.** The durable row
//! holds only the most recently scraped attempt — exactly as `sessionRef` and
//! `finalMessage` do — so summing rows would charge a three-attempt task once.
//! Every completed scrape, including both typed absences, has a durable
//! `usageEvidence` seat keyed by `taskUuid`/`attempt`/`leaseEpoch`. The rollup
//! sums its exact per-attempt `accounting.usage`, never a raw cumulative
//! observation. Pre-schema raw-only records are visible on job detail but are
//! excluded here and caveated rather than guessed fresh.
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

use crate::adapters::UsageCounterScope;
use crate::query_v2::FactAuthority;
use crate::usage::{
    account_fresh, unavailable_delta, UsageAccounting, UsageAccountingBasis, UsageAccountingReason,
    UsageAccountingState, UsageEvidence, UsageObservation, UsagePredecessor, UsageReconciliation,
    UsageShape, UsageTotalSource, USAGE_EVIDENCE_SCHEMA_VERSION,
};
use crate::witness::AttestationRecord;

/// Payload kind the exit recorder writes one of per scraped attempt.
const ADAPTER_SCRAPE_KIND: &str = "adapter-scrape";

/// Where the rollup's numbers came from.
pub const ROLLUP_PROVENANCE: &str =
    "adapter-scrape usageEvidence.accounting, per attempt, keyed by taskUuid/attempt/leaseEpoch; legacy raw observations excluded";

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
/// subset that does. For the four components a total is a sum of, that is not
/// merely informative — it is the completeness threshold, and it raises
/// [`UsageRollupCaveat::PartialComponents`]. The threshold's exact denominator
/// is [`UsageCoverage::attempts_reported_with_components`], which is
/// `attemptsReported` minus the attempts whose harness stated a total and
/// declared no components at all.
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
    /// Member tasks the ledger holds no attempt for at all. Every successfully
    /// completed scrape writes an attestation even for empty captures, but an
    /// append can fail and old attestations can age out of retention. Either
    /// way these tasks are invisible to the sums and are counted rather than
    /// dropped.
    pub tasks_without_attestation: usize,
    /// Distinct `(task, attempt, leaseEpoch)` triples found for member tasks.
    pub attempts_observed: usize,
    /// Attempts whose attestation carries a `reported` usage record.
    pub attempts_reported: usize,
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
    /// The subset of [`UsageCoverage::attempts_reported`] whose adapter was
    /// reporting components at all, and therefore the denominator the
    /// per-component threshold behind
    /// [`UsageRollupCaveat::PartialComponents`] compares against.
    ///
    /// It is below `attemptsReported` for exactly one **reported** shape: an
    /// attempt that stated a harness total and reported no component beside it
    /// ([`crate::usage::UsageShape::Lump`] with a `harness-reported` total).
    /// An adapter may legally declare `totalTokens` and no components, and for
    /// that adapter every component being absent is the configuration working,
    /// not drift; comparing such an attempt against the component threshold
    /// made a run that reported everything it ever intended to grade
    /// permanently incomplete.
    ///
    /// The exemption is one *reported* shape wide, and that is **not** the
    /// same promise as "an adapter that declared components is always judged".
    /// It is not: although schema-1 evidence carries `declaredFields`, this
    /// compatibility counter is still a projection of reported shape. An
    /// adapter that declared components *and* a total, whose harness renamed
    /// every component key while keeping the total, reports exactly this shape
    /// and leaves this denominator. What
    /// stops that passing silently is
    /// [`UsageRollupCaveat::TotalOnlyAttempts`], which fires whenever such an
    /// attempt sits beside attempts that did report components — then the
    /// component sums provably cover a strict subset of the run and the rollup
    /// says so. The one case reported evidence cannot separate at all is a run
    /// where *every* attempt is total-only: a legal total-only adapter and a
    /// wholly drifted component adapter are indistinguishable to this
    /// shape-based compatibility projection.
    ///
    /// An attempt that reported *any* component is in this denominator even
    /// when its harness also stated a total, and an attempt that stated no
    /// total either — the cost-only and the fully-drifted shapes — is in it
    /// too, because those are the drift cases the threshold exists to catch.
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
    /// A component that feeds the total was reported by fewer attempts than
    /// were reporting components at all, so **that component's sum is over a
    /// subset of those attempts** — the meaning [`UsageSum::attempts`] already
    /// carries, promoted from a number a reader had to notice into a stated
    /// caveat.
    ///
    /// This is what a single renamed harness key looks like. Every other
    /// declared path still resolves, so the attempt contributes and is not in
    /// `attemptsReportedWithoutFigures`; the one component silently drops out
    /// of the total and, on a real claude-code capture, takes 97% of the run's
    /// tokens with it. Checked over exactly the four components the total is a
    /// sum of — `inputTokens`, `cacheReadTokens`, `cacheWriteTokens`,
    /// `outputTokens`.
    ///
    /// Two exclusions, both load-bearing. `reasoningTokens` is not checked:
    /// claude-code reports no reasoning figure at all and it enters no total,
    /// so checking it would fire on every claude run and mean nothing. And the
    /// comparison is against
    /// [`UsageCoverage::attempts_reported_with_components`], not against every
    /// reported attempt, so an adapter that declares a harness total and no
    /// components is not told forever that components it never declared are
    /// missing.
    PartialComponents,
    /// Some attempt contributed a harness-stated total and no component at
    /// all, while other attempts of this run did report components — so the
    /// component sums cover a strict subset of the attempts the total covers.
    ///
    /// Computed from published coverage counts only:
    /// `attemptsReported - attemptsReportedWithComponents > 0` **and**
    /// `attemptsReportedWithComponents > 0`. The evidence is what each attempt
    /// *reported*, not the schema-1 `declaredFields` carried beside it — which
    /// is exactly why the second conjunct is there.
    /// A total-only attempt is either a legal total-only adapter or an adapter
    /// whose component keys all drifted at once, and those two are
    /// indistinguishable in isolation; beside an attempt that did report
    /// components, they stop being indistinguishable in what matters, because
    /// the component sums demonstrably do not cover the whole run.
    ///
    /// Distinct from [`UsageRollupCaveat::PartialComponents`] on purpose:
    /// that one means a component is missing *within* the attempts being
    /// judged, this one means an attempt is missing from the judgement
    /// altogether. A run can raise either, both, or neither.
    TotalOnlyAttempts,
    /// Some attestation carries no readable usage record.
    UnreadableUsageRecord,
    /// Some attestation predates `usageEvidence`; its raw observation is not
    /// assumed fresh and is excluded from the sums.
    LegacyUsageContract,
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
    Accounted(Box<UsageEvidence>),
    /// A pre-schema raw observation. It stays visible on job detail but has no
    /// trustworthy attempt accounting meaning for a run sum.
    Legacy,
    /// The payload predates the usage record, or carries one this build cannot
    /// read.
    NoRecord,
}

fn has_readable_legacy_usage(payload: &Value) -> bool {
    payload
        .get("usage")
        .is_some_and(|value| serde_json::from_value::<UsageObservation>(value.clone()).is_ok())
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
            if evidence.schema_version == USAGE_EVIDENCE_SCHEMA_VERSION {
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

    Some(UsageEvidence {
        schema_version,
        declared_fields,
        counter_scope,
        observed,
        accounting,
    })
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
            let usage = accounted_usage(record, &records_by_sequence)
                .map(Box::new)
                .map_or_else(
                    || {
                        if has_readable_legacy_usage(payload) {
                            LedgerUsage::Legacy
                        } else {
                            LedgerUsage::NoRecord
                        }
                    },
                    LedgerUsage::Accounted,
                );
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
            LedgerUsage::Legacy => {
                coverage.attempts_legacy_usage += 1;
                caveats.insert(UsageRollupCaveat::LegacyUsageContract);
                continue;
            }
            LedgerUsage::Accounted(evidence) => {
                if evidence.accounting.state != UsageAccountingState::Exact {
                    coverage.attempts_accounting_unavailable += 1;
                    caveats.insert(UsageRollupCaveat::AccountingUnavailable);
                }
                if evidence.accounting.basis == UsageAccountingBasis::Delta
                    && matches!(
                        evidence.accounting.reason,
                        Some(
                            UsageAccountingReason::MissingPredecessor
                                | UsageAccountingReason::LegacyPredecessor
                        )
                    )
                {
                    caveats.insert(UsageRollupCaveat::CumulativeBaselineMissing);
                }
                &evidence.accounting.usage
            }
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
        // An attempt whose harness stated a total and reported no component
        // beside it declared no components to be missing. It is the one shape
        // the per-component threshold must not judge; every other reported
        // attempt is in that denominator, including one that reported a
        // component *and* a harness total, so drift cannot hide behind a
        // stated total.
        let states_only_a_harness_total = breakdown.shape == UsageShape::Lump
            && breakdown
                .total_tokens
                .is_some_and(|total| total.source == UsageTotalSource::HarnessReported);
        if !states_only_a_harness_total {
            coverage.attempts_reported_with_components += 1;
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
    // The completeness threshold. Every component the total is a sum of has to
    // have been reported by every attempt that was reporting components at
    // all; a component reported by fewer is a sum over a subset, which is
    // exactly what `UsageSum::attempts` has always meant and what nothing used
    // to read. Reasoning is not in this list on purpose — it enters no total,
    // and claude-code reports none, so checking it would fire on every claude
    // run.
    if [
        tokens.input_tokens,
        tokens.cache_read_tokens,
        tokens.cache_write_tokens,
        tokens.output_tokens,
    ]
    .iter()
    .any(|component| component.attempts < coverage.attempts_reported_with_components)
    {
        caveats.insert(UsageRollupCaveat::PartialComponents);
    }
    // The exemption above is decided from what an attempt reported, and a
    // total-only report is produced both by a legal total-only adapter and by
    // one whose component keys all drifted at once. In isolation those cannot
    // be told apart — but beside an attempt that did report components they do
    // not need to be: the component sums then cover strictly fewer attempts
    // than the total does, which is a fact about this run and not a guess
    // about its adapters.
    let attempts_reported_without_components = coverage
        .attempts_reported
        .saturating_sub(coverage.attempts_reported_with_components);
    if attempts_reported_without_components > 0 && coverage.attempts_reported_with_components > 0 {
        caveats.insert(UsageRollupCaveat::TotalOnlyAttempts);
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
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::adapters::{AdapterConfig, AdapterEngine, ScrapeCapture, ScrapeMode, ScrapeStream};
    use crate::usage::{
        observe, UsageBreakdown, UsageCost, UsageTotalTokens, FIELD_CACHE_READ_TOKENS,
        FIELD_CACHE_WRITE_TOKENS, FIELD_INPUT_TOKENS_WITH_CACHE_READ, FIELD_OUTPUT_TOKENS,
        FIELD_REASONING_TOKENS,
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

    fn attestation(seq: u64, task: &str, attempt: u64, usage: Value) -> AttestationRecord {
        let observed: UsageObservation =
            serde_json::from_value(usage.clone()).expect("test usage is readable");
        let usage_evidence = UsageEvidence {
            schema_version: USAGE_EVIDENCE_SCHEMA_VERSION,
            declared_fields: Vec::new(),
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
            [task],
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
        let rollup = roll_up([task], &AttestationEvidence::new(true, &records));

        assert_eq!(rollup.coverage.attempts_legacy_usage, 0);
        assert_eq!(rollup.tokens.input_tokens.value, 10_101);
        assert_eq!(rollup.tokens.cache_read_tokens.value, 22_016);
        assert_eq!(rollup.tokens.output_tokens.value, 11);
        assert_eq!(
            rollup.tokens.total_tokens.map(|total| total.value),
            Some(32_128)
        );
        assert!(!rollup
            .caveats
            .contains(&UsageRollupCaveat::LegacyUsageContract));
        assert!(!rollup
            .caveats
            .contains(&UsageRollupCaveat::AccountingUnavailable));
        assert!(!rollup
            .caveats
            .contains(&UsageRollupCaveat::CumulativeBaselineMissing));
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
        let rejected = roll_up([task], &AttestationEvidence::new(true, &unbound));
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
        let rollup = roll_up([task], &AttestationEvidence::new(true, &[record]));

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
    }

    #[test]
    fn legacy_raw_usage_is_visible_evidence_but_not_a_confident_rollup_charge() {
        let mut legacy = attestation(1, "task", 1, codex_usage());
        legacy
            .payload
            .as_object_mut()
            .unwrap()
            .remove("usageEvidence");
        let rollup = roll_up(["task"], &AttestationEvidence::new(true, &[legacy]));
        assert_eq!(rollup.coverage.attempts_legacy_usage, 1);
        assert_eq!(rollup.tokens.total_tokens, None);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::LegacyUsageContract));
        assert!(!rollup.is_complete());
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
    fn declared_total_usage() -> Value {
        let adapter = AdapterConfig {
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
        };
        observed_with(
            &adapter,
            "declared-total",
            r#"{"type":"turn.completed","usage":{"total_tokens":120}}"#,
        )
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
        // The real path's shape for a total-only mapping: a lump, with every
        // component absent because none was ever declared.
        let observation = declared_total_usage();
        assert_eq!(observation["breakdown"]["shape"], "lump");
        assert!(observation["breakdown"]["inputTokens"].is_null());

        let declared = [attestation(1, "declared", 1, observation)];
        let harness = roll_up(["declared"], &AttestationEvidence::new(true, &declared));
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
        // No attempt of this run reported components, so no component sum
        // covers a subset of anything: the beside-ness the total-only caveat
        // needs does not exist here.
        assert!(!harness
            .caveats
            .contains(&UsageRollupCaveat::TotalOnlyAttempts));

        // And a run mixing that adapter with a preset is where `mixed` — and
        // its caveat — actually becomes reachable. The preset attempt is the
        // only one the component threshold judges, and it reported everything,
        // so no component is missing from within that judgement — but the
        // component sums do cover one of the run's two attempts, and that is
        // the total-only caveat's job to say.
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
        assert_eq!(mixed.coverage.attempts_reported, 2);
        assert_eq!(mixed.coverage.attempts_reported_with_components, 1);
        assert!(mixed
            .caveats
            .contains(&UsageRollupCaveat::MixedTotalAuthority));
        assert!(!mixed
            .caveats
            .contains(&UsageRollupCaveat::PartialComponents));
        assert!(mixed
            .caveats
            .contains(&UsageRollupCaveat::TotalOnlyAttempts));

        // But an exempted attempt hides nothing from the threshold either: a
        // drifted preset attempt beside the total-only one is still caught.
        let drifted = CODEX_STREAM.replace("cached_input_tokens", "cached_input_tokens_v2");
        let records = [
            attestation(1, "declared", 1, declared_total_usage()),
            attestation(
                2,
                "preset",
                1,
                observed_with(&codex_preset(), "codex", &drifted),
            ),
        ];
        let hidden = roll_up(
            ["declared", "preset"],
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
    /// That attempt reports the same shape a legal total-only adapter does —
    /// a lump with a harness-stated total — so it leaves the component
    /// threshold's denominator, and this shape-based compatibility projection
    /// does not use the declaration to tell the two apart. What it can see is that the
    /// component sums now cover strictly fewer attempts than the total does,
    /// which is a fact about this run. Without the total-only caveat this run
    /// graded `complete` with an empty caveat list while half its tokens were
    /// missing from every component sum.
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
            attestation(1, "task", 1, honest),
            attestation(2, "task", 2, after),
        ];
        let rollup = roll_up(["task"], &AttestationEvidence::new(true, &records));

        assert_eq!(rollup.coverage.attempts_reported, 2);
        assert_eq!(
            rollup.coverage.attempts_reported_with_components, 1,
            "the drifted attempt reported no component, so it left the denominator"
        );
        assert_eq!(rollup.coverage.attempts_reported_without_figures, 0);
        // Half the run's tokens are absent from every component sum...
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
        assert_eq!(rollup.tokens.total_tokens.unwrap().value, 905_120 * 2);
        // ...and nothing within the judged attempt is missing, so the
        // per-component threshold is silent. Only the caveat that counts
        // attempts *outside* it can speak here.
        assert!(!rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialComponents));
        assert!(
            rollup
                .caveats
                .contains(&UsageRollupCaveat::TotalOnlyAttempts),
            "a component sum over half the run must not grade complete: {:?}",
            rollup.caveats
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
            ["task"],
            &AttestationEvidence::new(true, &[attestation(1, "task", 1, observed(CLAUDE_STREAM))]),
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
            ["task"],
            &AttestationEvidence::new(true, &[attestation(1, "task", 1, observed(&drifted))]),
        );
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(
            rollup.coverage.attempts_reported_without_figures, 0,
            "the other declared keys still resolved, so this is not total drift"
        );
        assert_eq!(rollup.tokens.input_tokens.attempts, 1);
        assert_eq!(rollup.tokens.cache_read_tokens.attempts, 0);
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
                ["task"],
                &AttestationEvidence::new(true, &[attestation(1, "task", 1, observed(&drifted))]),
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

    /// The same drift on codex, where the total stays right **by accident** and
    /// the caveat is the only thing that betrays it.
    ///
    /// codex's `input_tokens` is cache-inclusive, so `observe` derives the
    /// exclusive figure by subtracting the cache read; an absent cache read
    /// subtracts zero. The derived total therefore lands on the same number it
    /// would have without the drift, while `inputTokens` and `freshInputTokens`
    /// are both reported at the cache-inclusive figure — 26.9× the truth. No
    /// number on the wire looks wrong; the coverage count is what says so.
    #[test]
    fn a_drifted_codex_key_leaves_the_total_right_and_the_input_figure_wrong() {
        let honest = roll_up(
            ["task"],
            &AttestationEvidence::new(
                true,
                &[attestation(
                    1,
                    "task",
                    1,
                    observed_with(&codex_preset(), "codex", CODEX_STREAM),
                )],
            ),
        );
        assert!(honest.is_complete(), "unexpected: {:?}", honest.caveats);
        assert_eq!(honest.tokens.input_tokens.value, 262_086);
        assert_eq!(honest.tokens.total_tokens.unwrap().value, 7_093_008);

        let drifted = CODEX_STREAM.replace("cached_input_tokens", "cached_input_tokens_v2");
        let rollup = roll_up(
            ["task"],
            &AttestationEvidence::new(
                true,
                &[attestation(
                    1,
                    "task",
                    1,
                    observed_with(&codex_preset(), "codex", &drifted),
                )],
            ),
        );
        assert_eq!(
            rollup.tokens.total_tokens.unwrap().value,
            7_093_008,
            "the total is unchanged, so it cannot be what warns anyone"
        );
        assert_eq!(
            rollup.tokens.input_tokens.value, 7_060_166,
            "the cache-inclusive figure is promoted to the exclusive one"
        );
        assert_eq!(rollup.tokens.fresh_input_tokens.attempts_partial, 0);
        assert_eq!(rollup.tokens.cache_read_tokens.attempts, 0);
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
            ["task"],
            &AttestationEvidence::new(true, &[attestation(1, "task", 1, observation)]),
        );
        assert_eq!(rollup.coverage.attempts_reported, 1);
        assert_eq!(rollup.coverage.attempts_reported_without_figures, 0);
        assert_eq!(rollup.tokens, UsageTokenRollup::default());
        assert_eq!(rollup.cost.attempts, 1);
        assert!(rollup
            .caveats
            .contains(&UsageRollupCaveat::PartialComponents));
        assert!(!rollup.is_complete());
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
        record
            .payload
            .as_object_mut()
            .expect("payload is an object")
            .remove("usageEvidence");
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
