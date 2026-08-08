use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::flow_lineage::{FlowLineage, FlowSupersedeRecord};
use crate::flow_membership::FlowMembership;
use crate::history::{LifecycleRecord, LifecycleSnapshot, RetentionMetadata};
use crate::journal::TallyEvent;
use crate::occupancy::{ContextWindow, ContextWindowSource};
use crate::provenance::{Orchestration, TaskRef};
use crate::query::{
    GhOriginProjection, HeadroomSignal, RowStatus, StandupDigest, StandupRunUsage,
    StandupUsageBasis, QUERY_PROTOCOL_VERSION, QUERY_SCHEMA_VERSION,
};
use crate::reader_state::ReaderState;
use crate::taskdb::{
    related_trigger_from_gh_origin, AdmissionOrigin, ProducerOrigin, RelatedTrigger, RowSeed,
    WorkspaceMetadata,
};
use crate::usage::UsageObservation;
use crate::usage_rollup::{roll_up, AttestationEvidence, UsageRollup};
use crate::witness::{
    counts_toward_canonical_gpu_seconds, AttestationRecord, AuthorshipSession, AuthorshipStatus,
    Charge, LaborClass, TerminalError, Verdict, VerifyReport, WitnessRecord,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FactAuthority {
    DurableAdmissionFact,
    TallyLifecycleObservation,
    CanonicalWitnessFact,
    AdvisoryAttestation,
    AdvisoryProviderCapture,
    /// A value read from the daemon's live adapter configuration rather than
    /// from a durable row, a witness, or a provider's own captured stream.
    /// Unlike `DurableAdmissionFact` it does not survive a restart (nothing
    /// persists it — see `RowSeed.context_window`'s transport-only
    /// comment), and unlike `AdvisoryProviderCapture` it was never stated by
    /// the harness that ran the attempt. It is the operator's own assertion,
    /// advisory like the rest of this family.
    AdvisoryConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SourcedValue<T> {
    pub value: T,
    pub authority: FactAuthority,
    pub provenance: String,
}

impl<T> SourcedValue<T> {
    fn new(value: T, authority: FactAuthority, provenance: &str) -> Self {
        Self {
            value,
            authority,
            provenance: provenance.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowDetailFact {
    pub task_uuid: String,
    pub description: String,
    pub argv: Vec<String>,
    pub dedup_key: Option<String>,
    pub brief_hash: Option<String>,
    pub orchestration: Option<Orchestration>,
    pub row_status: RowStatus,
    pub priority: String,
    pub pools: Vec<String>,
    pub executor: Option<String>,
    pub adapter: String,
    pub source: String,
    pub requested_model: Option<String>,
    pub observed_model: Option<String>,
    pub session_ref: Option<String>,
    pub final_message: Option<String>,
    pub usage: Option<UsageObservation>,
    pub context_tokens: Option<u64>,
    pub context_window: Option<ContextWindow>,
    pub workspace: Option<WorkspaceMetadata>,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub labor_class: LaborClass,
    pub parent_task_uuid: Option<String>,
    pub evidence_specs: Vec<String>,
    pub consumption_estimate: Option<u64>,
    pub runtime_max_sec: Option<u64>,
    pub no_enqueue: bool,
    pub credential_names: Vec<String>,
    pub evidence_class: Option<Value>,
    pub manifest_hash: Option<Value>,
    pub origin: OriginProjection,
    pub related_trigger: Option<RelatedTrigger>,
}

impl RowDetailFact {
    pub fn from_seed(row: &RowSeed, row_status: RowStatus, labor_class: LaborClass) -> Self {
        let fallback_origin;
        let origin = if let Some(origin) = row.origin.as_ref() {
            origin
        } else {
            fallback_origin = row.gh_origin.as_ref().map_or_else(
                || AdmissionOrigin::direct(row.source),
                |github| AdmissionOrigin::github(&github.producer, github.clone()),
            );
            &fallback_origin
        };
        Self {
            task_uuid: row.uuid.to_string(),
            description: row.description.clone(),
            argv: row.argv.clone(),
            dedup_key: row.dedup_key.clone(),
            brief_hash: row.brief_hash.clone(),
            orchestration: row.orchestration.clone(),
            row_status,
            priority: priority_name(row.priority).to_owned(),
            pools: row.pools.clone(),
            executor: row.executor.clone(),
            adapter: row.adapter.clone(),
            source: source_name(row.source).to_owned(),
            requested_model: row.adapter_options.model.clone(),
            observed_model: row.model.clone(),
            session_ref: row.session_ref.clone(),
            final_message: row.final_message.clone(),
            usage: row.usage.clone(),
            context_tokens: row.context_tokens,
            context_window: row.context_window,
            workspace: row.workspace.clone(),
            attempt: row.attempt,
            lease_epoch: row.lease_epoch,
            labor_class,
            parent_task_uuid: row.parent_uuid.map(|uuid| uuid.to_string()),
            evidence_specs: row.evidence.clone(),
            consumption_estimate: row.consumption_estimate,
            runtime_max_sec: row.runtime_max_sec,
            no_enqueue: row.no_enqueue,
            credential_names: row.credentials.keys().cloned().collect(),
            evidence_class: row.evidence_class.clone(),
            manifest_hash: row.manifest_hash.clone(),
            origin: OriginProjection::from_admission(origin),
            related_trigger: row.related_trigger.clone().or_else(|| {
                row.gh_origin
                    .as_ref()
                    .and_then(|origin| related_trigger_from_gh_origin(origin).ok())
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OriginProjection {
    pub source: String,
    pub producer: Option<ProducerOrigin>,
    pub github: Option<GhOriginProjection>,
}

impl OriginProjection {
    fn from_admission(origin: &AdmissionOrigin) -> Self {
        Self {
            source: origin.source.as_str().to_owned(),
            producer: origin.producer.clone(),
            github: origin
                .github
                .as_ref()
                .and_then(GhOriginProjection::from_origin),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveJobFact {
    pub anchor: String,
    pub job_id: String,
    pub live_state: String,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub unit: String,
    pub labor_class: LaborClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PoolSignalProjection {
    pub pool: String,
    pub signal: HeadroomSignal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceResult {
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JobTimestamps {
    pub enqueued_at: Option<String>,
    pub dispatched_at: Option<String>,
    pub started_at: Option<String>,
    pub last_event_at: Option<String>,
    pub terminal_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TraceAvailability {
    pub available: bool,
    pub complete: bool,
    pub byte_count: Option<u64>,
    pub retained_range: Option<String>,
    pub truncation: Option<String>,
    pub reason: String,
}

impl Default for TraceAvailability {
    fn default() -> Self {
        Self {
            available: false,
            complete: false,
            byte_count: None,
            retained_range: None,
            truncation: None,
            reason: "trace-capability-not-evaluated".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TerminationProjection {
    pub verdict: Verdict,
    pub exit_code: i32,
    pub authority: FactAuthority,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuthorshipProjection {
    pub status: AuthorshipStatus,
    pub provider: String,
    pub provider_version: String,
    pub result_revision: String,
    pub note_ref: String,
    pub notes_ref_target: Option<String>,
    pub note_content_sha256: Option<String>,
    pub reason: Option<String>,
    pub identity_mismatch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<SourcedValue<WorkspaceMetadata>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tally_session: Option<SourcedValue<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tally_model: Option<SourcedValue<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub git_ai_sessions: Vec<SourcedValue<AuthorshipSession>>,
}

/// How a durable row came to exist, in the daemon's own enqueue vocabulary.
///
/// A flow node's replay is only verifiable if the operator can read the answer
/// the daemon gave, so the projection reports it rather than making the reader
/// translate `laborClass`. Only admissions that write a row appear here:
/// `attached` and full-mode `reused` and `terminal` answers reuse an existing
/// row or a governing witness and write none, so those show up in the flow
/// runner's `node-submitted` and `node-terminal` lifecycle events instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RowDisposition {
    Created,
    Reused,
    Substituted,
}

impl RowDisposition {
    const fn from_labor_class(labor_class: LaborClass) -> Self {
        match labor_class {
            // A recovered row was still created by its original admission;
            // recovery is a property of the attempt, not of the admission.
            LaborClass::Fresh | LaborClass::Recovered => Self::Created,
            LaborClass::Reused => Self::Reused,
            LaborClass::Substituted => Self::Substituted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JobSummary {
    pub anchor: String,
    pub task_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<TaskRef>,
    pub live_job_id: Option<String>,
    pub description: Option<String>,
    pub argv: Vec<String>,
    pub dedup_key: Option<String>,
    pub disposition: Option<RowDisposition>,
    pub brief_hash: Option<String>,
    pub orchestration: Option<Orchestration>,
    pub row_status: Option<RowStatus>,
    pub live_state: Option<String>,
    pub terminal_verdict: Option<Verdict>,
    pub terminal_attempt: Option<u32>,
    pub evidence_result: Option<EvidenceResult>,
    pub lifecycle_event: Option<TallyEvent>,
    pub pool_signals: Vec<PoolSignalProjection>,
    pub priority: Option<String>,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize_optional",
        deserialize_with = "crate::poolset::deserialize_optional"
    )]
    pub pools: Option<Vec<String>>,
    pub executor: Option<String>,
    pub adapter: Option<String>,
    pub source: Option<String>,
    pub origin: Option<SourcedValue<OriginProjection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_trigger: Option<RelatedTrigger>,
    pub model: Vec<SourcedValue<String>>,
    pub session_ref: Option<SourcedValue<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message: Option<SourcedValue<String>>,
    /// Normalized per-attempt usage. Advisory throughout: it is what the
    /// harness said about itself, never a tally measurement, and its three
    /// states stay distinct on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<SourcedValue<UsageObservation>>,
    /// Occupancy as of the attempt's last valid assistant turn: the same
    /// total `usage` already normalizes, read under its occupancy meaning.
    /// Independent of `context_window` — a session can report how full it
    /// is without anyone stating how full it can get.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<SourcedValue<u64>>,
    /// The ceiling `context_tokens` is measured against. Two provenances
    /// stay distinguishable on the wire: `advisory-provider-capture` for a
    /// harness that stated its own window, `durable-admission-fact` for an
    /// operator-declared ceiling in adapter configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<SourcedValue<u64>>,
    pub current_attempt: Option<u32>,
    pub lease_epoch: Option<u64>,
    pub unit: Option<String>,
    pub labor_class: Option<LaborClass>,
    pub parent_task_uuid: Option<String>,
    pub child_task_uuids: Vec<String>,
    pub timestamps: JobTimestamps,
    pub wall_clock_seconds: Option<f64>,
    pub runtime_max_sec: Option<u64>,
    pub consumption_estimate: Option<u64>,
    pub no_enqueue: Option<bool>,
    pub evidence_specs: Vec<String>,
    pub evidence_class: Option<Value>,
    pub manifest_hash: Option<Value>,
    pub artifact_content_hash: Option<String>,
    pub exit_code: Option<i32>,
    pub termination: Option<TerminationProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorship: Option<AuthorshipProjection>,
    pub gpu_seconds: Option<f64>,
    pub charge: Option<Charge>,
    pub canonical_gpu_seconds: Option<f64>,
    pub credential_names: Vec<String>,
    pub trace: TraceAvailability,
    /// True when the flow run that created this job (per its orchestration
    /// capsule) is archived reader-state. In an explicit `flowRun` lookup it
    /// is also true when the selected run is archived, including for a
    /// membership-only summary that has no orchestration capsule of its own.
    /// Set post-projection by [`apply_reader_state_to_jobs`], which owns the
    /// reader-state store this projection has no access to; an unfilled summary
    /// carries `false` rather than a wrong value.
    #[serde(default)]
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QueryChainHead {
    pub seq: u64,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct QuerySnapshotMetadata {
    pub created_at: String,
    pub cursor: Option<String>,
    pub history: RetentionMetadata,
    pub witness_head: QueryChainHead,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CollectionEnvelope<T> {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    /// Durable stream position of this response, for collections that have
    /// one. Unlike `next_cursor` it survives daemon restarts and page-cache
    /// eviction, so a monitor can hold it between polls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
    /// Set when a requested `after` position predates what durable history
    /// still retains: events between the retained floor and the request are
    /// gone, and the response is therefore not a complete continuation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_gap: Option<PositionGap>,
    /// How many task UUIDs a `flowRun` filter resolved to, present only when
    /// one was supplied.
    ///
    /// Membership is a durable admission fact as of #380: every admission under
    /// a `flowRunId` records `(run, task)` in the membership ledger before it is
    /// acknowledged, including the row-less dispositions — `attached`, and
    /// full-mode `reused` and `terminal` — that used to hand a run a task UUID
    /// it could never see in its own window. `flow_run_tasks` resolves that
    /// ledger unioned with the rows and witnesses carrying the run's capsule,
    /// so a run submitted before that ledger existed still resolves exactly as
    /// it did.
    ///
    /// The count remains reported for a different reason than it was
    /// introduced: it is still the difference between a window that is empty
    /// because the run is quiet and one that is empty because the run has no
    /// members at all — which now means the run really admitted nothing, rather
    /// than that its nodes went missing.
    ///
    /// An explicit `flowRun` filter is a by-ID inspection, so reader-state
    /// never withholds its jobs: an archived member remains in `items` with
    /// `archived: true`. Consequently `flowRunTasks: N, items: []` retains its
    /// original meaning: the run has members, but none matched the remaining
    /// job filters or window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_run_tasks: Option<usize>,
    pub snapshot: QuerySnapshotMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PositionGap {
    pub requested: String,
    pub earliest_available: String,
}

/// A durable coordinate in the lifecycle log stream.
///
/// `query.log` merges two append-only durable streams — the lifecycle history
/// (`history.rs`) and the witness ledger — so one sequence number cannot name
/// a position in it. Both components are monotone, which is what makes the
/// position durable where a page cursor is not: it survives a daemon restart
/// and page-cache eviction, and an external monitor can hold it between polls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogPosition {
    pub lifecycle: u64,
    pub witness: u64,
}

pub const LOG_POSITION_PREFIX: &str = "log-v1";

impl LogPosition {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "{LOG_POSITION_PREFIX}:{:020}:{:020}",
            self.lifecycle, self.witness
        )
    }

    /// Parse a position emitted by a previous response. Page cursors and watch
    /// cursors are rejected by name rather than silently misread: they are
    /// ephemeral and would make a monitor's window wrong.
    pub fn parse(value: &str) -> Result<Self, ObservabilityError> {
        let mut parts = value.split(':');
        let version = parts.next();
        let lifecycle = parts.next().and_then(|part| part.trim().parse().ok());
        let witness = parts.next().and_then(|part| part.trim().parse().ok());
        if version != Some(LOG_POSITION_PREFIX)
            || lifecycle.is_none()
            || witness.is_none()
            || parts.next().is_some()
        {
            return Err(ObservabilityError::InvalidPosition(value.to_owned()));
        }
        Ok(Self {
            lifecycle: lifecycle.unwrap(),
            witness: witness.unwrap(),
        })
    }

    /// Whether a lifecycle item, identified by its durable `cursor`, is newer
    /// than this position. An unrecognised cursor is treated as newer: a
    /// monitor may then see an item twice, never miss one.
    #[must_use]
    pub fn precedes(&self, cursor: &str) -> bool {
        match sequence_after_prefix(cursor, "lifecycle:") {
            Some(sequence) => return sequence > self.lifecycle,
            None => {
                if let Some(sequence) = sequence_after_prefix(cursor, "witness:") {
                    return sequence > self.witness;
                }
            }
        }
        true
    }
}

fn sequence_after_prefix(cursor: &str, prefix: &str) -> Option<u64> {
    cursor.strip_prefix(prefix)?.trim().parse().ok()
}

/// The head of the lifecycle log stream at projection time. Reporting the head
/// rather than the newest matched item is what makes `--after` + empty items
/// mean "provably quiet" for a filtered query.
#[must_use]
pub fn log_position_head(history: &LifecycleSnapshot, witness: &[WitnessRecord]) -> LogPosition {
    LogPosition {
        lifecycle: history
            .records
            .last()
            .map(|record| record.sequence)
            .or_else(|| {
                history
                    .retention
                    .latest_cursor
                    .as_deref()
                    .and_then(|cursor| sequence_after_prefix(cursor, "lifecycle:"))
            })
            .unwrap_or(0),
        witness: witness.last().map_or(0, |record| record.seq),
    }
}

/// The oldest position a caller can resume from without a gap: one below the
/// earliest record each durable stream still retains.
#[must_use]
pub fn log_position_floor(history: &LifecycleSnapshot, witness: &[WitnessRecord]) -> LogPosition {
    LogPosition {
        lifecycle: history
            .records
            .first()
            .map_or(0, |record| record.sequence.saturating_sub(1)),
        witness: witness
            .first()
            .map_or(0, |record| record.seq.saturating_sub(1)),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobsFilter {
    pub live_state: Option<String>,
    pub terminal_verdict: Option<Verdict>,
    pub pool: Option<String>,
    pub executor: Option<String>,
    pub adapter: Option<String>,
    pub source: Option<String>,
    pub origin: Option<String>,
    pub parent: Option<String>,
    pub flow_run: Option<String>,
    pub session: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

#[derive(Debug, Error)]
pub enum ObservabilityError {
    #[error("invalid query timestamp {0:?}")]
    InvalidTimestamp(String),
    #[error("unknown job {0:?}")]
    UnknownJob(String),
    #[error("job {task:?} has no attempt {attempt}")]
    UnknownAttempt { task: String, attempt: u32 },
    #[error("flow run {0:?} has an invalid reconciliation projection")]
    InvalidRunProjection(String),
    #[error(
        "invalid lifecycle stream position {0:?}; use the `position` field of a previous \
         query.log response, not a page or watch cursor"
    )]
    InvalidPosition(String),
}

#[allow(clippy::too_many_arguments)]
pub fn query_jobs(
    details: &[RowDetailFact],
    live: &[LiveJobFact],
    history: &LifecycleSnapshot,
    witness: &[WitnessRecord],
    pool_signals: &BTreeMap<String, HeadroomSignal>,
    filter: &JobsFilter,
    membership: &FlowMembership,
) -> Result<CollectionEnvelope<JobSummary>, ObservabilityError> {
    let since = filter.since.as_deref().map(parse_timestamp).transpose()?;
    let until = filter.until.as_deref().map(parse_timestamp).transpose()?;
    // A `flowRun` filter selects on run membership, which a row-less admission
    // does not put on the row: resolve it once, the same way `query.log` does.
    let flow_tasks = filter
        .flow_run
        .as_deref()
        .map(|flow_run| flow_run_tasks(flow_run, details, witness, membership));
    let children = child_index(details);
    let mut anchors = BTreeSet::new();
    anchors.extend(details.iter().map(|detail| detail.task_uuid.clone()));
    anchors.extend(live.iter().map(|fact| fact.anchor.clone()));
    anchors.extend(
        history
            .records
            .iter()
            .map(|record| record.fields.task_uuid.clone()),
    );
    anchors.extend(witness.iter().map(|record| {
        record
            .task_uuid
            .clone()
            .unwrap_or_else(|| format!("witness:{}", record.seq))
    }));
    // An explicit run lookup is also an identity lookup for every UUID in its
    // durable membership ledger. A row-less member can have no detail,
    // lifecycle, live, or witness fact left in this projection; omitting the
    // membership set from `anchors` would therefore report `flowRunTasks: N`
    // beside fewer than N items even when no other filter was supplied.
    if let Some(tasks) = &flow_tasks {
        anchors.extend(tasks.iter().cloned());
    }

    let detail_by_task = details
        .iter()
        .map(|detail| (detail.task_uuid.as_str(), detail))
        .collect::<BTreeMap<_, _>>();
    let live_by_task = live
        .iter()
        .map(|fact| (fact.anchor.as_str(), fact))
        .collect::<BTreeMap<_, _>>();
    // Grouped in one pass each. Filtering the whole history and ledger once
    // per anchor made this collection O(anchors x (records + witnesses)) --
    // at estate scale (~30k rows, ~150k lifecycle records) that is minutes of
    // CPU for one call, and it used to run on the daemon's dispatch thread
    // (#431). Iteration order within a group is preserved, so per-anchor
    // consumers still see records in ledger order.
    let mut events_by_task = HashMap::<&str, Vec<&LifecycleRecord>>::new();
    for record in &history.records {
        events_by_task
            .entry(record.fields.task_uuid.as_str())
            .or_default()
            .push(record);
    }
    let mut witness_by_anchor = HashMap::<String, Vec<&WitnessRecord>>::new();
    for record in witness {
        let anchor = record
            .task_uuid
            .clone()
            .unwrap_or_else(|| format!("witness:{}", record.seq));
        witness_by_anchor.entry(anchor).or_default().push(record);
    }
    let mut items = Vec::new();
    for anchor in anchors {
        let events = events_by_task
            .get(anchor.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let witnesses = witness_by_anchor
            .get(anchor.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut summary = build_summary(
            &anchor,
            detail_by_task.get(anchor.as_str()).copied(),
            live_by_task.get(anchor.as_str()).copied(),
            events,
            witnesses,
            children.get(&anchor).cloned().unwrap_or_default(),
            pool_signals,
        );
        // Membership is itself the durable fact that this anchor is a task
        // UUID. Preserve that identity on a skeletal summary even when all of
        // the task's other observable facts are absent.
        if flow_tasks
            .as_ref()
            .is_some_and(|tasks| tasks.contains(&anchor))
        {
            summary.task_uuid = Some(anchor.clone());
        }
        if matches_jobs_filter(&summary, filter, flow_tasks.as_ref(), since, until) {
            items.push(summary);
        }
    }
    Ok(CollectionEnvelope {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        items,
        next_cursor: None,
        position: None,
        position_gap: None,
        flow_run_tasks: flow_tasks.map(|tasks| tasks.len()),
        snapshot: snapshot_metadata(history, witness),
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EvidenceObservation {
    pub event_id: String,
    pub cursor: String,
    pub timestamp: String,
    pub spec: String,
    pub passed: bool,
    pub message: String,
    pub authority: FactAuthority,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AttemptProjection {
    pub attempt: Option<u32>,
    pub lease_epoch: Option<u64>,
    pub events: Vec<LifecycleEventProjection>,
    pub evidence_result: Option<EvidenceResult>,
    pub evidence_observations: Vec<EvidenceObservation>,
    pub witness_records: Vec<WitnessRecord>,
    pub timestamps: JobTimestamps,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct JobDetail {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub job: JobSummary,
    pub attempts: Vec<AttemptProjection>,
    pub snapshot: QuerySnapshotMetadata,
}

pub fn query_job(
    task_or_job_id: &str,
    details: &[RowDetailFact],
    live: &[LiveJobFact],
    history: &LifecycleSnapshot,
    witness: &[WitnessRecord],
    pool_signals: &BTreeMap<String, HeadroomSignal>,
) -> Result<JobDetail, ObservabilityError> {
    let is_task_anchor = details
        .iter()
        .any(|detail| detail.task_uuid == task_or_job_id)
        || history
            .records
            .iter()
            .any(|record| record.fields.task_uuid == task_or_job_id)
        || witness
            .iter()
            .any(|record| record.task_uuid.as_deref() == Some(task_or_job_id));
    let anchor = if is_task_anchor {
        task_or_job_id
    } else {
        live.iter()
            .find(|fact| fact.job_id == task_or_job_id)
            .map(|fact| fact.anchor.as_str())
            .or_else(|| {
                history
                    .records
                    .iter()
                    .rev()
                    .find(|record| record.fields.job_id.as_deref() == Some(task_or_job_id))
                    .map(|record| record.fields.task_uuid.as_str())
            })
            .unwrap_or(task_or_job_id)
    };
    let collection = query_jobs(
        details,
        live,
        history,
        witness,
        pool_signals,
        &JobsFilter::default(),
        &FlowMembership::default(),
    )?;
    let job = collection
        .items
        .into_iter()
        .find(|job| job.anchor == anchor)
        .ok_or_else(|| ObservabilityError::UnknownJob(task_or_job_id.to_owned()))?;
    let events = history
        .records
        .iter()
        .filter(|record| record.fields.task_uuid == anchor)
        .collect::<Vec<_>>();
    let witnesses = witness
        .iter()
        .filter(|record| record.task_uuid.as_deref() == Some(anchor))
        .collect::<Vec<_>>();
    let attempts = attempt_projections(&events, &witnesses);
    Ok(JobDetail {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        job,
        attempts,
        snapshot: collection.snapshot,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LifecycleEventProjection {
    pub origin: String,
    pub event_id: String,
    pub cursor: String,
    pub timestamp: String,
    pub event: TallyEvent,
    pub task_uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<TaskRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_label: Option<String>,
    pub attempt: Option<u32>,
    pub lease_epoch: Option<u64>,
    pub adapter: Option<String>,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize_optional",
        deserialize_with = "crate::poolset::deserialize_optional"
    )]
    pub pools: Option<Vec<String>>,
    pub executor: Option<String>,
    pub unit: Option<String>,
    pub job_id: Option<String>,
    pub parent_task_uuid: Option<String>,
    pub exit_code: Option<i32>,
    pub stderr_tail: Option<String>,
    pub stderr_truncated: Option<bool>,
    pub labor_class: Option<LaborClass>,
    pub evidence_result: Option<EvidenceResult>,
    pub evidence_spec: Option<String>,
    pub session_ref: Option<String>,
    pub source: String,
    pub gpu_seconds: Option<f64>,
    pub artifact_hash: Option<String>,
    pub evidence_class: Option<Value>,
    pub manifest_hash: Option<Value>,
    pub message: String,
    pub authority: FactAuthority,
    pub provenance: String,
    pub witness_seq: Option<u64>,
    pub terminal_verdict: Option<Verdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_clock_seconds: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecycleLogFilter {
    pub task: Option<String>,
    pub flow_run: Option<String>,
    pub attempt: Option<u32>,
    pub session: Option<String>,
    pub event: Option<TallyEvent>,
    pub source: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

pub fn query_lifecycle_log(
    details: &[RowDetailFact],
    history: &LifecycleSnapshot,
    witness: &[WitnessRecord],
    filter: &LifecycleLogFilter,
    membership: &FlowMembership,
) -> Result<CollectionEnvelope<LifecycleEventProjection>, ObservabilityError> {
    let since = filter.since.as_deref().map(parse_timestamp).transpose()?;
    let until = filter.until.as_deref().map(parse_timestamp).transpose()?;
    let flow_tasks = filter
        .flow_run
        .as_deref()
        .map(|flow_run| flow_run_tasks(flow_run, details, witness, membership));
    let mut ordered = Vec::<(DateTime<Utc>, u8, u64, LifecycleEventProjection)>::new();
    for record in &history.records {
        let projection = lifecycle_projection(record);
        if lifecycle_matches(&projection, filter, flow_tasks.as_ref(), since, until) {
            ordered.push((
                parse_timestamp(&projection.timestamp)?,
                0,
                record.sequence,
                projection,
            ));
        }
    }
    for record in witness {
        let projection = witness_lifecycle_projection(record);
        if lifecycle_matches(&projection, filter, flow_tasks.as_ref(), since, until) {
            ordered.push((
                parse_timestamp(&projection.timestamp)?,
                1,
                record.seq,
                projection,
            ));
        }
    }
    // Label resolution is a scan of every witness and detail row, so it runs
    // once over the whole corpus and only for records that survived the
    // filter -- never once per candidate record, which made a `--task <uuid>`
    // query cost O(records x (witnesses + details)) on the daemon thread.
    if !ordered.is_empty() {
        let labels = NodeLabelIndex::build(details, witness);
        for entry in &mut ordered {
            entry.3.node_label = labels.lookup(&entry.3.task_uuid);
        }
    }
    ordered.sort_by(|left, right| {
        (left.0, left.1, left.2, &left.3.event_id).cmp(&(
            right.0,
            right.1,
            right.2,
            &right.3.event_id,
        ))
    });
    Ok(CollectionEnvelope {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        items: ordered
            .into_iter()
            .map(|(_, _, _, projection)| projection)
            .collect(),
        next_cursor: None,
        position: None,
        // Membership never touches this. `positionGap` is decided in the RPC
        // layer by comparing the caller's `after` against the retained floor of
        // durable history, not against the filtered window, so a run whose
        // membership just became complete still reports the same gap it
        // reported before. "No gap detected" keeps meaning history is intact,
        // not that the window happens to be reachable.
        position_gap: None,
        // Reported whenever the caller scoped the query to a run, including
        // when it resolved to nothing: a zero here is the difference between
        // a quiet run and a run that admitted nothing at all.
        flow_run_tasks: flow_tasks.map(|tasks| tasks.len()),
        snapshot: snapshot_metadata(history, witness),
    })
}

/// Collapse the two durable representations of a terminal transition into
/// one compact item. The journal record keeps its event/timestamp identity;
/// canonical verdict and artifact fields come from the matching witness.
/// Evidence observations remain available to explicit evidence queries but
/// are omitted from the ordinary transition view.
pub fn collapse_lifecycle_echoes(
    mut envelope: CollectionEnvelope<LifecycleEventProjection>,
    suppress_evidence: bool,
) -> CollectionEnvelope<LifecycleEventProjection> {
    type EchoKey = (String, Option<u32>, Option<u64>);
    let key =
        |item: &LifecycleEventProjection| (item.task_uuid.clone(), item.attempt, item.lease_epoch);
    // Items arrive in transition order, so the last journal terminal for a key
    // is its newest. A key can carry more than one -- `preempted` followed by
    // `failed` -- and merging the canonical verdict into every one of them
    // reports the same outcome twice. Only the newest absorbs the witness;
    // the earlier transitions stay in the log as themselves.
    let mut merge_targets = BTreeMap::<EchoKey, String>::new();
    for item in &envelope.items {
        if item.origin == "journal" && terminal_event(item.event) {
            merge_targets.insert(key(item), item.event_id.clone());
        }
    }
    // First witness per key wins the merge. Any further witness sharing the
    // key is not the one folded in, so it survives as its own row rather than
    // being dropped by a silent overwrite.
    let mut witnesses = BTreeMap::<EchoKey, LifecycleEventProjection>::new();
    for item in &envelope.items {
        if item.origin == "witness" {
            witnesses.entry(key(item)).or_insert_with(|| item.clone());
        }
    }

    envelope.items = envelope
        .items
        .into_iter()
        .filter_map(|mut item| {
            if suppress_evidence && item.event.is_evidence() {
                return None;
            }
            let item_key = key(&item);
            if item.origin == "witness"
                && merge_targets.contains_key(&item_key)
                && witnesses
                    .get(&item_key)
                    .is_some_and(|merged| merged.event_id == item.event_id)
            {
                return None;
            }
            if item.origin == "journal"
                && terminal_event(item.event)
                && merge_targets.get(&item_key) == Some(&item.event_id)
            {
                if let Some(witness) = witnesses.get(&item_key) {
                    item.origin = "journal+witness".to_owned();
                    item.task_ref = item.task_ref.or_else(|| witness.task_ref.clone());
                    item.node_label = item.node_label.or_else(|| witness.node_label.clone());
                    item.pools = item.pools.or_else(|| witness.pools.clone());
                    item.executor = item.executor.or_else(|| witness.executor.clone());
                    item.exit_code = witness.exit_code.or(item.exit_code);
                    item.labor_class = witness.labor_class.or(item.labor_class);
                    item.gpu_seconds = witness.gpu_seconds.or(item.gpu_seconds);
                    item.artifact_hash = witness
                        .artifact_hash
                        .clone()
                        .or_else(|| item.artifact_hash.clone());
                    item.evidence_class = witness
                        .evidence_class
                        .clone()
                        .or_else(|| item.evidence_class.clone());
                    item.manifest_hash = witness
                        .manifest_hash
                        .clone()
                        .or_else(|| item.manifest_hash.clone());
                    item.authority = FactAuthority::CanonicalWitnessFact;
                    item.provenance = "durable-lifecycle-history+witness-ledger".to_owned();
                    item.witness_seq = witness.witness_seq;
                    item.terminal_verdict = witness.terminal_verdict;
                    item.wall_clock_seconds = witness.wall_clock_seconds;
                }
            }
            Some(item)
        })
        .collect();
    envelope
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunState {
    Running,
    Complete,
    Advanced,
    NeedsAttention,
    Idle,
    /// A durable rollover named a successor for this run. It is terminal:
    /// replaying it is refused, and nothing will advance it again.
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunTaskStatus {
    Done,
    Running,
    Blocked,
    Pending,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunTaskCounts {
    pub done: usize,
    pub running: usize,
    pub blocked: usize,
    pub pending: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunTaskProjection {
    pub task_ref: TaskRef,
    pub title: String,
    pub status: RunTaskStatus,
    pub blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<String>,
}

/// One durable member of an explicitly identified flow run.
///
/// This identity list is deliberately separate from [`RunTaskProjection`]:
/// `tasks` is the optional spec-build reconciliation board, while `items`
/// must name every UUID resolved from durable run membership even when that
/// member has no row, lifecycle event, or witness of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunMemberProjection {
    pub task_uuid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunNodeProjection {
    pub task_uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<TaskRef>,
    pub ordinal: Option<u64>,
    pub label: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_max_sec: Option<u64>,
    /// Signed on purpose: a node past its budget reports how far past it is.
    /// A saturating floor of zero made a 400-second overrun indistinguishable
    /// from a node landing exactly on budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_remaining_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunFailureProjection {
    pub task_uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<TaskRef>,
    pub ordinal: Option<u64>,
    pub stage: String,
    pub verdict: Verdict,
    pub attempt: Option<u32>,
    pub lease_epoch: Option<u64>,
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TerminalError>,
}

/// A campaign fact that contradicts the forge's own projection: a sub-issue
/// closed by hand while the task holds no merged proof. Closure is
/// human-clickable, so it completes nothing; the run view says so out loud
/// rather than filing it with the reconciler's warnings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunAnomalyProjection {
    pub kind: String,
    pub task_ref: TaskRef,
    pub issue: String,
    pub url: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunView {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub flow_run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub campaign: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    pub state: RunState,
    /// The rollover that made this run terminal, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<FlowSupersedeRecord>,
    /// The rollover that created this run, when it is itself a successor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<FlowSupersedeRecord>,
    pub counts: RunTaskCounts,
    /// What the run cost, summed per attempt over its durable membership.
    /// Advisory by construction and partial by default; see
    /// [`crate::usage_rollup`].
    pub usage: UsageRollup,
    /// Exact member identities resolved from the durable membership union.
    /// Unlike `tasks`, this is present for every flow kind and does not depend
    /// on a spec-build reconciliation result.
    #[serde(default)]
    pub items: Vec<RunMemberProjection>,
    /// The optional spec-build reconciliation board. This is not the durable
    /// member list, so an empty board is omitted rather than emitted beside a
    /// populated `items` array under the historically ambiguous `tasks` key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<RunTaskProjection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anomalies: Vec<RunAnomalyProjection>,
    pub current_nodes: Vec<RunNodeProjection>,
    pub failures: Vec<RunFailureProjection>,
    pub snapshot: QuerySnapshotMetadata,
    /// Operator reader-state, overlaid by [`apply_reader_state_to_run`] the
    /// same way [`apply_run_lineage`] overlays supersession: durable
    /// reader-state is a different store from the rows, history, and witness
    /// this projection reads, and the handler joins them.
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage_tag: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileProjection {
    #[serde(default)]
    campaign: Option<String>,
    repository: String,
    tasks: Vec<ReconcileTask>,
    merged: Vec<ReconcileMergedTask>,
    #[serde(default)]
    checkpoints: Vec<ReconcileCheckpointTask>,
    frontier: Vec<ReconcileTask>,
    #[serde(default)]
    anomalies: Vec<ReconcileAnomaly>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileAnomaly {
    kind: String,
    task_id: String,
    issue: String,
    url: String,
    detail: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileTask {
    id: String,
    title: String,
    dependencies: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileMergedTask {
    task_id: String,
    pull_request: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconcileCheckpointTask {
    task_id: String,
}

/// Whether [`query_run`] would find this run.
///
/// Split out and made the sole definition of that check so a caller can answer
/// an unknown run id *before* reading the attestation chain (#404).
/// `read_attestations` parses and hash-verifies the whole append-only ledger on
/// every `query.run` — measured at ~2.7 ms/MB — and an id that does not resolve
/// never reaches the rollup that needs it. Because `query_run` raises its own
/// `UnknownJob` from this same predicate, a caller that skips the read on a
/// `false` here cannot answer differently from one that did not.
#[must_use]
pub fn flow_run_exists(
    flow_run: &str,
    details: &[RowDetailFact],
    live: &[LiveJobFact],
    history: &LifecycleSnapshot,
    witness: &[WitnessRecord],
    membership: &FlowMembership,
) -> bool {
    !flow_run_tasks(flow_run, details, witness, membership).is_empty()
        || details.iter().any(|detail| detail.task_uuid == flow_run)
        || live.iter().any(|fact| fact.anchor == flow_run)
        || history
            .records
            .iter()
            .any(|record| record.fields.task_uuid == flow_run)
        || witness
            .iter()
            .any(|record| record.task_uuid.as_deref() == Some(flow_run))
}

/// Project one flow run without returning the large argv, brief, evidence, and
/// provenance fields carried by the general job collection. For spec-build,
/// the schema-validated reconciliation result is the task-state source; live
/// rows and canonical terminal witnesses only advance or block that view.
///
/// `attestations` is the advisory ledger the usage rollup sums, per attempt,
/// over the run's durable membership. A caller with no ledger to offer passes
/// [`AttestationEvidence::unavailable`] and gets a rollup that says it summed
/// nothing, never one that reads as a zero-cost run.
#[allow(clippy::too_many_arguments)]
pub fn query_run(
    flow_run: &str,
    details: &[RowDetailFact],
    live: &[LiveJobFact],
    history: &LifecycleSnapshot,
    witness: &[WitnessRecord],
    now: DateTime<Utc>,
    membership: &FlowMembership,
    attestations: &AttestationEvidence<'_>,
) -> Result<RunView, ObservabilityError> {
    let flow_tasks = flow_run_tasks(flow_run, details, witness, membership);
    let items = flow_tasks
        .iter()
        .cloned()
        .map(|task_uuid| RunMemberProjection { task_uuid })
        .collect();
    let parent_detail = details.iter().find(|detail| detail.task_uuid == flow_run);
    let parent_live = live.iter().find(|fact| fact.anchor == flow_run);
    let parent_events = history
        .records
        .iter()
        .filter(|record| record.fields.task_uuid == flow_run)
        .collect::<Vec<_>>();
    let parent_witness = witness
        .iter()
        .filter(|record| record.task_uuid.as_deref() == Some(flow_run))
        .collect::<Vec<_>>();
    if !flow_run_exists(flow_run, details, live, history, witness, membership) {
        return Err(ObservabilityError::UnknownJob(flow_run.to_owned()));
    }

    let children = child_index(details);
    let detail_by_task = details
        .iter()
        .map(|detail| (detail.task_uuid.as_str(), detail))
        .collect::<BTreeMap<_, _>>();
    let live_by_task = live
        .iter()
        .map(|fact| (fact.anchor.as_str(), fact))
        .collect::<BTreeMap<_, _>>();
    let mut nodes = Vec::new();
    for task_uuid in &flow_tasks {
        let events = history
            .records
            .iter()
            .filter(|record| record.fields.task_uuid == *task_uuid)
            .collect::<Vec<_>>();
        let witnesses = witness
            .iter()
            .filter(|record| record.task_uuid.as_deref() == Some(task_uuid.as_str()))
            .collect::<Vec<_>>();
        nodes.push(build_summary(
            task_uuid,
            detail_by_task.get(task_uuid.as_str()).copied(),
            live_by_task.get(task_uuid.as_str()).copied(),
            &events,
            &witnesses,
            children.get(task_uuid).cloned().unwrap_or_default(),
            &BTreeMap::new(),
        ));
    }
    nodes.sort_by(|left, right| {
        (node_ordinal(left.orchestration.as_ref()), &left.anchor)
            .cmp(&(node_ordinal(right.orchestration.as_ref()), &right.anchor))
    });

    let parent = (!parent_events.is_empty()
        || !parent_witness.is_empty()
        || parent_detail.is_some()
        || parent_live.is_some())
    .then(|| {
        build_summary(
            flow_run,
            parent_detail,
            parent_live,
            &parent_events,
            &parent_witness,
            flow_tasks.iter().cloned().collect(),
            &BTreeMap::new(),
        )
    });

    let flow_name = nodes
        .iter()
        .filter_map(|node| orchestration_string(node.orchestration.as_ref(), "flowName"))
        .next();
    let reconciliation = details
        .iter()
        .filter(|detail| {
            detail.orchestration.as_ref().is_some_and(|orchestration| {
                orchestration.flow_run_id() == flow_run
                    && orchestration_string(Some(orchestration), "flowName").as_deref()
                        == Some("spec-build")
                    && orchestration_string(Some(orchestration), "nodeLabel").as_deref()
                        == Some("spec-build-reconcile")
            })
        })
        .filter_map(|detail| {
            let result =
                serde_json::from_str::<ReconcileProjection>(detail.final_message.as_deref()?)
                    .ok()?;
            Some((
                node_ordinal(detail.orchestration.as_ref()).unwrap_or_default(),
                result,
            ))
        })
        .max_by_key(|(ordinal, _)| *ordinal)
        .map(|(_, result)| result);
    let campaign = reconciliation
        .as_ref()
        .and_then(|result| result.campaign.clone())
        .or_else(|| {
            nodes
                .iter()
                .filter_map(|node| node.task_ref.as_ref())
                .map(|task_ref| task_ref.campaign().to_owned())
                .next()
        });

    let mut current_nodes = nodes
        .iter()
        .filter(|node| node.live_state.is_some())
        .map(|node| {
            let started_at = history
                .records
                .iter()
                .filter(|record| record.fields.task_uuid == node.anchor)
                .filter(|record| record.fields.attempt == node.current_attempt)
                .filter(|record| {
                    node.lease_epoch
                        .is_none_or(|epoch| record.fields.lease_epoch == Some(epoch))
                })
                .filter(|record| record.fields.event == TallyEvent::Started)
                .max_by_key(|record| record.sequence)
                .map(|record| record.observed_at.clone());
            let elapsed_seconds = started_at
                .as_deref()
                .and_then(|timestamp| parse_timestamp(timestamp).ok())
                .map(|started| now.signed_duration_since(started).num_seconds().max(0) as u64);
            RunNodeProjection {
                task_uuid: node.anchor.clone(),
                task_ref: node.task_ref.clone(),
                ordinal: node_ordinal(node.orchestration.as_ref()),
                label: node_label(node),
                state: node
                    .live_state
                    .clone()
                    .unwrap_or_else(|| "pending".to_owned()),
                started_at,
                elapsed_seconds,
                runtime_max_sec: node.runtime_max_sec,
                budget_remaining_seconds: node.runtime_max_sec.map(|budget| {
                    i64::try_from(budget).unwrap_or(i64::MAX)
                        - i64::try_from(elapsed_seconds.unwrap_or(0)).unwrap_or(i64::MAX)
                }),
            }
        })
        .collect::<Vec<_>>();
    current_nodes.sort_by_key(|node| (node.ordinal, node.task_uuid.clone()));

    let mut failures = Vec::new();
    for node in nodes.iter().chain(parent.iter()) {
        if node.terminal_attempt != node.current_attempt {
            continue;
        }
        let Some(verdict) = node
            .terminal_verdict
            .filter(|verdict| !passing_verdict(*verdict))
        else {
            continue;
        };
        let failed_event = history
            .records
            .iter()
            .filter(|record| record.fields.task_uuid == node.anchor)
            .filter(|record| record.fields.attempt == node.current_attempt)
            .filter(|record| {
                node.lease_epoch
                    .is_none_or(|epoch| record.fields.lease_epoch == Some(epoch))
            })
            .filter(|record| record.fields.event == TallyEvent::Failed)
            .max_by_key(|record| record.sequence);
        let terminal_error = witness
            .iter()
            .filter(|record| record.task_uuid.as_deref() == Some(node.anchor.as_str()))
            .filter(|record| node.current_attempt == Some(record.attempt))
            .filter(|record| {
                node.lease_epoch
                    .is_none_or(|lease_epoch| lease_epoch == record.lease_epoch)
            })
            .max_by_key(|record| record.seq)
            .and_then(|record| record.error.clone());
        failures.push(RunFailureProjection {
            task_uuid: node.anchor.clone(),
            task_ref: node.task_ref.clone(),
            ordinal: node_ordinal(node.orchestration.as_ref()),
            stage: node_label(node),
            verdict,
            attempt: node.current_attempt,
            lease_epoch: node.lease_epoch,
            timestamp: node.timestamps.terminal_at.clone(),
            capture_path: None,
            stderr_tail: failed_event.and_then(|record| record.fields.stderr_tail.clone()),
            stderr_truncated: failed_event.and_then(|record| record.fields.stderr_truncated),
            error: terminal_error,
        });
    }
    failures.sort_by_key(|failure| (failure.ordinal, failure.task_uuid.clone()));

    let run_active = parent
        .as_ref()
        .is_some_and(|parent| parent.live_state.is_some())
        || !current_nodes.is_empty();
    let mut tasks = Vec::new();
    let mut anomalies = Vec::new();
    let mut advanced_ids = BTreeSet::new();
    let mut pull_requests = BTreeMap::new();
    let mut done_ids = BTreeSet::new();
    let mut frontier_ids = BTreeSet::new();
    if let (Some(reconciliation), Some(campaign)) = (reconciliation.as_ref(), campaign.as_deref()) {
        for merged in &reconciliation.merged {
            done_ids.insert(merged.task_id.clone());
            pull_requests.insert(merged.task_id.clone(), merged.pull_request.clone());
        }
        done_ids.extend(
            reconciliation
                .checkpoints
                .iter()
                .map(|checkpoint| checkpoint.task_id.clone()),
        );
        frontier_ids.extend(reconciliation.frontier.iter().map(|task| task.id.clone()));
        for node in &nodes {
            let Some(task_ref) = node.task_ref.as_ref() else {
                continue;
            };
            if !node.terminal_verdict.is_some_and(passing_verdict) {
                continue;
            }
            let task_id = task_ref.task_id().to_owned();
            let label = node_label(node);
            if label == format!("merge-{task_id}") {
                done_ids.insert(task_id.clone());
                advanced_ids.insert(task_id.clone());
                if let Some(pull_request) = detail_by_task
                    .get(node.anchor.as_str())
                    .and_then(|detail| detail.final_message.as_deref())
                    .and_then(|message| serde_json::from_str::<Value>(message).ok())
                    .and_then(|value| {
                        value
                            .get("pullRequest")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                {
                    pull_requests.insert(task_id, pull_request);
                }
            } else if label == format!("checkpoint-record-{task_id}") {
                done_ids.insert(task_id.clone());
                advanced_ids.insert(task_id);
            }
        }

        let failure_stages = failures
            .iter()
            .filter_map(|failure| {
                failure
                    .task_ref
                    .as_ref()
                    .map(|task_ref| (task_ref.task_id().to_owned(), failure.stage.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let current_by_task = current_nodes
            .iter()
            .filter_map(|node| {
                node.task_ref
                    .as_ref()
                    .map(|task_ref| (task_ref.task_id().to_owned(), node))
            })
            .collect::<BTreeMap<_, _>>();
        for task in &reconciliation.tasks {
            let task_ref = TaskRef::new(format!("{campaign}/{}", task.id))
                .map_err(|_| ObservabilityError::InvalidRunProjection(flow_run.to_owned()))?;
            let blocked_by = task
                .dependencies
                .iter()
                .filter(|dependency| !done_ids.contains(*dependency))
                .cloned()
                .collect::<Vec<_>>();
            let failure_stage = failure_stages.get(&task.id).cloned();
            let status = if done_ids.contains(&task.id) {
                RunTaskStatus::Done
            } else if failure_stage.is_some() {
                RunTaskStatus::Blocked
            } else if current_by_task
                .get(&task.id)
                .is_some_and(|node| node.state == "running")
            {
                RunTaskStatus::Running
            } else if current_by_task.contains_key(&task.id)
                || (frontier_ids.contains(&task.id) && run_active)
            {
                RunTaskStatus::Pending
            } else if frontier_ids.contains(&task.id) {
                RunTaskStatus::Blocked
            } else if blocked_by.is_empty() {
                RunTaskStatus::Pending
            } else {
                RunTaskStatus::Blocked
            };
            tasks.push(RunTaskProjection {
                task_ref,
                title: task.title.clone(),
                status,
                blocked_by,
                current_node: current_by_task.get(&task.id).map(|node| node.label.clone()),
                failure_stage,
                pull_request: pull_requests.get(&task.id).cloned(),
            });
        }
        for anomaly in &reconciliation.anomalies {
            // A task that reached durable proof after the pass observed the
            // closure is no longer anomalous.
            if done_ids.contains(&anomaly.task_id) {
                continue;
            }
            anomalies.push(RunAnomalyProjection {
                kind: anomaly.kind.clone(),
                task_ref: TaskRef::new(format!("{campaign}/{}", anomaly.task_id))
                    .map_err(|_| ObservabilityError::InvalidRunProjection(flow_run.to_owned()))?,
                issue: anomaly.issue.clone(),
                url: anomaly.url.clone(),
                detail: anomaly.detail.clone(),
            });
        }
    }

    let counts = tasks
        .iter()
        .fold(RunTaskCounts::default(), |mut counts, task| {
            match task.status {
                RunTaskStatus::Done => counts.done += 1,
                RunTaskStatus::Running => counts.running += 1,
                RunTaskStatus::Blocked => counts.blocked += 1,
                RunTaskStatus::Pending => counts.pending += 1,
            }
            counts
        });
    // Only spec-build reconciles a task table. Every other flow finishes with
    // nothing in `tasks`, so completion for those runs is the node verdicts:
    // each admitted node reached a terminal pass on its current attempt.
    let all_nodes_terminal = !nodes.is_empty()
        && nodes.iter().all(|node| {
            node.terminal_attempt == node.current_attempt
                && node.terminal_verdict.is_some_and(passing_verdict)
        });
    let all_tasks_done = if tasks.is_empty() {
        all_nodes_terminal
    } else {
        counts.done == tasks.len()
    };
    let parent_failed = parent
        .as_ref()
        .and_then(|parent| parent.terminal_verdict)
        .is_some_and(|verdict| !passing_verdict(verdict));
    let state = if run_active {
        RunState::Running
    } else if !failures.is_empty() || parent_failed || !anomalies.is_empty() {
        // A hand-closed sub-issue is a human's mistaken belief that a task is
        // done. Nothing in the campaign can resolve it, so the run reports
        // that it needs attention rather than sitting idle.
        RunState::NeedsAttention
    } else if all_tasks_done {
        RunState::Complete
    } else if !advanced_ids.is_empty() {
        RunState::Advanced
    } else {
        RunState::Idle
    };

    Ok(RunView {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        flow_run_id: flow_run.to_owned(),
        flow_name,
        campaign,
        repository: reconciliation
            .as_ref()
            .map(|result| result.repository.clone()),
        state,
        superseded_by: None,
        supersedes: None,
        counts,
        // Over `flow_tasks`, which is durable membership unioned with the rows
        // and witnesses that name the run — so a node this run was handed but
        // whose row names its creating run (the W-316 shape) is inside the sum.
        usage: roll_up(flow_tasks.iter().map(String::as_str), attestations),
        items,
        tasks,
        anomalies,
        current_nodes,
        failures,
        snapshot: snapshot_metadata(history, witness),
        archived: false,
        triage_tag: None,
    })
}

/// Overlay durable generation lineage onto a projected run view.
///
/// Kept out of [`query_run`] because lineage is a different durable store from
/// the rows, history, and witness the projection reads; the handler joins them.
/// A superseded run's state is unconditional: it has an explicit, durable
/// successor, so it is terminal no matter what its own rows say.
pub fn apply_run_lineage(view: &mut RunView, lineage: &FlowLineage) {
    view.superseded_by = lineage.superseded_by(&view.flow_run_id).cloned();
    view.supersedes = lineage.supersedes(&view.flow_run_id).cloned();
    if view.superseded_by.is_some() {
        view.state = RunState::Superseded;
    }
}

/// Attach one usage rollup per flow run the stand-up window touched.
///
/// Kept out of [`crate::query::query_standup`] for the same reason lineage is
/// kept out of [`query_run`]: durable membership and the attestation ledger are
/// different stores from the rows, journal, and witness that projection reads,
/// and the handler is where they are joined.
///
/// "Touched" is decided per entry, from two sources that disagree on purpose.
/// A row's orchestration capsule names the run that *created* the node, while
/// the membership ledger names every run that was *handed* it — so a node one
/// run created and another attached (the W-316 shape) appears under both, and
/// the run that only attached it does not silently drop out of the digest.
/// The flow runs a digest's window touched, in run-ID order.
///
/// Split out and made the sole definition so a caller can find out whether
/// there is anything to roll up *before* reading the attestation chain (#404):
/// a window that touched no run needs no attestations at all, and the read is a
/// full parse and hash-verify of the append-only ledger. [`apply_standup_usage`]
/// uses this same function, so the two cannot disagree about emptiness.
#[must_use]
pub fn standup_touched_runs(
    digest: &StandupDigest,
    details: &[RowDetailFact],
    membership: &FlowMembership,
) -> BTreeSet<String> {
    let flow_run_by_task = details
        .iter()
        .filter_map(|detail| {
            let flow_run = detail.orchestration.as_ref()?.flow_run_id().to_owned();
            Some((detail.task_uuid.as_str(), flow_run))
        })
        .collect::<BTreeMap<_, _>>();
    let touched_tasks = digest
        .completed
        .iter()
        .chain(&digest.gate_fails)
        .chain(&digest.cancelled)
        .filter_map(|entry| entry.task_uuid.as_deref())
        .chain(
            digest
                .in_flight
                .iter()
                .filter_map(|entry| entry.task_uuid.as_deref()),
        )
        .collect::<BTreeSet<_>>();
    let mut runs = BTreeSet::new();
    for task in touched_tasks {
        if let Some(flow_run) = flow_run_by_task.get(task) {
            runs.insert(flow_run.clone());
        }
        runs.extend(membership.runs_holding(task).map(ToOwned::to_owned));
    }
    runs
}

pub fn apply_standup_usage(
    digest: &mut StandupDigest,
    details: &[RowDetailFact],
    witness: &[WitnessRecord],
    membership: &FlowMembership,
    attestations: &AttestationEvidence<'_>,
) {
    let runs = standup_touched_runs(digest, details, membership);
    digest.runs = runs
        .into_iter()
        .map(|flow_run| {
            let tasks = flow_run_tasks(&flow_run, details, witness, membership);
            StandupRunUsage {
                usage: roll_up(tasks.iter().map(String::as_str), attestations),
                flow_run_id: flow_run,
            }
        })
        .collect();
    // Stated once beside the list rather than ~650 bytes per entry (#404). Set
    // only when there is a list to state it for, so a digest that touched no
    // run carries no claim about how its runs were summed.
    digest.usage_basis = (!digest.runs.is_empty()).then(StandupUsageBasis::default);
}

/// Overlay operator reader-state onto a projected run view: whether it is
/// archived, and its free-form triage tag.
///
/// Kept out of [`query_run`] for the same reason lineage is: the reader-state
/// store is a different file from the rows, history, and witness this
/// projection reads, and the handler is where they are joined. Unlike
/// lineage this never changes `view.state` — reader-state is not a fact
/// about execution, so it must never read as one.
pub fn apply_reader_state_to_run(view: &mut RunView, reader_state: &ReaderState) {
    view.archived = reader_state.is_archived(&view.flow_run_id);
    view.triage_tag = reader_state
        .triage_tag(&view.flow_run_id)
        .map(ToOwned::to_owned);
}

/// How a jobs query applies archived reader-state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobsReaderStateMode<'a> {
    /// A broad collection. Archived jobs are retained only when the caller
    /// explicitly opted in.
    Broad { include_archived: bool },
    /// A canonical `flowRun` filter is an explicit by-ID inspection. Its jobs
    /// are annotated with both their creating run's state and the selected
    /// run's state, but never withheld.
    ExplicitLookup { flow_run: &'a str },
}

/// Overlay operator reader-state onto a page of jobs. Every item's `archived`
/// field is set first. Broad collections use the job's *creating* run (the
/// orchestration capsule's `flowRunId`, the same single source
/// [`crate::query::query_standup`]'s primary attribution uses) and filter
/// archived jobs unless the caller opted in. An explicit lookup also annotates
/// from the selected run, which is the only archive identity a membership-only
/// item has, and retains all of its items.
///
/// Returns how many jobs this call actually removed — computed from the same
/// pass that built the returned list, never from a separate count that could
/// disagree with the rows beside it.
pub fn apply_reader_state_to_jobs(
    items: &mut Vec<JobSummary>,
    reader_state: &ReaderState,
    mode: JobsReaderStateMode<'_>,
) -> usize {
    let selected_run_archived = match mode {
        JobsReaderStateMode::ExplicitLookup { flow_run } => reader_state.is_archived(flow_run),
        JobsReaderStateMode::Broad { .. } => false,
    };
    for item in items.iter_mut() {
        item.archived = selected_run_archived
            || item
                .orchestration
                .as_ref()
                .is_some_and(|orchestration| reader_state.is_archived(orchestration.flow_run_id()));
    }
    if matches!(
        mode,
        JobsReaderStateMode::Broad {
            include_archived: true
        } | JobsReaderStateMode::ExplicitLookup { .. }
    ) {
        return 0;
    }
    let before = items.len();
    items.retain(|item| !item.archived);
    before - items.len()
}

/// Remove every entry `keep` rejects from `items`, adding the number removed
/// to `hidden`.
///
/// Filtering and counting are deliberately ONE operation, and *this* part is
/// unconditional: a caller cannot remove an entry through this helper without
/// the removal reaching the counter it passed. Round-2 HIGH-11 found the
/// previous shape — a `retain` here, a `before`/`after` sum over a
/// hand-written list of collections there — pinned for exactly one of the
/// five collections it filtered: dropping `cancelled`, `gate_fails` or
/// `in_flight` from the sum left the whole suite green while the digest
/// under-reported what it had withheld.
///
/// A sum over the digest's collections still exists — [`filterable_entries`],
/// which the conservation backstop compares before and after. What changed is
/// that the sum can no longer silently omit a field: it destructures
/// `StandupDigest` exhaustively, so a new field is a compile error there
/// rather than a quiet blind spot. Using this helper is what keeps the two
/// numbers in agreement; the exhaustive destructure is what keeps the
/// enumeration honest. Neither alone is the whole guarantee.
fn retain_counting<T>(items: &mut Vec<T>, hidden: &mut usize, keep: impl FnMut(&T) -> bool) {
    let before = items.len();
    items.retain(keep);
    *hidden += before - items.len();
}

/// Every entry [`apply_reader_state_to_standup`] could remove, summed across
/// the digest's filterable collections.
///
/// Deliberately written as an exhaustive destructure rather than a sum of
/// field accesses. Round-3 (MUTATION H) showed why: when the enumerator is a
/// hand-written list, a removal from a collection it does not mention changes
/// neither side of the conservation check, so the backstop was blind to
/// exactly the case it was introduced for. Destructuring without `..` makes a
/// new `StandupDigest` field a compile error here —
/// `error[E0027]: pattern does not mention field ...` — so the decision about
/// whether that field is filterable is *forced and visible*.
///
/// What this does **not** buy: an author facing that error can bind the new
/// field to `_` and move on, and nothing here will notice a bare `retain` on
/// it later. The honest invariant is only this:
///
/// > A new `Vec` field on [`StandupDigest`] does not compile until this
/// > function names it. Naming it `_` is a deliberate, visible decision that
/// > the field is not filterable here.
///
/// Do not add `..` to this pattern; `..` is precisely the escape hatch that
/// restores the blindness MUTATION H found.
fn filterable_entries(digest: &StandupDigest) -> usize {
    let StandupDigest {
        schema_version: _,
        protocol_version: _,
        window: _,
        completed,
        in_flight,
        reused: _,
        gate_fails,
        cancelled,
        canonical_gpu_seconds: _,
        runs,
        archived_hidden: _,
        archived_runs_hidden: _,
        // Not a collection of entries: one optional statement about how the
        // `runs` rollups were summed (#404). Nothing is ever removed *from*
        // it, so it contributes no filterable entries. It is not independent
        // of `runs`, though — see the basis clear at the end of
        // `apply_reader_state_to_standup`.
        usage_basis: _,
    } = digest;
    runs.len() + completed.len() + gate_fails.len() + cancelled.len() + in_flight.len()
}

/// Recompute the two task-entry aggregates from the UUIDs that remain visible
/// after reader-state filtering. GPU seconds deliberately retain every
/// qualifying attempt for a visible task, matching `query_standup`; reuse is
/// a task classification, so it follows the newest canonical witness whose
/// verdict is the one projected into a surviving terminal entry.
fn visible_standup_aggregates(digest: &StandupDigest, witness: &[WitnessRecord]) -> (usize, f64) {
    let visible_tasks = digest
        .completed
        .iter()
        .chain(&digest.gate_fails)
        .chain(&digest.cancelled)
        .filter_map(|entry| entry.task_uuid.as_deref())
        .chain(
            digest
                .in_flight
                .iter()
                .filter_map(|entry| entry.task_uuid.as_deref()),
        )
        .collect::<BTreeSet<_>>();
    let terminal_verdicts = digest
        .completed
        .iter()
        .chain(&digest.gate_fails)
        .chain(&digest.cancelled)
        .filter_map(|entry| Some((entry.task_uuid.as_deref()?, entry.verdict)))
        .collect::<BTreeMap<_, _>>();

    let mut latest_witness = BTreeMap::<&str, &WitnessRecord>::new();
    for record in witness {
        let Some(task_uuid) = record.task_uuid.as_deref() else {
            continue;
        };
        if !terminal_verdicts.contains_key(task_uuid) {
            continue;
        }
        latest_witness
            .entry(task_uuid)
            .and_modify(|current| {
                if record.seq > current.seq {
                    *current = record;
                }
            })
            .or_insert(record);
    }
    let reused = latest_witness
        .into_iter()
        .filter(|(task_uuid, record)| {
            record.labor_class == LaborClass::Reused
                && terminal_verdicts.get(task_uuid).copied() == Some(record.verdict)
        })
        .count();
    let canonical_gpu_seconds = witness
        .iter()
        .filter(|record| {
            record
                .task_uuid
                .as_deref()
                .is_some_and(|task_uuid| visible_tasks.contains(task_uuid))
        })
        .filter(|record| counts_toward_canonical_gpu_seconds(record))
        .filter_map(|record| record.gpu_seconds)
        .sum();

    (reused, canonical_gpu_seconds)
}

/// Overlay operator reader-state onto a stand-up digest, filtering entries
/// whose creating run is archived unless `include_archived` is set, and
/// setting [`StandupDigest::archived_hidden`] and
/// [`StandupDigest::archived_runs_hidden`] to exactly how many *task
/// entries* and how many *per-run cost rows* this call removed,
/// respectively — two counts, because they count different lists at
/// different granularity (one archived run holding three tasks removes one
/// `runs` row and up to three task entries) and merging them would make
/// either number a claim about a list it does not describe.
///
/// Task-entry attribution uses only the orchestration capsule's
/// `flowRunId` (the run that *created* the task), not the membership union
/// [`apply_standup_usage`] also folds into `digest.runs` — reader-state
/// stays runs-only and single-owner by design (see the issue's non-goals),
/// so a task attached to an archived run by another run's flow does not
/// vanish from that other run's stand-up. `digest.runs`, by contrast, is
/// filtered (and counted) by its own `flowRunId` directly: a run that only
/// *attached* a task (the W-316 shape) is archived exactly like a run that
/// created one, and its cost row must not survive un-hidden while its count
/// stays silently zero.
///
/// After the four task-entry collections are filtered, their retained task
/// UUIDs also define the displayed aggregates. `reused` follows the latest
/// matching canonical witness's [`LaborClass::Reused`] classification, while
/// `canonical_gpu_seconds` sums retained tasks' witness attempts through
/// [`counts_toward_canonical_gpu_seconds`]. Neither aggregate is inferred from
/// an entry's `gpu_seconds` field or from the separately filtered `runs` rows.
///
/// Both counts are accumulated by [`retain_counting`] as the collections are
/// filtered, never by a separate recount over `details` or anything else. A
/// detail whose run is archived but which produced no digest entry at all
/// (still pending, filtered out by `source`, or otherwise never reached a
/// bucket) must not be counted: nothing was hidden from a reader who did not
/// see it in the first place. A recount over `details` cannot tell that
/// difference; counting what was actually removed always can. See
/// `apply_reader_state_to_standup_never_counts_an_archived_detail_that_produced_no_digest_entry`
/// in this module's tests, which fails if this function is rewritten as a
/// recount.
///
/// Two mechanisms hold the counts to the removals, and it is worth being
/// exact about how far each reaches, because overstating this is a defect
/// this function has already shipped twice:
///
/// - [`retain_counting`] makes counting inseparable from filtering for every
///   collection routed through it. Unconditional.
/// - The closing conservation check compares [`filterable_entries`] before
///   and after against the two counters, catching a removal that bypassed
///   `retain_counting` — a bare `retain`, or a second predicate on a
///   collection already filtered. It reaches the fields
///   `filterable_entries` names, and that enumeration is kept honest by an
///   exhaustive destructure: a new [`StandupDigest`] field does not compile
///   until it is named there. It is a `debug_assertions`-only backstop (see
///   the assertion itself).
///
/// What is *not* claimed: that a removal can never miss a counter whatever
/// collection it lives in. An author who meets the compile error and binds
/// the new field to `_` has made that decision visibly, but they have made
/// it, and nothing here revisits it.
pub fn apply_reader_state_to_standup(
    digest: &mut StandupDigest,
    details: &[RowDetailFact],
    witness: &[WitnessRecord],
    reader_state: &ReaderState,
    include_archived: bool,
) -> usize {
    let flow_run_by_task = details
        .iter()
        .filter_map(|detail| {
            let flow_run = detail.orchestration.as_ref()?.flow_run_id().to_owned();
            Some((detail.task_uuid.clone(), flow_run))
        })
        .collect::<BTreeMap<_, _>>();
    let entries_before = filterable_entries(digest);

    // `include_archived` is folded into each predicate rather than taken as
    // an early return, so the conservation check below covers that path too:
    // an opted-in caller removes nothing and both counts are zero because
    // nothing was removed, not because a branch skipped the counting.
    let mut runs_hidden = 0;
    retain_counting(&mut digest.runs, &mut runs_hidden, |run| {
        include_archived || !reader_state.is_archived(&run.flow_run_id)
    });
    digest.archived_runs_hidden = runs_hidden;

    let keep = |task_uuid: &Option<String>| -> bool {
        include_archived
            || !task_uuid
                .as_ref()
                .and_then(|task| flow_run_by_task.get(task))
                .is_some_and(|flow_run| reader_state.is_archived(flow_run))
    };
    let mut task_hidden = 0;
    retain_counting(&mut digest.completed, &mut task_hidden, |entry| {
        keep(&entry.task_uuid)
    });
    retain_counting(&mut digest.gate_fails, &mut task_hidden, |entry| {
        keep(&entry.task_uuid)
    });
    retain_counting(&mut digest.cancelled, &mut task_hidden, |entry| {
        keep(&entry.task_uuid)
    });
    retain_counting(&mut digest.in_flight, &mut task_hidden, |entry| {
        keep(&entry.task_uuid)
    });
    digest.archived_hidden = task_hidden;

    let (reused, canonical_gpu_seconds) = visible_standup_aggregates(digest, witness);
    digest.reused = reused;
    digest.canonical_gpu_seconds = canonical_gpu_seconds;

    // Every removal across the fields `filterable_entries` names reached a
    // counter. A `debug_assert` rather than a hard one on purpose: a
    // mis-accounted count is a correctness bug that the test suite must
    // refuse, but panicking a live query path over it would trade a slightly
    // wrong number for an unavailable digest, which is the worse failure for
    // the operator this surface serves. The consequence is that this check
    // binds only where `debug_assertions` is on — which includes every
    // `cargo test` run the gate makes, and excludes a release build. It is a
    // development-time backstop behind `retain_counting`, not a runtime
    // guarantee.
    debug_assert_eq!(
        entries_before - filterable_entries(digest),
        digest.archived_hidden + digest.archived_runs_hidden,
        "every entry apply_reader_state_to_standup removes from a collection \
         filterable_entries names must reach one of its two counters; an \
         uncounted removal is exactly the defect archived_hidden and \
         archived_runs_hidden exist to prevent"
    );

    // `usage_basis` states how the entries in `runs` were summed (#404), and
    // `apply_standup_usage` sets it exactly when it leaves a non-empty
    // `runs`. This function can empty `runs` afterwards, so the basis is
    // cleared with them: a digest that shows no run must not carry a
    // statement about how its runs were summed.
    //
    // The invariant `StandupDigest::usage_basis` documents — present exactly
    // when `runs` is non-empty — is therefore a property of the COMPOSITION
    // of the two calls, not of `apply_standup_usage` alone. It is kept rather
    // than weakened deliberately. The alternative, "present when the producer
    // had runs", is a claim about production history, which no consumer can
    // check against the payload it holds; a reader CAN check this one. What
    // distinguishes "the window touched no run" from "reader-state hid them
    // all" is `archived_runs_hidden`, which is still set above.
    //
    // Clearing is lossless: `inherit_usage_basis` only fills entries of
    // `runs`, so with `runs` empty there is nothing left the basis could tell.
    if digest.runs.is_empty() {
        digest.usage_basis = None;
    }

    digest.archived_hidden
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProofStatus {
    Verified,
    NoWitnessExpectedYet,
    ProofMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AttestationReference {
    pub seq: u64,
    pub hash: String,
    pub observed_at: String,
    pub kind: Option<String>,
    pub authority: FactAuthority,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProofEvidence {
    pub specs: Vec<String>,
    pub observations: Vec<EvidenceObservation>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProofLedgerStatus {
    pub verified: bool,
    pub report: VerifyReport,
    pub chain_head: QueryChainHead,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProofView {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub task_uuid: String,
    pub attempt: u32,
    pub lease_epoch: Option<u64>,
    pub status: ProofStatus,
    pub witness_expected: bool,
    pub witness_record: Option<WitnessRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorship: Option<AuthorshipProjection>,
    pub evidence: ProofEvidence,
    pub advisory_attestations: Vec<AttestationReference>,
    pub ledger: ProofLedgerStatus,
    pub history: RetentionMetadata,
}

#[allow(clippy::too_many_arguments)]
pub fn query_proof(
    task: &str,
    requested_attempt: Option<u32>,
    details: &[RowDetailFact],
    history: &LifecycleSnapshot,
    witness_report: &VerifyReport,
    witness: &[WitnessRecord],
    attestations: &[AttestationRecord],
) -> Result<ProofView, ObservabilityError> {
    let detail = details.iter().find(|detail| detail.task_uuid == task);
    let task_events = history
        .records
        .iter()
        .filter(|record| record.fields.task_uuid == task)
        .collect::<Vec<_>>();
    let task_witness = witness
        .iter()
        .filter(|record| record.task_uuid.as_deref() == Some(task))
        .collect::<Vec<_>>();
    if detail.is_none() && task_events.is_empty() && task_witness.is_empty() {
        return Err(ObservabilityError::UnknownJob(task.to_owned()));
    }
    let latest_attempt = detail
        .map(|detail| detail.attempt)
        .into_iter()
        .chain(
            task_events
                .iter()
                .filter_map(|record| record.fields.attempt),
        )
        .chain(task_witness.iter().map(|record| record.attempt))
        .max()
        .unwrap_or(1);
    let attempt = requested_attempt.unwrap_or(latest_attempt);
    let attempt_exists = detail.is_some_and(|detail| detail.attempt == attempt)
        || task_events
            .iter()
            .any(|record| record.fields.attempt == Some(attempt))
        || task_witness.iter().any(|record| record.attempt == attempt);
    if !attempt_exists {
        return Err(ObservabilityError::UnknownAttempt {
            task: task.to_owned(),
            attempt,
        });
    }
    let lease_epoch = detail
        .filter(|detail| detail.attempt == attempt)
        .map(|detail| detail.lease_epoch)
        .into_iter()
        .chain(
            task_events
                .iter()
                .filter(|record| record.fields.attempt == Some(attempt))
                .filter_map(|record| record.fields.lease_epoch),
        )
        .chain(
            task_witness
                .iter()
                .filter(|record| record.attempt == attempt)
                .map(|record| record.lease_epoch),
        )
        .max();
    let selected_witness = task_witness
        .iter()
        .copied()
        .filter(|record| record.attempt == attempt)
        .filter(|record| lease_epoch.is_none_or(|epoch| record.lease_epoch == epoch))
        .max_by_key(|record| record.seq)
        .cloned();
    let selected_events = task_events
        .iter()
        .copied()
        .filter(|record| record.fields.attempt == Some(attempt))
        .filter(|record| lease_epoch.is_none_or(|epoch| record.fields.lease_epoch == Some(epoch)))
        .collect::<Vec<_>>();
    let observations = evidence_observations(&selected_events);
    let terminal_observed = selected_events
        .iter()
        .any(|record| terminal_event(record.fields.event));
    let row_terminal = detail.is_some_and(|detail| {
        detail.attempt == attempt
            && matches!(detail.row_status, RowStatus::Completed | RowStatus::Deleted)
    });
    let witness_expected = selected_witness.is_some() || terminal_observed || row_terminal;
    let status = if selected_witness.is_some() {
        ProofStatus::Verified
    } else if witness_expected {
        ProofStatus::ProofMissing
    } else {
        ProofStatus::NoWitnessExpectedYet
    };
    let advisory_attestations = attestations
        .iter()
        .filter(|record| attestation_matches(record, task, attempt, lease_epoch))
        .map(|record| AttestationReference {
            seq: record.seq,
            hash: record.hash.clone(),
            observed_at: record.observed_at.clone(),
            kind: record
                .payload
                .get("kind")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            authority: FactAuthority::AdvisoryAttestation,
            provenance: "attestation-ledger".to_owned(),
        })
        .collect();
    let authorship = authorship_projection(selected_witness.as_ref(), detail);
    Ok(ProofView {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        task_uuid: task.to_owned(),
        attempt,
        lease_epoch,
        status,
        witness_expected,
        witness_record: selected_witness,
        authorship,
        evidence: ProofEvidence {
            specs: detail.map_or_else(Vec::new, |detail| detail.evidence_specs.clone()),
            observations,
        },
        advisory_attestations,
        ledger: ProofLedgerStatus {
            verified: witness_report.ok,
            report: witness_report.clone(),
            chain_head: witness_head(witness),
        },
        history: history.retention.clone(),
    })
}

/// Every node proof for one flow run, in node-ordinal order.
///
/// Verifying a flow means verifying its nodes, and asking for them one task
/// UUID at a time requires already knowing the UUIDs — which is the thing the
/// operator is trying to find out.
#[allow(clippy::too_many_arguments)]
pub fn query_flow_proofs(
    flow_run: &str,
    details: &[RowDetailFact],
    history: &LifecycleSnapshot,
    witness_report: &VerifyReport,
    witness: &[WitnessRecord],
    attestations: &[AttestationRecord],
    membership: &FlowMembership,
) -> Result<CollectionEnvelope<ProofView>, ObservabilityError> {
    let detail_ordinals = details
        .iter()
        .filter_map(|detail| {
            let orchestration = detail.orchestration.as_ref()?;
            (orchestration.flow_run_id() == flow_run)
                .then(|| (detail.task_uuid.as_str(), orchestration.node_ordinal()))
        })
        .collect::<BTreeMap<_, _>>();
    // The ordinal to sort a node by is the one *this* run admitted it under.
    // For a node the run attached to, the durable row carries the creating
    // run's ordinal instead, so the membership record is the only place the
    // submitting run's own position is written down.
    let mut nodes = flow_run_tasks(flow_run, details, witness, membership)
        .into_iter()
        // A member whose row, events, and witnesses have all aged out of
        // retention has no proof to render, and asking for one would fail the
        // whole run's proof set rather than the one node. This is exactly the
        // existence test `query_proof` applies, so nothing provable is dropped.
        .filter(|task_uuid| {
            detail_ordinals.contains_key(task_uuid.as_str())
                || details.iter().any(|detail| detail.task_uuid == *task_uuid)
                || history
                    .records
                    .iter()
                    .any(|record| record.fields.task_uuid == *task_uuid)
                || witness
                    .iter()
                    .any(|record| record.task_uuid.as_deref() == Some(task_uuid.as_str()))
        })
        .map(|task_uuid| {
            let ordinal = membership
                .node_ordinal(flow_run, &task_uuid)
                .or_else(|| detail_ordinals.get(task_uuid.as_str()).copied().flatten());
            (ordinal, task_uuid)
        })
        .collect::<Vec<_>>();
    nodes.sort();
    nodes.dedup();
    if nodes.is_empty() {
        return Err(ObservabilityError::UnknownJob(flow_run.to_owned()));
    }
    let items = nodes
        .into_iter()
        .map(|(_, task_uuid)| {
            query_proof(
                &task_uuid,
                None,
                details,
                history,
                witness_report,
                witness,
                attestations,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CollectionEnvelope {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        items,
        next_cursor: None,
        position: None,
        position_gap: None,
        flow_run_tasks: None,
        snapshot: snapshot_metadata(history, witness),
    })
}

fn build_summary(
    anchor: &str,
    detail: Option<&RowDetailFact>,
    live: Option<&LiveJobFact>,
    events: &[&LifecycleRecord],
    witnesses: &[&WitnessRecord],
    children: Vec<String>,
    pool_signals: &BTreeMap<String, HeadroomSignal>,
) -> JobSummary {
    let latest_event = events.iter().max_by_key(|record| record.sequence).copied();
    let latest_witness = witnesses
        .iter()
        .max_by_key(|record| (record.attempt, record.lease_epoch, record.seq))
        .copied();
    let current_attempt = live
        .map(|live| live.attempt)
        .into_iter()
        .chain(detail.map(|detail| detail.attempt))
        .chain(events.iter().filter_map(|record| record.fields.attempt))
        .chain(witnesses.iter().map(|record| record.attempt))
        .max();
    let current_lease_epoch = live
        .filter(|live| current_attempt == Some(live.attempt))
        .map(|live| live.lease_epoch)
        .into_iter()
        .chain(
            detail
                .filter(|detail| current_attempt == Some(detail.attempt))
                .map(|detail| detail.lease_epoch),
        )
        .chain(
            events
                .iter()
                .filter(|record| record.fields.attempt == current_attempt)
                .filter_map(|record| record.fields.lease_epoch),
        )
        .chain(
            witnesses
                .iter()
                .filter(|record| current_attempt == Some(record.attempt))
                .map(|record| record.lease_epoch),
        )
        .max();
    let current_event = events
        .iter()
        .filter(|record| {
            record.fields.attempt == current_attempt
                && current_lease_epoch.is_none_or(|lease| record.fields.lease_epoch == Some(lease))
        })
        .max_by_key(|record| record.sequence)
        .copied()
        .or(latest_event);
    let current_events = events
        .iter()
        .copied()
        .filter(|record| {
            record.fields.attempt == current_attempt
                && current_lease_epoch.is_none_or(|lease| record.fields.lease_epoch == Some(lease))
        })
        .collect::<Vec<_>>();
    let current_witness = witnesses
        .iter()
        .copied()
        .filter(|record| current_attempt == Some(record.attempt))
        .filter(|record| current_lease_epoch.is_none_or(|lease| record.lease_epoch == lease))
        .max_by_key(|record| record.seq);
    let labor_class = current_witness
        .map(|record| record.labor_class)
        .or_else(|| {
            live.filter(|live| current_attempt == Some(live.attempt))
                .map(|live| live.labor_class)
        })
        .or_else(|| current_event.and_then(|event| event.fields.labor_class))
        .or_else(|| {
            detail
                .filter(|detail| current_attempt == Some(detail.attempt))
                .map(|detail| detail.labor_class)
        })
        .or_else(|| live.map(|live| live.labor_class))
        .or_else(|| latest_witness.map(|record| record.labor_class));
    let latest_evidence = aggregate_evidence(&current_events);
    let pools = detail
        .map(|detail| detail.pools.clone())
        .or_else(|| current_event.and_then(|event| event.fields.pools.clone()))
        .or_else(|| latest_witness.map(|record| record.pools.clone()));
    let pool_signal_values = pools
        .iter()
        .flatten()
        .filter_map(|pool| {
            pool_signals
                .get(pool)
                .copied()
                .map(|signal| PoolSignalProjection {
                    pool: pool.clone(),
                    signal,
                })
        })
        .collect();
    let mut model = Vec::new();
    if let Some(value) = detail.and_then(|detail| detail.requested_model.clone()) {
        model.push(SourcedValue::new(
            value,
            FactAuthority::DurableAdmissionFact,
            "adapter-options",
        ));
    }
    if let Some(value) = latest_witness.and_then(|record| record.model.clone()) {
        model.push(SourcedValue::new(
            value,
            FactAuthority::CanonicalWitnessFact,
            "witness-ledger",
        ));
    }
    if let Some(value) = detail.and_then(|detail| detail.observed_model.clone()) {
        model.push(SourcedValue::new(
            value,
            FactAuthority::AdvisoryProviderCapture,
            "adapter-scrape",
        ));
    }
    let timestamps = job_timestamps(events, latest_witness);
    let has_durable_task = detail.is_some()
        || witnesses
            .iter()
            .any(|record| record.task_uuid.as_deref() == Some(anchor))
        || live.is_some_and(|live| live.anchor != live.job_id);
    JobSummary {
        anchor: anchor.to_owned(),
        task_uuid: has_durable_task.then(|| anchor.to_owned()),
        task_ref: latest_witness
            .and_then(|record| record.orchestration.as_ref())
            .and_then(Orchestration::task_ref)
            .or_else(|| {
                detail
                    .and_then(|detail| detail.orchestration.as_ref())
                    .and_then(Orchestration::task_ref)
            })
            .or_else(|| current_event.and_then(|event| event.fields.task_ref.clone())),
        live_job_id: live
            .map(|live| live.job_id.clone())
            .or_else(|| current_event.and_then(|event| event.fields.job_id.clone()))
            .or_else(|| latest_event.and_then(|event| event.fields.job_id.clone())),
        description: detail.map(|detail| detail.description.clone()),
        argv: detail.map_or_else(Vec::new, |detail| detail.argv.clone()),
        dedup_key: detail
            .and_then(|detail| detail.dedup_key.clone())
            .or_else(|| latest_witness.and_then(|record| record.dedup_key.clone())),
        disposition: labor_class.map(RowDisposition::from_labor_class),
        brief_hash: latest_witness
            .and_then(|record| record.brief_hash.clone())
            .or_else(|| detail.and_then(|detail| detail.brief_hash.clone())),
        orchestration: latest_witness
            .and_then(|record| record.orchestration.clone())
            .or_else(|| detail.and_then(|detail| detail.orchestration.clone())),
        row_status: detail.map(|detail| detail.row_status),
        live_state: live.map(|live| live.live_state.clone()),
        terminal_verdict: latest_witness.map(|record| record.verdict),
        terminal_attempt: latest_witness.map(|record| record.attempt),
        evidence_result: latest_evidence,
        lifecycle_event: current_event.map(|event| event.fields.event),
        pool_signals: pool_signal_values,
        priority: detail.map(|detail| detail.priority.clone()),
        pools,
        executor: detail
            .and_then(|detail| detail.executor.clone())
            .or_else(|| current_event.and_then(|event| event.fields.executor.clone()))
            .or_else(|| latest_witness.and_then(|record| record.executor.clone())),
        adapter: detail
            .map(|detail| detail.adapter.clone())
            .or_else(|| current_event.and_then(|event| event.fields.agent.clone())),
        source: detail
            .map(|detail| detail.source.clone())
            .or_else(|| current_event.map(|event| source_name(event.fields.source).to_owned())),
        origin: detail.map(|detail| {
            SourcedValue::new(
                detail.origin.clone(),
                FactAuthority::DurableAdmissionFact,
                "durable-task-admission",
            )
        }),
        related_trigger: detail.and_then(|detail| detail.related_trigger.clone()),
        model,
        session_ref: detail.and_then(|detail| {
            detail.session_ref.clone().map(|value| {
                SourcedValue::new(
                    value,
                    FactAuthority::AdvisoryProviderCapture,
                    "adapter-scrape",
                )
            })
        }),
        final_message: detail.and_then(|detail| {
            detail.final_message.clone().map(|value| {
                SourcedValue::new(
                    value,
                    FactAuthority::AdvisoryProviderCapture,
                    "adapter-scrape",
                )
            })
        }),
        usage: detail.and_then(|detail| {
            detail.usage.clone().map(|value| {
                SourcedValue::new(
                    value,
                    FactAuthority::AdvisoryProviderCapture,
                    "adapter-scrape",
                )
            })
        }),
        context_tokens: detail.and_then(|detail| {
            detail.context_tokens.map(|value| {
                SourcedValue::new(
                    value,
                    FactAuthority::AdvisoryProviderCapture,
                    "adapter-scrape",
                )
            })
        }),
        context_window: detail
            .and_then(|detail| detail.context_window.map(context_window_sourced_value)),
        current_attempt,
        lease_epoch: current_lease_epoch,
        unit: live
            .map(|live| live.unit.clone())
            .or_else(|| current_event.and_then(|event| event.fields.unit.clone())),
        labor_class,
        parent_task_uuid: detail
            .and_then(|detail| detail.parent_task_uuid.clone())
            .or_else(|| current_event.and_then(|event| event.fields.parent.clone())),
        child_task_uuids: children,
        timestamps,
        wall_clock_seconds: latest_witness.map(|record| record.wall_clock),
        runtime_max_sec: detail.and_then(|detail| detail.runtime_max_sec),
        consumption_estimate: detail.and_then(|detail| detail.consumption_estimate),
        no_enqueue: detail.map(|detail| detail.no_enqueue),
        evidence_specs: detail.map_or_else(Vec::new, |detail| detail.evidence_specs.clone()),
        evidence_class: latest_witness
            .and_then(|record| record.evidence_class.clone())
            .or_else(|| detail.and_then(|detail| detail.evidence_class.clone())),
        manifest_hash: latest_witness
            .and_then(|record| record.manifest_hash.clone())
            .or_else(|| detail.and_then(|detail| detail.manifest_hash.clone())),
        artifact_content_hash: latest_witness
            .and_then(|record| record.artifact_content_hash.clone()),
        exit_code: latest_witness.map(|record| record.exit_code),
        termination: latest_witness.map(|record| TerminationProjection {
            verdict: record.verdict,
            exit_code: record.exit_code,
            authority: FactAuthority::CanonicalWitnessFact,
            provenance: "witness-ledger".to_owned(),
        }),
        authorship: authorship_projection(latest_witness, detail),
        gpu_seconds: latest_witness.and_then(|record| record.gpu_seconds),
        charge: latest_witness.and_then(|record| record.charge.clone()),
        canonical_gpu_seconds: latest_witness.and_then(|record| {
            counts_toward_canonical_gpu_seconds(record)
                .then_some(record.gpu_seconds)
                .flatten()
        }),
        credential_names: detail.map_or_else(Vec::new, |detail| detail.credential_names.clone()),
        trace: TraceAvailability::default(),
        archived: false,
    }
}

/// A scraped ceiling is what the harness said about itself. A configured
/// ceiling is the operator's own assertion, read from the daemon's live
/// adapter configuration -- not `DurableAdmissionFact`: unlike
/// `requested_model` (a durable `RowSeed` field, written at admission and
/// read back from disk), `RowSeed.context_window` is transport-only and
/// vanishes on a daemon restart, so labelling it durable and admission-time
/// would over-claim exactly what a consumer ranking by authority would
/// trust more, which is backwards for an unpersisted number.
fn context_window_sourced_value(window: ContextWindow) -> SourcedValue<u64> {
    match window.source {
        ContextWindowSource::ProviderCapture => SourcedValue::new(
            window.tokens,
            FactAuthority::AdvisoryProviderCapture,
            "adapter-scrape",
        ),
        ContextWindowSource::AdapterConfig => SourcedValue::new(
            window.tokens,
            FactAuthority::AdvisoryConfig,
            "adapter-config",
        ),
    }
}

fn authorship_projection(
    witness: Option<&WitnessRecord>,
    detail: Option<&RowDetailFact>,
) -> Option<AuthorshipProjection> {
    let witness = witness?;
    let authorship = witness.authorship.as_ref()?;
    let result_revision = witness.result_revision.clone()?;
    let matching_detail = detail.filter(|detail| {
        detail.attempt == witness.attempt && detail.lease_epoch == witness.lease_epoch
    });
    let workspace = matching_detail
        .and_then(|detail| detail.workspace.clone())
        .map(|value| {
            SourcedValue::new(
                value,
                FactAuthority::DurableAdmissionFact,
                "durable-task-admission",
            )
        });
    let tally_session = matching_detail
        .and_then(|detail| detail.session_ref.clone())
        .map(|value| {
            SourcedValue::new(
                value,
                FactAuthority::AdvisoryProviderCapture,
                "adapter-scrape",
            )
        });
    let tally_model = matching_detail.and_then(|detail| {
        detail
            .requested_model
            .clone()
            .map(|value| {
                SourcedValue::new(
                    value,
                    FactAuthority::DurableAdmissionFact,
                    "adapter-options",
                )
            })
            .or_else(|| {
                detail.observed_model.clone().map(|value| {
                    SourcedValue::new(
                        value,
                        FactAuthority::AdvisoryProviderCapture,
                        "adapter-scrape",
                    )
                })
            })
    });
    let git_ai_sessions = witness
        .authorship_sessions
        .iter()
        .flatten()
        .cloned()
        .map(|value| {
            SourcedValue::new(
                value,
                FactAuthority::CanonicalWitnessFact,
                "witness-ledger:git-ai-note",
            )
        })
        .collect();
    Some(AuthorshipProjection {
        status: authorship.status,
        provider: authorship.provider.clone(),
        provider_version: authorship.provider_version.clone(),
        result_revision,
        note_ref: authorship.note_ref.clone(),
        notes_ref_target: authorship.notes_ref_target.clone(),
        note_content_sha256: authorship.note_content_sha256.clone(),
        reason: authorship.reason.clone(),
        identity_mismatch: authorship.status == AuthorshipStatus::Mismatch,
        workspace,
        tally_session,
        tally_model,
        git_ai_sessions,
    })
}

fn child_index(details: &[RowDetailFact]) -> BTreeMap<String, Vec<String>> {
    let mut children = BTreeMap::<String, Vec<String>>::new();
    for detail in details {
        if let Some(parent) = &detail.parent_task_uuid {
            children
                .entry(parent.clone())
                .or_default()
                .push(detail.task_uuid.clone());
        }
    }
    for values in children.values_mut() {
        values.sort();
        values.dedup();
    }
    children
}

fn matches_jobs_filter(
    job: &JobSummary,
    filter: &JobsFilter,
    flow_tasks: Option<&BTreeSet<String>>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> bool {
    if flow_tasks.is_some_and(|tasks| !tasks.contains(&job.anchor)) {
        return false;
    }
    if filter
        .live_state
        .as_deref()
        .is_some_and(|value| job.live_state.as_deref() != Some(value))
        || filter
            .terminal_verdict
            .is_some_and(|value| job.terminal_verdict != Some(value))
        || filter.pool.as_deref().is_some_and(|value| {
            job.pools
                .as_ref()
                .is_none_or(|pools| !pools.iter().any(|pool| pool == value))
        })
        || filter
            .executor
            .as_deref()
            .is_some_and(|value| job.executor.as_deref() != Some(value))
        || filter
            .adapter
            .as_deref()
            .is_some_and(|value| job.adapter.as_deref() != Some(value))
        || filter
            .source
            .as_deref()
            .is_some_and(|value| job.source.as_deref() != Some(value))
        || filter.origin.as_deref().is_some_and(|value| {
            job.origin.as_ref().is_none_or(|origin| {
                origin.value.source != value
                    && origin
                        .value
                        .producer
                        .as_ref()
                        .is_none_or(|producer| producer.name != value)
            })
        })
        || filter
            .parent
            .as_deref()
            .is_some_and(|value| job.parent_task_uuid.as_deref() != Some(value))
        || filter.session.as_deref().is_some_and(|value| {
            job.session_ref
                .as_ref()
                .is_none_or(|session| session.value != value)
        })
    {
        return false;
    }
    let last = [
        job.timestamps.last_event_at.as_deref(),
        job.timestamps.terminal_at.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter_map(|value| DateTime::parse_from_rfc3339(value).ok())
    .map(|value| value.with_timezone(&Utc))
    .max();
    if since.is_some_and(|since| last.is_none_or(|last| last < since))
        || until.is_some_and(|until| last.is_none_or(|last| last > until))
    {
        return false;
    }
    true
}

fn attempt_projections(
    events: &[&LifecycleRecord],
    witness: &[&WitnessRecord],
) -> Vec<AttemptProjection> {
    let mut lanes = BTreeSet::new();
    lanes.extend(
        events
            .iter()
            .map(|record| (record.fields.attempt, record.fields.lease_epoch)),
    );
    lanes.extend(
        witness
            .iter()
            .map(|record| (Some(record.attempt), Some(record.lease_epoch))),
    );
    lanes
        .into_iter()
        .map(|(attempt, lease_epoch)| {
            let lane_events = events
                .iter()
                .copied()
                .filter(|record| {
                    record.fields.attempt == attempt && record.fields.lease_epoch == lease_epoch
                })
                .collect::<Vec<_>>();
            let lane_witness = witness
                .iter()
                .copied()
                .filter(|record| {
                    Some(record.attempt) == attempt && Some(record.lease_epoch) == lease_epoch
                })
                .cloned()
                .collect::<Vec<_>>();
            let evidence_result = aggregate_evidence(&lane_events);
            let terminal = lane_witness.iter().max_by_key(|record| record.seq);
            let timestamps = job_timestamps(&lane_events, terminal);
            AttemptProjection {
                attempt,
                lease_epoch,
                events: lane_events
                    .iter()
                    .map(|record| lifecycle_projection(record))
                    .collect(),
                evidence_result,
                evidence_observations: evidence_observations(&lane_events),
                witness_records: lane_witness,
                timestamps,
            }
        })
        .collect()
}

fn lifecycle_projection(record: &LifecycleRecord) -> LifecycleEventProjection {
    LifecycleEventProjection {
        origin: "journal".to_owned(),
        event_id: record.event_id.clone(),
        cursor: record.cursor.clone(),
        timestamp: record.observed_at.clone(),
        event: record.fields.event,
        task_uuid: record.fields.task_uuid.clone(),
        task_ref: record.fields.task_ref.clone(),
        node_label: None,
        attempt: record.fields.attempt,
        lease_epoch: record.fields.lease_epoch,
        adapter: record.fields.agent.clone(),
        pools: record.fields.pools.clone(),
        executor: record.fields.executor.clone(),
        unit: record.fields.unit.clone(),
        job_id: record.fields.job_id.clone(),
        parent_task_uuid: record.fields.parent.clone(),
        exit_code: record.fields.exit_code,
        stderr_tail: record.fields.stderr_tail.clone(),
        stderr_truncated: record.fields.stderr_truncated,
        labor_class: record.fields.labor_class,
        evidence_result: record
            .fields
            .event
            .is_evidence()
            .then(|| evidence_result(record.fields.event)),
        evidence_spec: record.fields.evidence.clone(),
        session_ref: record.fields.session_ref.clone(),
        source: source_name(record.fields.source).to_owned(),
        gpu_seconds: record.fields.gpu_seconds,
        artifact_hash: record.fields.artifact_hash.clone(),
        evidence_class: None,
        manifest_hash: None,
        message: record.fields.message.clone(),
        authority: FactAuthority::TallyLifecycleObservation,
        provenance: "durable-lifecycle-history".to_owned(),
        witness_seq: None,
        terminal_verdict: None,
        wall_clock_seconds: None,
    }
}

fn witness_lifecycle_projection(record: &WitnessRecord) -> LifecycleEventProjection {
    LifecycleEventProjection {
        origin: "witness".to_owned(),
        event_id: format!("witness:{}", record.seq),
        cursor: format!("witness:{:020}", record.seq),
        timestamp: record.transition_timestamp.clone(),
        event: TallyEvent::WitnessEmitted,
        task_uuid: record
            .task_uuid
            .clone()
            .unwrap_or_else(|| format!("witness:{}", record.seq)),
        task_ref: record
            .orchestration
            .as_ref()
            .and_then(Orchestration::task_ref),
        node_label: None,
        attempt: Some(record.attempt),
        lease_epoch: Some(record.lease_epoch),
        adapter: None,
        pools: Some(record.pools.clone()),
        executor: record.executor.clone(),
        unit: None,
        job_id: None,
        parent_task_uuid: None,
        exit_code: Some(record.exit_code),
        stderr_tail: None,
        stderr_truncated: None,
        labor_class: Some(record.labor_class),
        evidence_result: None,
        evidence_spec: None,
        session_ref: None,
        source: "witness".to_owned(),
        gpu_seconds: record.gpu_seconds,
        artifact_hash: record.artifact_content_hash.clone(),
        evidence_class: record.evidence_class.clone(),
        manifest_hash: record.manifest_hash.clone(),
        message: format!("canonical witness {}", record.seq),
        authority: FactAuthority::CanonicalWitnessFact,
        provenance: "witness-ledger".to_owned(),
        witness_seq: Some(record.seq),
        terminal_verdict: Some(record.verdict),
        wall_clock_seconds: Some(record.wall_clock),
    }
}

fn orchestration_string(orchestration: Option<&Orchestration>, key: &str) -> Option<String> {
    orchestration?
        .as_value()
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn node_ordinal(orchestration: Option<&Orchestration>) -> Option<u64> {
    orchestration.and_then(Orchestration::node_ordinal)
}

fn node_label(node: &JobSummary) -> String {
    orchestration_string(node.orchestration.as_ref(), "nodeLabel")
        .or_else(|| node.description.clone())
        .unwrap_or_else(|| node.anchor.clone())
}

/// One pass over the witness ledger and durable rows, answering the same
/// question `node_label_for_task` answered per call: the label carried by a
/// task's newest witness, falling back to its oldest durable row.
struct NodeLabelIndex {
    witness: BTreeMap<String, Option<String>>,
    detail: BTreeMap<String, Option<String>>,
}

impl NodeLabelIndex {
    fn build(details: &[RowDetailFact], witness: &[WitnessRecord]) -> Self {
        let mut witness_labels = BTreeMap::new();
        for record in witness {
            let Some(task_uuid) = record.task_uuid.as_deref() else {
                continue;
            };
            // Later records overwrite earlier ones, so the newest witness for
            // a task wins even when it carries no label of its own.
            witness_labels.insert(
                task_uuid.to_owned(),
                orchestration_string(record.orchestration.as_ref(), "nodeLabel"),
            );
        }
        let mut detail_labels = BTreeMap::<String, Option<String>>::new();
        for detail in details {
            detail_labels
                .entry(detail.task_uuid.clone())
                .or_insert_with(|| {
                    orchestration_string(detail.orchestration.as_ref(), "nodeLabel")
                });
        }
        Self {
            witness: witness_labels,
            detail: detail_labels,
        }
    }

    fn lookup(&self, task_uuid: &str) -> Option<String> {
        self.witness
            .get(task_uuid)
            .cloned()
            .flatten()
            .or_else(|| self.detail.get(task_uuid).cloned().flatten())
    }
}

const fn passing_verdict(verdict: Verdict) -> bool {
    matches!(
        verdict,
        Verdict::Pass | Verdict::Reused | Verdict::Substituted
    )
}

/// Every task UUID a flow run holds: the durable membership ledger, unioned
/// with the rows and witnesses that carry the run's orchestration capsule.
///
/// A lifecycle event carries no orchestration capsule, so a `--flow-run` filter
/// has to resolve the run's nodes from records that do.
///
/// The union is not belt-and-braces, it is the compatibility story. Membership
/// became a durable admission fact in #380; every row written before that, and
/// every row recovered from a durable enqueue event, carries its capsule and
/// nothing else. Resolving membership from the ledger alone would empty every
/// pre-existing run's window the moment a host advanced its pin. Resolving it
/// from the scan alone is W-316: a row-less admission (`attached`, and
/// full-mode `reused` and `terminal`) hands a run a task UUID whose row belongs
/// to a different run, and the submitting run never sees its own node.
fn flow_run_tasks(
    flow_run: &str,
    details: &[RowDetailFact],
    witness: &[WitnessRecord],
    membership: &FlowMembership,
) -> BTreeSet<String> {
    let mut tasks = BTreeSet::new();
    for detail in details {
        if detail
            .orchestration
            .as_ref()
            .is_some_and(|orchestration| orchestration.flow_run_id() == flow_run)
        {
            tasks.insert(detail.task_uuid.clone());
        }
    }
    for record in witness {
        if let (Some(task_uuid), Some(orchestration)) =
            (record.task_uuid.as_ref(), record.orchestration.as_ref())
        {
            if orchestration.flow_run_id() == flow_run {
                tasks.insert(task_uuid.clone());
            }
        }
    }
    tasks.extend(membership.tasks(flow_run).map(ToOwned::to_owned));
    tasks
}

fn lifecycle_matches(
    record: &LifecycleEventProjection,
    filter: &LifecycleLogFilter,
    flow_tasks: Option<&BTreeSet<String>>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> bool {
    if flow_tasks.is_some_and(|tasks| !tasks.contains(&record.task_uuid)) {
        return false;
    }
    if filter
        .task
        .as_deref()
        .is_some_and(|value| record.task_uuid != value)
        || filter
            .attempt
            .is_some_and(|value| record.attempt != Some(value))
        || filter
            .session
            .as_deref()
            .is_some_and(|value| record.session_ref.as_deref() != Some(value))
        || filter.event.is_some_and(|value| record.event != value)
        || filter
            .source
            .as_deref()
            .is_some_and(|value| record.source != value)
    {
        return false;
    }
    let timestamp = DateTime::parse_from_rfc3339(&record.timestamp)
        .ok()
        .map(|value| value.with_timezone(&Utc));
    if since.is_some_and(|since| timestamp.is_none_or(|timestamp| timestamp < since))
        || until.is_some_and(|until| timestamp.is_none_or(|timestamp| timestamp > until))
    {
        return false;
    }
    true
}

fn evidence_observations(events: &[&LifecycleRecord]) -> Vec<EvidenceObservation> {
    events
        .iter()
        .filter(|record| record.fields.event.is_evidence())
        .filter_map(|record| {
            record
                .fields
                .evidence
                .as_ref()
                .map(|spec| EvidenceObservation {
                    event_id: record.event_id.clone(),
                    cursor: record.cursor.clone(),
                    timestamp: record.observed_at.clone(),
                    spec: spec.clone(),
                    passed: record.fields.event == TallyEvent::EvidencePass,
                    message: record.fields.message.clone(),
                    authority: FactAuthority::TallyLifecycleObservation,
                    provenance: "durable-lifecycle-history".to_owned(),
                })
        })
        .collect()
}

fn job_timestamps(events: &[&LifecycleRecord], witness: Option<&WitnessRecord>) -> JobTimestamps {
    let first = |kind| {
        events
            .iter()
            .filter(|record| record.fields.event == kind)
            .min_by_key(|record| record.sequence)
            .map(|record| record.observed_at.clone())
    };
    JobTimestamps {
        enqueued_at: first(TallyEvent::Enqueued),
        dispatched_at: first(TallyEvent::Dispatched),
        started_at: first(TallyEvent::Started),
        last_event_at: events
            .iter()
            .max_by_key(|record| record.sequence)
            .map(|record| record.observed_at.clone()),
        terminal_at: witness
            .map(|record| record.transition_timestamp.clone())
            .or_else(|| {
                events
                    .iter()
                    .filter(|record| terminal_event(record.fields.event))
                    .max_by_key(|record| record.sequence)
                    .map(|record| record.observed_at.clone())
            }),
    }
}

fn aggregate_evidence(events: &[&LifecycleRecord]) -> Option<EvidenceResult> {
    let mut observed = false;
    for record in events
        .iter()
        .filter(|record| record.fields.event.is_evidence())
    {
        observed = true;
        if record.fields.event == TallyEvent::EvidenceFail {
            return Some(EvidenceResult::Fail);
        }
    }
    observed.then_some(EvidenceResult::Pass)
}

fn evidence_result(event: TallyEvent) -> EvidenceResult {
    if event == TallyEvent::EvidencePass {
        EvidenceResult::Pass
    } else {
        EvidenceResult::Fail
    }
}

fn terminal_event(event: TallyEvent) -> bool {
    matches!(
        event,
        TallyEvent::Completed
            | TallyEvent::Failed
            | TallyEvent::Preempted
            | TallyEvent::WitnessEmitted
    )
}

pub fn snapshot_metadata(
    history: &LifecycleSnapshot,
    witness: &[WitnessRecord],
) -> QuerySnapshotMetadata {
    QuerySnapshotMetadata {
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        cursor: history.retention.latest_cursor.clone(),
        history: history.retention.clone(),
        witness_head: witness_head(witness),
    }
}

fn witness_head(witness: &[WitnessRecord]) -> QueryChainHead {
    witness.last().map_or_else(
        || QueryChainHead {
            seq: 0,
            hash: crate::witness::GENESIS_PREV_HASH.to_owned(),
        },
        |record| QueryChainHead {
            seq: record.seq,
            hash: record.hash.clone(),
        },
    )
}

fn attestation_matches(
    record: &AttestationRecord,
    task: &str,
    attempt: u32,
    lease_epoch: Option<u64>,
) -> bool {
    record.payload.get("taskUuid").and_then(Value::as_str) == Some(task)
        && record
            .payload
            .get("attempt")
            .and_then(Value::as_u64)
            .is_none_or(|value| value == u64::from(attempt))
        && lease_epoch.is_none_or(|epoch| {
            record
                .payload
                .get("leaseEpoch")
                .and_then(Value::as_u64)
                .is_none_or(|value| value == epoch)
        })
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, ObservabilityError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| ObservabilityError::InvalidTimestamp(value.to_owned()))
}

fn priority_name(priority: crate::config::Priority) -> &'static str {
    use crate::config::Priority;
    match priority {
        Priority::Interrupt => "interrupt",
        Priority::High => "high",
        Priority::Medium => "medium",
        Priority::Low => "low",
    }
}

fn source_name(source: crate::taskdb::EnqueueSource) -> &'static str {
    use crate::taskdb::EnqueueSource;
    match source {
        EnqueueSource::Manual => "manual",
        EnqueueSource::Orchestrator => "orchestrator",
        EnqueueSource::Calendar => "calendar",
        EnqueueSource::EventsDir => "events-dir",
        EnqueueSource::Gh => "gh",
        EnqueueSource::BuildEffect => "build-effect",
        EnqueueSource::PoolReachability => "pool-reachability",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Priority;
    use crate::history::{lifecycle_cursor, LIFECYCLE_SCHEMA_VERSION};
    use crate::journal::EmitEvent;
    use crate::taskdb::EnqueueSource;
    use crate::witness::{build_record, Authorship, ChainHead, WitnessBody};

    fn history() -> LifecycleSnapshot {
        LifecycleSnapshot {
            records: Vec::new(),
            retention: RetentionMetadata {
                complete: true,
                policy: crate::history::LIFECYCLE_RETENTION_POLICY.to_owned(),
                earliest_cursor: None,
                latest_cursor: None,
                truncation_boundary: None,
                reason: None,
            },
        }
    }

    fn lifecycle_record(
        sequence: u64,
        event: TallyEvent,
        attempt: u32,
        lease_epoch: u64,
        job_id: &str,
    ) -> LifecycleRecord {
        let terminal = matches!(event, TallyEvent::Completed | TallyEvent::Failed);
        let fields = EmitEvent {
            event,
            task_uuid: "00000000-0000-4000-8000-000000000024".to_owned(),
            task_ref: None,
            class: Priority::High,
            source: EnqueueSource::Manual,
            message: Some(format!("fixture {event}")),
            agent: Some("codex".to_owned()),
            session_ref: Some("scraped-session".to_owned()),
            unit: Some("tally-job-fixture.service".to_owned()),
            exit_code: terminal.then_some(if event == TallyEvent::Completed { 0 } else { 1 }),
            stderr_tail: (event == TallyEvent::Failed)
                .then(|| "actionable lifecycle failure\n".to_owned()),
            stderr_truncated: (event == TallyEvent::Failed).then_some(false),
            gpu_seconds: terminal.then_some(1.0),
            context_tokens: None,
            context_window: None,
            artifact_hash: (event == TallyEvent::Completed)
                .then(|| format!("sha256:{}", "a".repeat(64))),
            evidence: event.is_evidence().then(|| "exit:0".to_owned()),
            attempt: Some(attempt),
            lease_epoch: Some(lease_epoch),
            labor_class: Some(if attempt == 1 {
                LaborClass::Fresh
            } else {
                LaborClass::Recovered
            }),
            job_id: Some(job_id.to_owned()),
            parent: None,
            pools: Some(vec!["slot".to_owned()]),
            executor: None,
        }
        .into_fields()
        .unwrap();
        let cursor = lifecycle_cursor(sequence);
        LifecycleRecord {
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            sequence,
            event_id: cursor.clone(),
            cursor,
            observed_at: format!("2026-07-24T00:00:{sequence:02}.000000Z"),
            realtime_us: 1_753_315_200_000_000 + sequence,
            fields,
        }
    }

    fn detail(status: RowStatus) -> RowDetailFact {
        RowDetailFact {
            task_uuid: "00000000-0000-4000-8000-000000000024".to_owned(),
            description: "proof state fixture".to_owned(),
            argv: vec!["run-proof".to_owned()],
            dedup_key: Some("proof-fixture".to_owned()),
            brief_hash: None,
            orchestration: None,
            row_status: status,
            priority: "high".to_owned(),
            pools: vec!["slot".to_owned()],
            executor: None,
            adapter: "codex".to_owned(),
            source: "manual".to_owned(),
            requested_model: Some("requested-model".to_owned()),
            observed_model: Some("scraped-model".to_owned()),
            session_ref: Some("scraped-session".to_owned()),
            final_message: Some("{\"answer\":42}".to_owned()),
            workspace: Some(WorkspaceMetadata {
                repo: "mecattaf/tally.nix".to_owned(),
                base_rev: "a".repeat(40),
                branch: "query-authorship".to_owned(),
                worktree_path: "/var/lib/tally/worktrees/query-authorship".into(),
            }),
            attempt: 1,
            lease_epoch: 7,
            labor_class: LaborClass::Fresh,
            parent_task_uuid: None,
            evidence_specs: vec!["exit:0".to_owned()],
            consumption_estimate: Some(5),
            runtime_max_sec: Some(60),
            no_enqueue: false,
            credential_names: vec!["token".to_owned()],
            evidence_class: None,
            manifest_hash: None,
            origin: OriginProjection {
                source: "manual".to_owned(),
                producer: None,
                github: None,
            },
            related_trigger: None,
            usage: None,
            context_tokens: None,
            context_window: None,
        }
    }

    fn flow_orchestration(
        flow_run: &str,
        ordinal: u64,
        label: &str,
        task_ref: Option<&str>,
    ) -> Orchestration {
        let mut value = serde_json::json!({
            "flowName": "spec-build",
            "flowRunId": flow_run,
            "nodeOrdinal": ordinal,
            "nodeLabel": label,
        });
        if let Some(task_ref) = task_ref {
            value["taskRef"] = Value::String(task_ref.to_owned());
        }
        Orchestration::new(value).unwrap()
    }

    fn reconciliation_detail(flow_run: &str) -> RowDetailFact {
        let mut detail = detail(RowStatus::Completed);
        detail.task_uuid = "00000000-0000-4000-8000-000000000250".to_owned();
        detail.description = "spec-build-reconcile".to_owned();
        detail.orchestration = Some(flow_orchestration(
            flow_run,
            0,
            "spec-build-reconcile",
            None,
        ));
        detail.final_message = Some(
            serde_json::json!({
                "schemaVersion": 1,
                "campaign": "crm",
                "repository": "mecattaf/tally.nix",
                "source": {"path": "campaign.json", "sha256": format!("sha256:{}", "a".repeat(64))},
                "tasks": [
                    {"id": "t01", "title": "Already merged", "dependencies": []},
                    {"id": "t02", "title": "Current implementation", "dependencies": []},
                    {"id": "t03", "title": "Depends on current", "dependencies": ["t02"]},
                    {"id": "t04", "title": "Ready next", "dependencies": []}
                ],
                "merged": [{"taskId": "t01", "pullRequest": "https://github.com/mecattaf/tally.nix/pull/250"}],
                "remaining": ["t02", "t03", "t04"],
                "frontier": [
                    {"id": "t02", "title": "Current implementation", "dependencies": []},
                    {"id": "t04", "title": "Ready next", "dependencies": []}
                ],
                "complete": false
            })
            .to_string(),
        );
        detail
    }

    fn flow_node_detail(flow_run: &str, status: RowStatus) -> RowDetailFact {
        let mut detail = detail(status);
        detail.task_uuid = "00000000-0000-4000-8000-000000000251".to_owned();
        detail.description = "agent-t02".to_owned();
        detail.orchestration = Some(flow_orchestration(
            flow_run,
            1,
            "agent-t02",
            Some("crm/t02"),
        ));
        detail.runtime_max_sec = Some(60);
        detail
    }

    fn terminal_witness(
        task_uuid: &str,
        verdict: Verdict,
        orchestration: Orchestration,
    ) -> WitnessRecord {
        chained_terminal_witness(
            &ChainHead::default(),
            task_uuid,
            verdict,
            "2026-08-01T10:00:12.000Z",
            orchestration,
        )
    }

    /// A terminal witness that continues an existing chain, so a fixture can
    /// hold two witnesses for one attempt the way a re-emitted verdict does.
    fn chained_terminal_witness(
        head: &ChainHead,
        task_uuid: &str,
        verdict: Verdict,
        transition_timestamp: &str,
        orchestration: Orchestration,
    ) -> WitnessRecord {
        build_record(
            WitnessBody {
                task_uuid: Some(task_uuid.to_owned()),
                transition_timestamp: transition_timestamp.to_owned(),
                verdict,
                exit_code: if passing_verdict(verdict) { 0 } else { 17 },
                artifact_content_hash: None,
                store_paths: None,
                drv: None,
                gpu_seconds: None,
                wall_clock: 12.0,
                attempt: 1,
                lease_epoch: 7,
                dedup_key: None,
                payload_hash: None,
                brief_hash: None,
                origin: AdmissionOrigin::direct(EnqueueSource::Orchestrator),
                orchestration: Some(orchestration),
                labor_class: LaborClass::Fresh,
                trace_ref: None,
                pools: vec!["campaign-agent".to_owned()],
                executor: None,
                host_id: None,
                charge: None,
                model: None,
                evidence_class: None,
                manifest_hash: None,
                completion: None,
                error: None,
                result_revision: None,
                authorship: None,
                authorship_sessions: None,
            },
            head,
        )
        .unwrap()
    }

    fn empty_report() -> VerifyReport {
        VerifyReport {
            ok: true,
            records: 0,
            first_seq: None,
            last_seq: None,
            problems: Vec::new(),
        }
    }

    fn authorship_witness(status: AuthorshipStatus) -> WitnessRecord {
        build_record(
            WitnessBody {
                task_uuid: Some("00000000-0000-4000-8000-000000000024".to_owned()),
                transition_timestamp: "2026-07-26T20:00:00.000Z".to_owned(),
                verdict: Verdict::Pass,
                exit_code: 0,
                artifact_content_hash: None,
                store_paths: None,
                drv: None,
                gpu_seconds: None,
                wall_clock: 1.0,
                attempt: 1,
                lease_epoch: 7,
                dedup_key: None,
                payload_hash: None,
                brief_hash: None,
                origin: AdmissionOrigin::direct(EnqueueSource::Manual),
                orchestration: None,
                labor_class: LaborClass::Fresh,
                trace_ref: None,
                pools: vec!["slot".to_owned()],
                executor: None,
                host_id: None,
                charge: None,
                model: Some("requested-model".to_owned()),
                evidence_class: None,
                manifest_hash: None,
                completion: None,
                error: None,
                result_revision: Some("b".repeat(40)),
                authorship: Some(Authorship {
                    provider: "git-ai".to_owned(),
                    provider_version: "1.6.17".to_owned(),
                    note_ref: "refs/notes/ai".to_owned(),
                    status,
                    notes_ref_target: Some("c".repeat(40)),
                    note_content_sha256: Some(format!("sha256:{}", "d".repeat(64))),
                    reason: (status == AuthorshipStatus::Mismatch).then(|| {
                        "git-ai-mismatch: Tally session/model differs from Git AI's correlated attribution"
                            .to_owned()
                    }),
                }),
                authorship_sessions: Some(vec![AuthorshipSession {
                    tool: "codex".to_owned(),
                    id: "git-ai-session".to_owned(),
                    model: "git-ai-model".to_owned(),
                }]),
            },
            &ChainHead::default(),
        )
        .unwrap()
    }

    #[test]
    fn current_protocol_authority_vocabulary_is_byte_stable() {
        assert_eq!(QUERY_PROTOCOL_VERSION, 5);
        assert_eq!(
            [
                FactAuthority::DurableAdmissionFact,
                FactAuthority::TallyLifecycleObservation,
                FactAuthority::CanonicalWitnessFact,
                FactAuthority::AdvisoryAttestation,
                FactAuthority::AdvisoryProviderCapture,
            ]
            .map(|authority| serde_json::to_string(&authority).unwrap()),
            [
                "\"durable-admission-fact\"",
                "\"tally-lifecycle-observation\"",
                "\"canonical-witness-fact\"",
                "\"advisory-attestation\"",
                "\"advisory-provider-capture\"",
            ]
        );
    }

    #[test]
    fn proof_distinguishes_not_expected_yet_from_missing() {
        let pending = detail(RowStatus::Pending);
        let not_yet = query_proof(
            &pending.task_uuid,
            None,
            std::slice::from_ref(&pending),
            &history(),
            &empty_report(),
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(not_yet.status, ProofStatus::NoWitnessExpectedYet);
        assert!(!not_yet.witness_expected);
        assert!(not_yet.witness_record.is_none());

        let completed = detail(RowStatus::Completed);
        let missing = query_proof(
            &completed.task_uuid,
            None,
            std::slice::from_ref(&completed),
            &history(),
            &empty_report(),
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(missing.status, ProofStatus::ProofMissing);
        assert!(missing.witness_expected);
        assert!(missing.witness_record.is_none());
    }

    #[test]
    fn lifecycle_log_projects_the_failed_stderr_tail() {
        let mut history = history();
        history.records = vec![lifecycle_record(
            1,
            TallyEvent::Failed,
            1,
            7,
            "historical-job-24",
        )];
        let log = query_lifecycle_log(
            &[detail(RowStatus::Completed)],
            &history,
            &[],
            &LifecycleLogFilter {
                task: Some("00000000-0000-4000-8000-000000000024".to_owned()),
                ..LifecycleLogFilter::default()
            },
            &FlowMembership::default(),
        )
        .unwrap();
        assert_eq!(log.items.len(), 1);
        assert_eq!(
            log.items[0].stderr_tail.as_deref(),
            Some("actionable lifecycle failure\n")
        );
        assert_eq!(log.items[0].stderr_truncated, Some(false));
    }

    #[test]
    fn lifecycle_transition_view_collapses_terminal_and_evidence_echoes() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let mut detail = detail(RowStatus::Completed);
        detail.orchestration = Some(flow_orchestration(
            flow_run,
            1,
            "agent-t02",
            Some("crm/t02"),
        ));
        let mut history = history();
        history.records = vec![
            lifecycle_record(1, TallyEvent::EvidencePass, 1, 7, &detail.task_uuid),
            lifecycle_record(2, TallyEvent::Completed, 1, 7, &detail.task_uuid),
        ];
        let witness = terminal_witness(
            &detail.task_uuid,
            Verdict::Pass,
            detail.orchestration.clone().unwrap(),
        );
        let raw = query_lifecycle_log(
            &[detail],
            &history,
            &[witness],
            &LifecycleLogFilter::default(),
            &FlowMembership::default(),
        )
        .unwrap();
        assert_eq!(raw.items.len(), 3);

        let compact = collapse_lifecycle_echoes(raw.clone(), true);
        assert_eq!(compact.items.len(), 1);
        assert_eq!(compact.items[0].origin, "journal+witness");
        assert_eq!(compact.items[0].event, TallyEvent::Completed);
        assert_eq!(compact.items[0].terminal_verdict, Some(Verdict::Pass));
        assert_eq!(compact.items[0].node_label.as_deref(), Some("agent-t02"));
        assert_eq!(compact.items[0].wall_clock_seconds, Some(12.0));

        let evidence_requested = collapse_lifecycle_echoes(raw, false);
        assert_eq!(evidence_requested.items.len(), 2);
        assert_eq!(evidence_requested.items[0].event, TallyEvent::EvidencePass);
    }

    /// Only the witness that is actually folded into the journal terminal may
    /// disappear. A second witness sharing the same (task, attempt, epoch) key
    /// is a distinct durable fact, and dropping it would delete a ledger row
    /// from the operator's view of the attempt.
    #[test]
    fn a_second_witness_sharing_the_echo_key_survives_the_collapse() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let mut detail = detail(RowStatus::Completed);
        detail.orchestration = Some(flow_orchestration(
            flow_run,
            1,
            "agent-t02",
            Some("crm/t02"),
        ));
        let mut history = history();
        history.records = vec![lifecycle_record(
            1,
            TallyEvent::Completed,
            1,
            7,
            &detail.task_uuid,
        )];
        let orchestration = detail.orchestration.clone().unwrap();
        let first = terminal_witness(&detail.task_uuid, Verdict::Pass, orchestration.clone());
        let second = chained_terminal_witness(
            &ChainHead {
                seq: first.seq,
                hash: first.hash.clone(),
            },
            &detail.task_uuid,
            Verdict::Pass,
            "2026-08-01T10:00:13.000Z",
            orchestration,
        );
        assert_eq!((first.attempt, first.lease_epoch), (1, 7));
        assert_eq!((second.attempt, second.lease_epoch), (1, 7));
        assert_ne!(first.seq, second.seq);

        let raw = query_lifecycle_log(
            &[detail],
            &history,
            &[first.clone(), second.clone()],
            &LifecycleLogFilter::default(),
            &FlowMembership::default(),
        )
        .unwrap();
        assert_eq!(raw.items.len(), 3);

        let compact = collapse_lifecycle_echoes(raw, true);
        assert_eq!(compact.items.len(), 2, "{:#?}", compact.items);
        // The journal terminal absorbed exactly one witness -- the first.
        let merged = &compact.items[0];
        assert_eq!(merged.origin, "journal+witness");
        assert_eq!(merged.event, TallyEvent::Completed);
        assert_eq!(merged.witness_seq, Some(first.seq));
        // The other stayed a row of its own rather than being overwritten away.
        let survivor = &compact.items[1];
        assert_eq!(survivor.origin, "witness");
        assert_eq!(survivor.event, TallyEvent::WitnessEmitted);
        assert_eq!(survivor.witness_seq, Some(second.seq));
        assert_eq!(survivor.terminal_verdict, Some(Verdict::Pass));
    }

    /// A key can carry more than one journal terminal -- `preempted` then
    /// `failed`. Only the newest may absorb the canonical verdict; folding it
    /// into the earlier one too would report the same outcome twice and dress
    /// the preemption up as the terminal fact.
    #[test]
    fn only_the_newest_journal_terminal_absorbs_the_witness() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let mut detail = detail(RowStatus::Completed);
        detail.orchestration = Some(flow_orchestration(
            flow_run,
            1,
            "agent-t02",
            Some("crm/t02"),
        ));
        let mut history = history();
        history.records = vec![
            lifecycle_record(1, TallyEvent::Preempted, 1, 7, &detail.task_uuid),
            lifecycle_record(2, TallyEvent::Failed, 1, 7, &detail.task_uuid),
        ];
        let witness = terminal_witness(
            &detail.task_uuid,
            Verdict::Failed,
            detail.orchestration.clone().unwrap(),
        );
        let raw = query_lifecycle_log(
            &[detail],
            &history,
            std::slice::from_ref(&witness),
            &LifecycleLogFilter::default(),
            &FlowMembership::default(),
        )
        .unwrap();
        assert_eq!(raw.items.len(), 3);

        let compact = collapse_lifecycle_echoes(raw, true);
        assert_eq!(compact.items.len(), 2, "{:#?}", compact.items);
        let preempted = &compact.items[0];
        assert_eq!(preempted.event, TallyEvent::Preempted);
        assert_eq!(preempted.origin, "journal");
        assert_eq!(preempted.terminal_verdict, None);
        assert_eq!(preempted.witness_seq, None);
        assert_eq!(
            preempted.authority,
            FactAuthority::TallyLifecycleObservation
        );
        let failed = &compact.items[1];
        assert_eq!(failed.event, TallyEvent::Failed);
        assert_eq!(failed.origin, "journal+witness");
        assert_eq!(failed.terminal_verdict, Some(Verdict::Failed));
        assert_eq!(failed.witness_seq, Some(witness.seq));
    }

    #[test]
    fn run_view_projects_reconciled_tasks_and_live_node_budget() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let reconciliation = reconciliation_detail(flow_run);
        let node = flow_node_detail(flow_run, RowStatus::Pending);
        let mut history = history();
        let mut started = lifecycle_record(1, TallyEvent::Started, 1, 7, &node.task_uuid);
        started.fields.task_uuid.clone_from(&node.task_uuid);
        started.fields.task_ref = Some(TaskRef::new("crm/t02").unwrap());
        started.observed_at = "2026-08-01T10:00:00.000Z".to_owned();
        history.records = vec![started];
        let live = LiveJobFact {
            anchor: node.task_uuid.clone(),
            job_id: node.task_uuid.clone(),
            live_state: "running".to_owned(),
            attempt: 1,
            lease_epoch: 7,
            unit: "tally-job-crm-t02.service".to_owned(),
            labor_class: LaborClass::Fresh,
        };
        let view = query_run(
            flow_run,
            &[reconciliation, node],
            &[live],
            &history,
            &[],
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();

        assert_eq!(view.flow_name.as_deref(), Some("spec-build"));
        assert_eq!(view.campaign.as_deref(), Some("crm"));
        assert_eq!(view.repository.as_deref(), Some("mecattaf/tally.nix"));
        assert_eq!(view.state, RunState::Running);
        assert_eq!(
            view.counts,
            RunTaskCounts {
                done: 1,
                running: 1,
                blocked: 1,
                pending: 1,
            }
        );
        assert_eq!(
            view.tasks
                .iter()
                .map(|task| (task.task_ref.task_id(), task.status))
                .collect::<Vec<_>>(),
            [
                ("t01", RunTaskStatus::Done),
                ("t02", RunTaskStatus::Running),
                ("t03", RunTaskStatus::Blocked),
                ("t04", RunTaskStatus::Pending),
            ]
        );
        assert_eq!(view.tasks[2].blocked_by, ["t02"]);
        assert_eq!(view.current_nodes.len(), 1);
        assert_eq!(view.current_nodes[0].label, "agent-t02");
        assert_eq!(view.current_nodes[0].elapsed_seconds, Some(12));
        assert_eq!(view.current_nodes[0].budget_remaining_seconds, Some(48));
    }

    #[test]
    fn run_view_reports_a_budget_overrun_as_a_negative_remainder() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let reconciliation = reconciliation_detail(flow_run);
        let node = flow_node_detail(flow_run, RowStatus::Pending);
        let mut history = history();
        let mut started = lifecycle_record(1, TallyEvent::Started, 1, 7, &node.task_uuid);
        started.fields.task_uuid.clone_from(&node.task_uuid);
        started.fields.task_ref = Some(TaskRef::new("crm/t02").unwrap());
        started.observed_at = "2026-08-01T10:00:00.000Z".to_owned();
        history.records = vec![started];
        let live = LiveJobFact {
            anchor: node.task_uuid.clone(),
            job_id: node.task_uuid.clone(),
            live_state: "running".to_owned(),
            attempt: 1,
            lease_epoch: 7,
            unit: "tally-job-crm-t02.service".to_owned(),
            labor_class: LaborClass::Fresh,
        };
        let view = query_run(
            flow_run,
            &[reconciliation, node],
            &[live],
            &history,
            &[],
            // 460 s elapsed against a 60 s budget.
            parse_timestamp("2026-08-01T10:07:40.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();

        assert_eq!(view.current_nodes[0].elapsed_seconds, Some(460));
        assert_eq!(view.current_nodes[0].budget_remaining_seconds, Some(-400));
    }

    #[test]
    fn run_view_completes_a_flow_that_has_no_reconciled_task_table() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let node = flow_node_detail(flow_run, RowStatus::Completed);
        let witness = terminal_witness(
            &node.task_uuid,
            Verdict::Pass,
            node.orchestration.clone().unwrap(),
        );

        let pending = query_run(
            flow_run,
            std::slice::from_ref(&node),
            &[],
            &history(),
            &[],
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();
        assert!(pending.tasks.is_empty());
        assert_eq!(pending.state, RunState::Idle);

        let finished = query_run(
            flow_run,
            &[node],
            &[],
            &history(),
            &[witness],
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();
        assert!(finished.tasks.is_empty());
        assert_eq!(finished.state, RunState::Complete);
    }

    #[test]
    fn a_superseded_run_reads_as_terminal_and_names_both_ends_of_the_boundary() {
        let old_run = "00000000-0000-4000-8000-000000000249";
        let new_run = "00000000-0000-4000-8000-00000000024a";
        let node = flow_node_detail(old_run, RowStatus::Completed);
        let witness = terminal_witness(
            &node.task_uuid,
            Verdict::Pass,
            node.orchestration.clone().unwrap(),
        );
        let now = parse_timestamp("2026-08-01T10:00:12.000Z").unwrap();
        let project = |flow_run: &str| {
            query_run(
                flow_run,
                std::slice::from_ref(&node),
                &[],
                &history(),
                std::slice::from_ref(&witness),
                now,
                &FlowMembership::default(),
                &AttestationEvidence::unavailable(),
            )
            .unwrap()
        };

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(crate::flow_lineage::FLOW_LINEAGE_FILE);
        crate::flow_lineage::record_supersede(
            &path,
            old_run,
            new_run,
            crate::flow_lineage::SupersedeReason::GenerationChange,
            &crate::flow_lineage::PredecessorPins::default(),
        )
        .unwrap();
        let lineage = FlowLineage::read(&path).unwrap();

        // Every node passed, so without lineage this run reads Complete. The
        // durable rollover outranks that: it is retired, not finished.
        let mut retired = project(old_run);
        assert_eq!(retired.state, RunState::Complete);
        apply_run_lineage(&mut retired, &lineage);
        assert_eq!(retired.state, RunState::Superseded);
        assert_eq!(
            retired
                .superseded_by
                .as_ref()
                .unwrap()
                .successor_flow_run_id,
            new_run
        );
        assert!(retired.supersedes.is_none());

        // The successor points back without inheriting terminality.
        let mut successor = project(old_run);
        successor.flow_run_id = new_run.to_owned();
        apply_run_lineage(&mut successor, &lineage);
        assert_eq!(successor.state, RunState::Complete);
        assert!(successor.superseded_by.is_none());
        assert_eq!(successor.supersedes.as_ref().unwrap().flow_run_id, old_run);

        // A run outside the chain gains nothing.
        let mut unrelated = project(old_run);
        unrelated.flow_run_id = "00000000-0000-4000-8000-00000000024b".to_owned();
        apply_run_lineage(&mut unrelated, &lineage);
        assert_eq!(unrelated.state, RunState::Complete);
        assert!(unrelated.superseded_by.is_none());
        assert!(unrelated.supersedes.is_none());
    }

    #[test]
    fn lifecycle_labels_resolve_from_the_witness_then_the_durable_row() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let mut labelled = detail(RowStatus::Completed);
        labelled.orchestration = Some(flow_orchestration(
            flow_run,
            1,
            "agent-t02",
            Some("crm/t02"),
        ));
        let mut other = detail(RowStatus::Completed);
        other.task_uuid = "00000000-0000-4000-8000-000000000252".to_owned();
        other.orchestration = Some(flow_orchestration(flow_run, 2, "gate-t02", Some("crm/t02")));
        let mut history = history();
        let mut unrelated = lifecycle_record(2, TallyEvent::Completed, 1, 7, &other.task_uuid);
        unrelated.fields.task_uuid.clone_from(&other.task_uuid);
        history.records = vec![
            lifecycle_record(1, TallyEvent::Completed, 1, 7, &labelled.task_uuid),
            unrelated,
        ];

        // The durable row answers when no witness carries the task.
        let filtered = query_lifecycle_log(
            &[labelled.clone(), other.clone()],
            &history,
            &[],
            &LifecycleLogFilter {
                task: Some(labelled.task_uuid.clone()),
                ..LifecycleLogFilter::default()
            },
            &FlowMembership::default(),
        )
        .unwrap();
        assert_eq!(filtered.items.len(), 1);
        assert_eq!(filtered.items[0].node_label.as_deref(), Some("agent-t02"));

        // A witness for the same task outranks the row.
        let witness = terminal_witness(
            &labelled.task_uuid,
            Verdict::Pass,
            flow_orchestration(flow_run, 1, "retry-t02", None),
        );
        let promoted = query_lifecycle_log(
            &[labelled.clone(), other],
            &history,
            &[witness],
            &LifecycleLogFilter {
                task: Some(labelled.task_uuid.clone()),
                ..LifecycleLogFilter::default()
            },
            &FlowMembership::default(),
        )
        .unwrap();
        assert!(promoted
            .items
            .iter()
            .all(|item| item.node_label.as_deref() == Some("retry-t02")));
    }

    #[test]
    fn run_view_keeps_an_admitted_but_queued_task_pending() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let reconciliation = reconciliation_detail(flow_run);
        let node = flow_node_detail(flow_run, RowStatus::Pending);
        let live = LiveJobFact {
            anchor: node.task_uuid.clone(),
            job_id: node.task_uuid.clone(),
            live_state: "queued".to_owned(),
            attempt: 1,
            lease_epoch: 7,
            unit: "tally-job-crm-t02.service".to_owned(),
            labor_class: LaborClass::Fresh,
        };

        let view = query_run(
            flow_run,
            &[reconciliation, node],
            &[live],
            &history(),
            &[],
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();

        assert_eq!(view.state, RunState::Running);
        assert_eq!(view.tasks[1].status, RunTaskStatus::Pending);
        assert_eq!(view.tasks[1].current_node.as_deref(), Some("agent-t02"));
        assert_eq!(view.current_nodes[0].state, "queued");
        assert_eq!(view.current_nodes[0].elapsed_seconds, None);
    }

    #[test]
    fn run_view_uses_only_the_current_attempt_for_failure_and_elapsed_time() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let reconciliation = reconciliation_detail(flow_run);
        let mut node = flow_node_detail(flow_run, RowStatus::Pending);
        node.attempt = 2;
        node.lease_epoch = 8;
        let mut old_failed = lifecycle_record(1, TallyEvent::Failed, 1, 7, &node.task_uuid);
        old_failed.fields.task_uuid.clone_from(&node.task_uuid);
        old_failed.observed_at = "2026-08-01T09:00:00.000Z".to_owned();
        let mut current_started = lifecycle_record(2, TallyEvent::Started, 2, 8, &node.task_uuid);
        current_started.fields.task_uuid.clone_from(&node.task_uuid);
        current_started.observed_at = "2026-08-01T10:00:00.000Z".to_owned();
        let witness = terminal_witness(
            &node.task_uuid,
            Verdict::Failed,
            node.orchestration.clone().unwrap(),
        );
        let live = LiveJobFact {
            anchor: node.task_uuid.clone(),
            job_id: node.task_uuid.clone(),
            live_state: "running".to_owned(),
            attempt: 2,
            lease_epoch: 8,
            unit: "tally-job-crm-t02.service".to_owned(),
            labor_class: LaborClass::Fresh,
        };
        let mut history = history();
        history.records = vec![old_failed, current_started];

        let view = query_run(
            flow_run,
            &[reconciliation, node],
            &[live],
            &history,
            &[witness],
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();

        assert_eq!(view.tasks[1].status, RunTaskStatus::Running);
        assert!(view.failures.is_empty());
        assert_eq!(
            view.current_nodes[0].started_at.as_deref(),
            Some("2026-08-01T10:00:00.000Z")
        );
        assert_eq!(view.current_nodes[0].elapsed_seconds, Some(12));
    }

    #[test]
    fn a_hand_closed_sub_issue_surfaces_as_an_anomaly_that_needs_attention() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let mut reconciliation = reconciliation_detail(flow_run);
        reconciliation.final_message = Some(
            serde_json::json!({
                "campaign": "crm",
                "repository": "mecattaf/tally.nix",
                "tasks": [
                    {"id": "t01", "title": "Hand-closed task", "dependencies": []},
                    {"id": "t02", "title": "Merged task", "dependencies": []}
                ],
                "merged": [
                    {"taskId": "t02", "pullRequest": "https://example.test/pr/2"}
                ],
                "frontier": [
                    {"id": "t01", "title": "Hand-closed task", "dependencies": []}
                ],
                "anomalies": [
                    {
                        "kind": "closed-without-merged-proof",
                        "taskId": "t01",
                        "issue": "42",
                        "url": "https://example.test/issues/42",
                        "detail": "sub-issue #42 is closed but task 't01' holds no proof"
                    },
                    {
                        "kind": "closed-without-merged-proof",
                        "taskId": "t02",
                        "issue": "43",
                        "url": "https://example.test/issues/43",
                        "detail": "observed closed before the merge landed"
                    }
                ]
            })
            .to_string(),
        );

        let view = query_run(
            flow_run,
            &[reconciliation],
            &[],
            &history(),
            &[],
            parse_timestamp("2026-08-01T10:00:13.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();

        // Closure completes nothing: the task stays off the done list and the
        // run reports that a human has to look.
        assert_eq!(view.anomalies.len(), 1);
        assert_eq!(view.anomalies[0].task_ref.as_str(), "crm/t01");
        assert_eq!(view.anomalies[0].issue, "42");
        assert_eq!(view.state, RunState::NeedsAttention);
        assert_eq!(view.counts.done, 1);
        // A task that reached durable proof anyway is no longer anomalous.
        assert!(view
            .anomalies
            .iter()
            .all(|anomaly| anomaly.task_ref.task_id() != "t02"));
    }

    #[test]
    fn run_view_uses_the_highest_ordinal_reconciliation() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let stale = reconciliation_detail(flow_run);
        let mut current = reconciliation_detail(flow_run);
        current.task_uuid = "00000000-0000-4000-8000-000000000252".to_owned();
        current.orchestration = Some(flow_orchestration(
            flow_run,
            9,
            "spec-build-reconcile",
            None,
        ));
        current.final_message = Some(
            serde_json::json!({
                "campaign": "current",
                "repository": "mecattaf/current",
                "tasks": [
                    {"id": "t09", "title": "Current pass", "dependencies": []}
                ],
                "merged": [],
                "frontier": [
                    {"id": "t09", "title": "Current pass", "dependencies": []}
                ]
            })
            .to_string(),
        );

        let view = query_run(
            flow_run,
            &[stale, current],
            &[],
            &history(),
            &[],
            parse_timestamp("2026-08-01T10:00:13.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();

        assert_eq!(view.campaign.as_deref(), Some("current"));
        assert_eq!(view.repository.as_deref(), Some("mecattaf/current"));
        assert_eq!(view.tasks.len(), 1);
        assert_eq!(view.tasks[0].task_ref.as_str(), "current/t09");
    }

    #[test]
    fn run_view_treats_reconciled_and_new_checkpoint_refs_as_done() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let mut reconciliation = reconciliation_detail(flow_run);
        reconciliation.final_message = Some(
            serde_json::json!({
                "campaign": "crm",
                "repository": "mecattaf/tally.nix",
                "tasks": [
                    {"id": "c01", "kind": "checkpoint", "title": "Prior checkpoint", "dependencies": []},
                    {"id": "c02", "kind": "checkpoint", "title": "Current checkpoint", "dependencies": ["c01"]},
                    {"id": "t03", "kind": "implementation", "title": "Ready after checkpoint", "dependencies": ["c02"]}
                ],
                "merged": [],
                "checkpoints": [{
                    "taskId": "c01",
                    "ref": "refs/tally/spec-build/v1/crm/c01",
                    "revision": "a".repeat(40)
                }],
                "frontier": [
                    {"id": "c02", "kind": "checkpoint", "title": "Current checkpoint", "dependencies": ["c01"]}
                ]
            })
            .to_string(),
        );
        let mut checkpoint = flow_node_detail(flow_run, RowStatus::Completed);
        checkpoint.task_uuid = "00000000-0000-4000-8000-000000000253".to_owned();
        checkpoint.description = "checkpoint-record-c02".to_owned();
        checkpoint.orchestration = Some(flow_orchestration(
            flow_run,
            1,
            "checkpoint-record-c02",
            Some("crm/c02"),
        ));
        checkpoint.final_message = Some(
            serde_json::json!({
                "taskId": "c02",
                "ref": "refs/tally/spec-build/v1/crm/c02",
                "revision": "b".repeat(40)
            })
            .to_string(),
        );
        let witness = terminal_witness(
            &checkpoint.task_uuid,
            Verdict::Pass,
            checkpoint.orchestration.clone().unwrap(),
        );

        let view = query_run(
            flow_run,
            &[reconciliation, checkpoint],
            &[],
            &history(),
            &[witness],
            parse_timestamp("2026-08-01T10:00:13.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();

        assert_eq!(view.state, RunState::Advanced);
        assert_eq!(
            view.tasks
                .iter()
                .map(|task| (task.task_ref.task_id(), task.status))
                .collect::<Vec<_>>(),
            [
                ("c01", RunTaskStatus::Done),
                ("c02", RunTaskStatus::Done),
                ("t03", RunTaskStatus::Pending),
            ]
        );
        assert_eq!(view.tasks[2].blocked_by, Vec::<String>::new());
    }

    #[test]
    fn run_view_prioritizes_live_work_and_failures_over_campaign_completion() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let mut reconciliation = reconciliation_detail(flow_run);
        reconciliation.final_message = Some(
            serde_json::json!({
                "campaign": "crm",
                "repository": "mecattaf/tally.nix",
                "tasks": [
                    {"id": "t01", "title": "Already merged", "dependencies": []}
                ],
                "merged": [{
                    "taskId": "t01",
                    "pullRequest": "https://github.com/mecattaf/tally.nix/pull/250"
                }],
                "frontier": []
            })
            .to_string(),
        );
        let mut cleanup = flow_node_detail(flow_run, RowStatus::Pending);
        cleanup.description = "cleanup-t01".to_owned();
        cleanup.orchestration = Some(flow_orchestration(
            flow_run,
            1,
            "cleanup-t01",
            Some("crm/t01"),
        ));
        let live = LiveJobFact {
            anchor: cleanup.task_uuid.clone(),
            job_id: cleanup.task_uuid.clone(),
            live_state: "running".to_owned(),
            attempt: 1,
            lease_epoch: 7,
            unit: "tally-job-crm-t01.service".to_owned(),
            labor_class: LaborClass::Fresh,
        };

        let active = query_run(
            flow_run,
            &[reconciliation.clone(), cleanup.clone()],
            &[live],
            &history(),
            &[],
            parse_timestamp("2026-08-01T10:00:13.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();
        assert_eq!(active.counts.done, 1);
        assert_eq!(active.state, RunState::Running);

        cleanup.row_status = RowStatus::Completed;
        let mut failed = lifecycle_record(1, TallyEvent::Failed, 1, 7, &cleanup.task_uuid);
        failed.fields.task_uuid.clone_from(&cleanup.task_uuid);
        failed.fields.task_ref = Some(TaskRef::new("crm/t01").unwrap());
        let witness = terminal_witness(
            &cleanup.task_uuid,
            Verdict::Failed,
            cleanup.orchestration.clone().unwrap(),
        );
        let mut terminal_history = history();
        terminal_history.records = vec![failed];
        let failed = query_run(
            flow_run,
            &[reconciliation, cleanup],
            &[],
            &terminal_history,
            &[witness],
            parse_timestamp("2026-08-01T10:00:14.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();
        assert_eq!(failed.counts.done, 1);
        assert_eq!(failed.state, RunState::NeedsAttention);
        assert_eq!(failed.failures[0].stage, "cleanup-t01");
    }

    #[test]
    fn run_view_points_at_failed_task_stage_and_stderr_tail() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let reconciliation = reconciliation_detail(flow_run);
        let node = flow_node_detail(flow_run, RowStatus::Completed);
        let mut failed = lifecycle_record(1, TallyEvent::Failed, 1, 7, &node.task_uuid);
        failed.fields.task_uuid.clone_from(&node.task_uuid);
        failed.fields.task_ref = Some(TaskRef::new("crm/t02").unwrap());
        let witness = terminal_witness(
            &node.task_uuid,
            Verdict::Failed,
            node.orchestration.clone().unwrap(),
        );
        let mut history = history();
        history.records = vec![failed];
        let view = query_run(
            flow_run,
            &[reconciliation, node],
            &[],
            &history,
            &[witness],
            parse_timestamp("2026-08-01T10:00:13.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();

        assert_eq!(view.state, RunState::NeedsAttention);
        assert_eq!(view.tasks[1].status, RunTaskStatus::Blocked);
        assert_eq!(view.tasks[1].failure_stage.as_deref(), Some("agent-t02"));
        assert_eq!(view.failures.len(), 1);
        assert_eq!(
            view.failures[0].task_ref.as_ref().unwrap().as_str(),
            "crm/t02"
        );
        assert_eq!(view.failures[0].stage, "agent-t02");
        assert_eq!(
            view.failures[0].stderr_tail.as_deref(),
            Some("actionable lifecycle failure\n")
        );
    }

    #[test]
    fn scraped_fields_remain_advisory_and_credentials_are_names_only() {
        let detail = detail(RowStatus::Pending);
        let jobs = query_jobs(
            std::slice::from_ref(&detail),
            &[],
            &history(),
            &[],
            &BTreeMap::new(),
            &JobsFilter::default(),
            &FlowMembership::default(),
        )
        .unwrap();
        let job = &jobs.items[0];
        assert_eq!(job.credential_names, ["token"]);
        assert_eq!(job.model.len(), 2);
        assert_eq!(job.model[0].authority, FactAuthority::DurableAdmissionFact);
        assert_eq!(
            job.model[1].authority,
            FactAuthority::AdvisoryProviderCapture
        );
        assert_eq!(
            job.session_ref.as_ref().unwrap().authority,
            FactAuthority::AdvisoryProviderCapture
        );
        let encoded = serde_json::to_string(job).unwrap();
        assert!(!encoded.contains("/run/credentials"));
    }

    #[test]
    fn job_and_proof_project_authorship_as_separately_sourced_observations() {
        let detail = detail(RowStatus::Completed);
        let witness = authorship_witness(AuthorshipStatus::Mismatch);
        let jobs = query_jobs(
            std::slice::from_ref(&detail),
            &[],
            &history(),
            std::slice::from_ref(&witness),
            &BTreeMap::new(),
            &JobsFilter::default(),
            &FlowMembership::default(),
        )
        .unwrap();
        let projected = jobs.items[0].authorship.as_ref().unwrap();
        assert_eq!(jobs.protocol_version, 5);
        assert_eq!(projected.status, AuthorshipStatus::Mismatch);
        assert!(projected.identity_mismatch);
        assert_eq!(projected.result_revision, "b".repeat(40));
        assert_eq!(
            projected.workspace.as_ref().unwrap().authority,
            FactAuthority::DurableAdmissionFact
        );
        assert_eq!(
            projected.tally_session.as_ref().unwrap(),
            &SourcedValue::new(
                "scraped-session".to_owned(),
                FactAuthority::AdvisoryProviderCapture,
                "adapter-scrape",
            )
        );
        assert_eq!(
            projected.tally_model.as_ref().unwrap().authority,
            FactAuthority::DurableAdmissionFact
        );
        assert_eq!(
            projected.git_ai_sessions[0].value,
            AuthorshipSession {
                tool: "codex".to_owned(),
                id: "git-ai-session".to_owned(),
                model: "git-ai-model".to_owned(),
            }
        );
        assert_eq!(
            projected.git_ai_sessions[0].authority,
            FactAuthority::CanonicalWitnessFact
        );

        let proof = query_proof(
            &detail.task_uuid,
            Some(1),
            std::slice::from_ref(&detail),
            &history(),
            &VerifyReport {
                ok: true,
                records: 1,
                first_seq: Some(1),
                last_seq: Some(1),
                problems: Vec::new(),
            },
            std::slice::from_ref(&witness),
            &[],
        )
        .unwrap();
        assert_eq!(proof.authorship, jobs.items[0].authorship);
        let encoded = serde_json::to_string(&proof).unwrap();
        assert!(!encoded.contains("\"prompts\""));
        assert!(!encoded.contains("\"ranges\""));
    }

    #[test]
    fn latest_attempt_wins_over_a_stale_row_and_evidence_fail_is_sticky() {
        let stale = detail(RowStatus::Completed);
        let mut history = history();
        history.records = vec![
            lifecycle_record(1, TallyEvent::EvidencePass, 1, 1, "historical-job-24"),
            lifecycle_record(2, TallyEvent::EvidencePass, 2, 2, "historical-job-24"),
            lifecycle_record(3, TallyEvent::EvidenceFail, 2, 2, "historical-job-24"),
            lifecycle_record(4, TallyEvent::EvidencePass, 2, 2, "historical-job-24"),
            lifecycle_record(5, TallyEvent::Completed, 2, 2, "historical-job-24"),
        ];
        history.retention.latest_cursor = Some(lifecycle_cursor(5));

        let jobs = query_jobs(
            std::slice::from_ref(&stale),
            &[],
            &history,
            &[],
            &BTreeMap::new(),
            &JobsFilter::default(),
            &FlowMembership::default(),
        )
        .unwrap();
        let job = &jobs.items[0];
        assert_eq!(job.current_attempt, Some(2));
        assert_eq!(job.lease_epoch, Some(2));
        assert_eq!(job.labor_class, Some(LaborClass::Recovered));
        assert_eq!(job.lifecycle_event, Some(TallyEvent::Completed));
        assert_eq!(job.evidence_result, Some(EvidenceResult::Fail));

        let detail = query_job(
            "historical-job-24",
            std::slice::from_ref(&stale),
            &[],
            &history,
            &[],
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(detail.job.anchor, stale.task_uuid);
        assert_eq!(detail.attempts.len(), 2);
        assert_eq!(
            detail.attempts[1].evidence_result,
            Some(EvidenceResult::Fail)
        );
        assert!(detail.attempts[1].timestamps.terminal_at.is_some());
    }

    /// #380: the durable ledger and the original scan agree wherever both can
    /// see, and the ledger is exactly the difference where the scan is blind.
    ///
    /// The truth of this corpus is fixed by construction rather than derived
    /// from either mechanism: run A created two nodes and owns both rows; run B
    /// created nothing and was handed one of A's task UUIDs by a row-less
    /// admission. The assertions name those UUIDs.
    #[test]
    fn acceptance_380_membership_agrees_with_the_scan_and_supplies_only_what_it_cannot_see() {
        let run_a = "00000000-0000-4000-8000-0000000003a0";
        let run_b = "00000000-0000-4000-8000-0000000003b0";
        let owned = "00000000-0000-4000-8000-000000000251";
        let shared = "00000000-0000-4000-8000-000000000250";

        let details = vec![
            flow_node_detail(run_a, RowStatus::Pending),
            reconciliation_detail(run_a),
        ];
        assert_eq!(details[0].task_uuid, owned);
        assert_eq!(details[1].task_uuid, shared);
        let witness = vec![terminal_witness(
            shared,
            Verdict::Pass,
            flow_orchestration(run_a, 0, "spec-build-reconcile", None),
        )];

        // Run A owns both rows, so the scan alone already knows its whole
        // membership -- and the ledger, which also records A's created
        // admissions, must not change that number by so much as one.
        let mut ledger = FlowMembership::default();
        for task in [owned, shared] {
            ledger.insert(crate::flow_membership::FlowMembershipRecord::new(
                run_a.to_owned(),
                task.to_owned(),
                crate::flow_membership::MembershipDisposition::Created,
                Some(0),
                None,
            ));
        }
        let scan_only = flow_run_tasks(run_a, &details, &witness, &FlowMembership::default());
        let derived = flow_run_tasks(run_a, &details, &witness, &ledger);
        assert_eq!(
            scan_only,
            BTreeSet::from([owned.to_owned(), shared.to_owned()]),
            "the scan half must be untouched"
        );
        assert_eq!(
            derived, scan_only,
            "a run that owns its rows resolves identically with and without the ledger"
        );

        // Run B owns no row at all. The scan is blind to it; the ledger is the
        // whole of what it can see, and it is exactly the one task B was handed.
        ledger.insert(crate::flow_membership::FlowMembershipRecord::new(
            run_b.to_owned(),
            shared.to_owned(),
            crate::flow_membership::MembershipDisposition::Attached,
            Some(7),
            Some("b-node-7".to_owned()),
        ));
        assert!(flow_run_tasks(run_b, &details, &witness, &FlowMembership::default()).is_empty());
        assert_eq!(
            flow_run_tasks(run_b, &details, &witness, &ledger),
            BTreeSet::from([shared.to_owned()])
        );

        // A's answer is still A's answer: the ledger gaining a record for B
        // cannot move A's membership.
        assert_eq!(
            flow_run_tasks(run_a, &details, &witness, &ledger),
            scan_only
        );

        // And the node ordinal B submitted under is B's, not the ordinal on the
        // row, which belongs to A.
        assert_eq!(ledger.node_ordinal(run_b, shared), Some(7));
        assert_eq!(
            node_ordinal(details[1].orchestration.as_ref()),
            Some(0),
            "the row still carries the creating run's ordinal, unchanged"
        );
    }

    /// One scraped attempt as the exit recorder writes it: real codex numbers
    /// from `test/fixtures/usage`, normalized the way `usage::observe` does.
    fn scrape_attestation(
        seq: u64,
        task: &str,
        attempt: u32,
        output_tokens: u64,
    ) -> AttestationRecord {
        AttestationRecord {
            observed_at: "2026-08-01T10:00:12.000Z".to_owned(),
            payload: serde_json::json!({
                "kind": "adapter-scrape",
                "taskUuid": task,
                "jobId": task,
                "adapter": "codex",
                "attempt": attempt,
                "leaseEpoch": 7,
                "captures": {},
                "usage": {
                    "state": "reported",
                    "breakdown": {
                        "shape": "components",
                        "inputTokens": 262_086,
                        "inputTokensAsReported": 7_060_166,
                        "cacheReadTokens": 6_798_080,
                        "cacheWriteTokens": 0,
                        "outputTokens": output_tokens,
                        "reasoningTokens": 15_163,
                        "totalTokens": {
                            "value": 262_086 + 6_798_080 + output_tokens,
                            "source": "derived-from-components"
                        }
                    }
                },
                "usageAuthority": "advisory-only",
            }),
            seq,
            prev_hash: "sha256:prev".to_owned(),
            hash: "sha256:hash".to_owned(),
        }
    }

    #[test]
    fn acceptance_384_a_run_sums_the_usage_of_a_task_only_its_membership_names() {
        // The W-316 shape: run B was handed a node run A created, so B owns no
        // row for it and the scan is blind. The rollup must charge B for it
        // anyway, because the durable membership says B ran it.
        let run_a = "00000000-0000-4000-8000-0000000003a0";
        let run_b = "00000000-0000-4000-8000-0000000003b0";
        let shared = "00000000-0000-4000-8000-000000000250";
        let details = vec![reconciliation_detail(run_a)];
        assert_eq!(details[0].task_uuid, shared);

        let mut ledger = FlowMembership::default();
        ledger.insert(crate::flow_membership::FlowMembershipRecord::new(
            run_b.to_owned(),
            shared.to_owned(),
            crate::flow_membership::MembershipDisposition::Attached,
            Some(7),
            Some("b-node-7".to_owned()),
        ));
        let records = [scrape_attestation(1, shared, 1, 32_842)];
        let evidence = AttestationEvidence::new(true, &records);

        let view = query_run(
            run_b,
            &details,
            &[],
            &history(),
            &[],
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &ledger,
            &evidence,
        )
        .unwrap();
        assert_eq!(
            view.items,
            [RunMemberProjection {
                task_uuid: shared.to_owned(),
            }]
        );
        assert_eq!(view.usage.coverage.tasks, 1);
        assert_eq!(view.usage.coverage.attempts_reported, 1);
        assert_eq!(view.usage.coverage.tasks_without_attestation, 0);
        assert_eq!(view.usage.tokens.output_tokens.value, 32_842);
        assert_eq!(
            view.usage.authority,
            FactAuthority::AdvisoryProviderCapture,
            "a rollup over advisory captures is graded as one"
        );

        // Without the membership ledger the same query sees no member at all,
        // which is what makes the sum above membership's doing and not the
        // scan's.
        let scan_only = query_run(
            run_b,
            &details,
            &[],
            &history(),
            &[],
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &FlowMembership::default(),
            &evidence,
        );
        assert!(matches!(scan_only, Err(ObservabilityError::UnknownJob(_))));
    }

    #[test]
    fn explicit_run_serialization_falls_back_to_items_for_membership_only_identity() {
        let flow_run = "00000000-0000-4000-8000-000000000415";
        let member = "00000000-0000-4000-8000-000000000416";
        let mut membership = FlowMembership::default();
        membership.insert(crate::flow_membership::FlowMembershipRecord::new(
            flow_run.to_owned(),
            member.to_owned(),
            crate::flow_membership::MembershipDisposition::Reused,
            Some(0),
            Some("reuse-only".to_owned()),
        ));

        let view = query_run(
            flow_run,
            &[],
            &[],
            &history(),
            &[],
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &membership,
            &AttestationEvidence::unavailable(),
        )
        .unwrap();
        assert_eq!(
            view.items,
            [RunMemberProjection {
                task_uuid: member.to_owned(),
            }]
        );
        assert!(view.tasks.is_empty());

        let public = serde_json::to_value(view).unwrap();
        assert!(
            public.get("tasks").is_none(),
            "an empty reconciliation board must not shadow durable members"
        );
        let members = public
            .get("tasks")
            .or_else(|| public.get("items"))
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert!(members
            .iter()
            .any(|item| item["taskUuid"].as_str() == Some(member)));
    }

    #[test]
    fn a_run_rollup_counts_every_attempt_and_names_the_members_it_cannot_see() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let node = flow_node_detail(flow_run, RowStatus::Completed);
        let reconciliation = reconciliation_detail(flow_run);
        let unscraped = reconciliation.task_uuid.clone();
        let records = [
            scrape_attestation(1, &node.task_uuid, 1, 100),
            scrape_attestation(2, &node.task_uuid, 2, 200),
        ];
        let view = query_run(
            flow_run,
            &[node, reconciliation],
            &[],
            &history(),
            &[],
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::new(true, &records),
        )
        .unwrap();

        // Two members, two attempts of one of them, and the retry is charged.
        assert_eq!(view.usage.coverage.tasks, 2);
        assert_eq!(view.usage.coverage.attempts_observed, 2);
        assert_eq!(view.usage.coverage.attempts_reported, 2);
        assert_eq!(view.usage.tokens.output_tokens.value, 300);
        // The member with no attestation is stated, never quietly dropped.
        assert_eq!(view.usage.coverage.tasks_without_attestation, 1);
        assert!(view
            .usage
            .caveats
            .contains(&crate::usage_rollup::UsageRollupCaveat::MembersWithoutAttestation));
        assert!(!view.usage.is_complete());
        assert!(!unscraped.is_empty());
    }

    #[test]
    fn a_run_view_built_without_the_ledger_reports_no_usage_rather_than_zero_usage() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let node = flow_node_detail(flow_run, RowStatus::Completed);
        let view = query_run(
            flow_run,
            &[node],
            &[],
            &history(),
            &[],
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();
        assert!(!view.usage.coverage.ledger_verified);
        assert_eq!(view.usage.tokens.total_tokens, None);
        assert!(view
            .usage
            .caveats
            .contains(&crate::usage_rollup::UsageRollupCaveat::LedgerUnverified));
    }

    fn standup_fixture(task_uuid: &str) -> StandupDigest {
        StandupDigest {
            schema_version: QUERY_SCHEMA_VERSION,
            protocol_version: QUERY_PROTOCOL_VERSION,
            window: crate::query::StandupWindow {
                since: None,
                until: "2026-08-01T10:00:12.000Z".to_owned(),
            },
            completed: vec![crate::query::CompletedEntry {
                task_uuid: Some(task_uuid.to_owned()),
                task_ref: None,
                gpu_seconds: None,
                verdict: Verdict::Pass,
                session_ref: None,
                gh_origin: None,
            }],
            in_flight: Vec::new(),
            reused: 0,
            gate_fails: Vec::new(),
            cancelled: Vec::new(),
            canonical_gpu_seconds: 0.0,
            runs: Vec::new(),
            archived_hidden: 0,
            archived_runs_hidden: 0,
            usage_basis: None,
        }
    }

    fn standup_aggregate_witness(
        head: &mut ChainHead,
        task_uuid: &str,
        flow_run: &str,
        attempt: u32,
        labor_class: LaborClass,
        gpu_seconds: f64,
    ) -> WitnessRecord {
        let verdict = if labor_class == LaborClass::Reused {
            Verdict::Reused
        } else {
            Verdict::Pass
        };
        let record = build_record(
            WitnessBody {
                task_uuid: Some(task_uuid.to_owned()),
                transition_timestamp: format!("2026-08-01T10:00:{:02}.000Z", head.seq + 12),
                verdict,
                exit_code: 0,
                artifact_content_hash: None,
                store_paths: None,
                drv: None,
                gpu_seconds: Some(gpu_seconds),
                wall_clock: 12.0,
                attempt,
                lease_epoch: 7,
                dedup_key: None,
                payload_hash: None,
                brief_hash: None,
                origin: AdmissionOrigin::direct(EnqueueSource::Orchestrator),
                orchestration: Some(flow_orchestration(flow_run, 1, "aggregate-fixture", None)),
                labor_class,
                trace_ref: None,
                pools: vec!["campaign-agent".to_owned()],
                executor: None,
                host_id: None,
                charge: None,
                model: None,
                evidence_class: None,
                manifest_hash: None,
                completion: None,
                error: None,
                result_revision: None,
                authorship: None,
                authorship_sessions: None,
            },
            head,
        )
        .unwrap();
        *head = ChainHead {
            seq: record.seq,
            hash: record.hash.clone(),
        };
        record
    }

    #[test]
    fn acceptance_384_standup_carries_one_rollup_per_run_the_window_touched() {
        let run_a = "00000000-0000-4000-8000-0000000003a0";
        let run_b = "00000000-0000-4000-8000-0000000003b0";
        let shared = "00000000-0000-4000-8000-000000000250";
        let details = vec![reconciliation_detail(run_a)];
        let mut ledger = FlowMembership::default();
        ledger.insert(crate::flow_membership::FlowMembershipRecord::new(
            run_b.to_owned(),
            shared.to_owned(),
            crate::flow_membership::MembershipDisposition::Attached,
            Some(7),
            Some("b-node-7".to_owned()),
        ));
        let records = [scrape_attestation(1, shared, 1, 32_842)];

        let mut digest = standup_fixture(shared);
        apply_standup_usage(
            &mut digest,
            &details,
            &[],
            &ledger,
            &AttestationEvidence::new(true, &records),
        );

        // Both runs touched the task: A created it, B was handed it. Neither
        // is dropped, and each gets the same rollup `query run` would return.
        assert_eq!(
            digest
                .runs
                .iter()
                .map(|run| run.flow_run_id.as_str())
                .collect::<Vec<_>>(),
            [run_a, run_b]
        );
        for run in &digest.runs {
            assert_eq!(run.usage.coverage.attempts_reported, 1);
            assert_eq!(run.usage.tokens.output_tokens.value, 32_842);
            assert_eq!(run.usage.authority, FactAuthority::AdvisoryProviderCapture);
        }

        // A digest whose entries belong to no run lists no run, rather than an
        // empty rollup that reads as a costless window.
        let mut orphan = standup_fixture("00000000-0000-4000-8000-000000000024");
        apply_standup_usage(
            &mut orphan,
            &details,
            &[],
            &ledger,
            &AttestationEvidence::new(true, &records),
        );
        assert!(orphan.runs.is_empty());
        // Nothing to roll up means nothing to state a basis for, and it is what
        // lets `query.standup` skip the chain read entirely (#404).
        assert!(orphan.usage_basis.is_none());
        assert!(standup_touched_runs(&orphan, &details, &ledger).is_empty());
    }

    /// Issue #404: the predicate that lets the RPC layer skip the attestation
    /// chain read is exactly the one `query_run` raises `UnknownJob` from.
    ///
    /// The read is a full parse and hash-verify of the append-only ledger on
    /// every call, before the run id is even known to exist. Deferring it is
    /// only safe if the cheap check and the real one can never disagree, so
    /// this pins them together on both answers: an id nothing knows about, and
    /// each of the sources `query_run` accepts as evidence a run exists.
    #[test]
    fn acceptance_404_the_deferral_predicate_agrees_with_the_run_view_it_gates() {
        let flow_run = "00000000-0000-4000-8000-0000000003a0";
        let details = vec![reconciliation_detail(flow_run)];
        let membership = FlowMembership::default();
        let history = history();
        let unverified = AttestationEvidence::new(true, &[]);

        // Known through the detail row's orchestration.
        assert!(flow_run_exists(
            flow_run,
            &details,
            &[],
            &history,
            &[],
            &membership
        ));
        assert!(query_run(
            flow_run,
            &details,
            &[],
            &history,
            &[],
            "2026-08-01T10:00:12Z".parse().unwrap(),
            &membership,
            &unverified,
        )
        .is_ok());

        // Unknown to every source, so the chain would be read for nothing.
        let absent = "00000000-0000-4000-8000-0000000009f0";
        assert!(!flow_run_exists(
            absent,
            &details,
            &[],
            &history,
            &[],
            &membership
        ));
        assert!(matches!(
            query_run(
                absent,
                &details,
                &[],
                &history,
                &[],
                "2026-08-01T10:00:12Z".parse().unwrap(),
                &membership,
                &unverified,
            ),
            Err(ObservabilityError::UnknownJob(id)) if id == absent
        ));

        // Known only through durable membership -- the source a caller that
        // guessed from the detail rows alone would have missed, turning a real
        // run into a not-found.
        let mut held = FlowMembership::default();
        held.insert(crate::flow_membership::FlowMembershipRecord::new(
            absent.to_owned(),
            "00000000-0000-4000-8000-000000000250".to_owned(),
            crate::flow_membership::MembershipDisposition::Attached,
            Some(7),
            Some("node-7".to_owned()),
        ));
        assert!(flow_run_exists(absent, &details, &[], &history, &[], &held));
        assert!(query_run(
            absent,
            &details,
            &[],
            &history,
            &[],
            "2026-08-01T10:00:12Z".parse().unwrap(),
            &held,
            &unverified,
        )
        .is_ok());
    }

    /// Issue #404: a digest states the three rollup constants once instead of
    /// repeating ~650 bytes of them per run.
    ///
    /// Hoisting is only honest if the fields really are invariant across every
    /// run a window can contain. They are, structurally: `provenance` and
    /// `composition` have one writer (`roll_up`) that assigns them from
    /// compile-time constants with no dependence on the run, and `cost.basis`
    /// has one writer (`UsageCostRollup::default`). This asserts that on real
    /// rollups over two different runs with different membership, and then
    /// asserts the wire keeps the escape hatch: a rollup whose statements are
    /// *not* the constants carries its own inline rather than inheriting a
    /// digest-level claim that would be false for it.
    #[test]
    fn acceptance_404_standup_states_the_rollup_constants_once_without_flattening_a_difference() {
        let run_a = "00000000-0000-4000-8000-0000000003a0";
        let run_b = "00000000-0000-4000-8000-0000000003b0";
        let shared = "00000000-0000-4000-8000-000000000250";
        let details = vec![reconciliation_detail(run_a)];
        let mut ledger = FlowMembership::default();
        ledger.insert(crate::flow_membership::FlowMembershipRecord::new(
            run_b.to_owned(),
            shared.to_owned(),
            crate::flow_membership::MembershipDisposition::Attached,
            Some(7),
            Some("b-node-7".to_owned()),
        ));
        let records = [scrape_attestation(1, shared, 1, 32_842)];
        let mut digest = standup_fixture(shared);
        apply_standup_usage(
            &mut digest,
            &details,
            &[],
            &ledger,
            &AttestationEvidence::new(true, &records),
        );
        assert_eq!(digest.runs.len(), 2);

        // The premise, checked against the rollups themselves rather than
        // assumed: every run states the same three things.
        let basis = digest
            .usage_basis
            .clone()
            .expect("a filled digest states a basis");
        for run in &digest.runs {
            assert_eq!(run.usage.provenance, basis.provenance);
            assert_eq!(run.usage.composition, basis.composition);
            assert_eq!(run.usage.cost.basis, basis.cost_basis);
        }
        assert_eq!(basis, crate::query::StandupUsageBasis::default());

        // On the wire the three appear once, at the digest, and not per run.
        let wire = serde_json::to_value(&digest).unwrap();
        assert_eq!(
            wire["usageBasis"]["provenance"],
            serde_json::json!(basis.provenance)
        );
        assert_eq!(
            wire["usageBasis"]["composition"],
            serde_json::json!(basis.composition)
        );
        assert_eq!(
            wire["usageBasis"]["costBasis"],
            serde_json::json!(basis.cost_basis)
        );
        for run in wire["runs"].as_array().unwrap() {
            assert!(run["usage"].get("provenance").is_none(), "{run}");
            assert!(run["usage"].get("composition").is_none(), "{run}");
            assert!(run["usage"]["cost"].get("basis").is_none(), "{run}");
            // Everything that is genuinely per run is untouched.
            assert_eq!(
                run["usage"]["coverage"]["attemptsReported"],
                serde_json::json!(1)
            );
            assert_eq!(
                run["usage"]["tokens"]["outputTokens"]["value"],
                serde_json::json!(32_842)
            );
        }
        // And it round-trips back to the identical value, constants included.
        let round_tripped: StandupDigest = serde_json::from_value(wire).unwrap();
        assert_eq!(round_tripped, digest);

        // The escape hatch. If a run's statements ever stop matching the
        // constants, the omission stops: a digest may not say something about a
        // run it did not measure.
        let mut divergent = digest.clone();
        divergent.runs[0].usage.composition = "a different composition".to_owned();
        divergent.runs[0].usage.cost.basis = "a different cost basis".to_owned();
        let wire = serde_json::to_value(&divergent).unwrap();
        assert_eq!(
            wire["runs"][0]["usage"]["composition"],
            serde_json::json!("a different composition")
        );
        assert_eq!(
            wire["runs"][0]["usage"]["cost"]["basis"],
            serde_json::json!("a different cost basis")
        );
        assert!(wire["runs"][0]["usage"].get("provenance").is_none());
        assert!(wire["runs"][1]["usage"].get("composition").is_none());
        let round_tripped: StandupDigest = serde_json::from_value(wire).unwrap();
        assert_eq!(round_tripped, divergent);
    }

    /// Issue #404, the half a same-build round-trip cannot see: what an omitted
    /// entry field is filled from.
    ///
    /// Filling it from the *reader's* compiled constants makes the digest state
    /// one thing and every entry in it state another, and it does so silently
    /// on exactly the fleet this runs on — the coordinator pin is routinely one
    /// generation behind the workers, so a `query standup` across that gap is
    /// the normal case, not the exotic one. The producer's own answer travels
    /// in the payload; the entries have to inherit *that*.
    ///
    /// So this deserializes a payload whose `usageBasis` is deliberately not
    /// this build's constants — which is what a digest from another generation
    /// looks like — and asserts the entries agree with the payload rather than
    /// with the reader.
    #[test]
    fn acceptance_404_an_omitted_entry_field_is_filled_from_the_payloads_basis_not_the_readers() {
        let run_a = "00000000-0000-4000-8000-0000000003a0";
        let shared = "00000000-0000-4000-8000-000000000250";
        let details = vec![reconciliation_detail(run_a)];
        let membership = FlowMembership::default();
        let records = [scrape_attestation(1, shared, 1, 32_842)];
        let mut digest = standup_fixture(shared);
        apply_standup_usage(
            &mut digest,
            &details,
            &[],
            &membership,
            &AttestationEvidence::new(true, &records),
        );
        assert_eq!(digest.runs.len(), 1);

        // A payload from a build whose rollup statements are not ours.
        let mut wire = serde_json::to_value(&digest).unwrap();
        wire["usageBasis"]["provenance"] = serde_json::json!("another generation's provenance");
        wire["usageBasis"]["composition"] = serde_json::json!("another generation's composition");
        wire["usageBasis"]["costBasis"] = serde_json::json!("another generation's cost basis");
        // The entries really do omit all three: that is the case under test.
        assert!(wire["runs"][0]["usage"].get("provenance").is_none());
        assert!(wire["runs"][0]["usage"].get("composition").is_none());
        assert!(wire["runs"][0]["usage"]["cost"].get("basis").is_none());

        let read: StandupDigest = serde_json::from_value(wire).unwrap();
        let basis = read.usage_basis.clone().unwrap();
        assert_eq!(read.runs[0].usage.provenance, basis.provenance);
        assert_eq!(read.runs[0].usage.composition, basis.composition);
        assert_eq!(read.runs[0].usage.cost.basis, basis.cost_basis);
        // Said the other way round, because this is the failure that matters:
        // the reader must not have substituted its own strings.
        assert_ne!(
            read.runs[0].usage.provenance,
            crate::usage_rollup::ROLLUP_PROVENANCE
        );
        assert_ne!(
            read.runs[0].usage.composition,
            crate::usage_rollup::ROLLUP_COMPOSITION
        );
        assert_ne!(
            read.runs[0].usage.cost.basis,
            crate::usage_rollup::ROLLUP_COST_BASIS
        );

        // A payload with no basis at all — what a digest produced before
        // `usageBasis` existed looks like — still reads as this build's
        // constants rather than as empty strings.
        let mut legacy = serde_json::to_value(&digest).unwrap();
        legacy.as_object_mut().unwrap().remove("usageBasis");
        let read: StandupDigest = serde_json::from_value(legacy).unwrap();
        assert!(read.usage_basis.is_none());
        assert_eq!(
            read.runs[0].usage.provenance,
            crate::usage_rollup::ROLLUP_PROVENANCE
        );
        assert_eq!(
            read.runs[0].usage.composition,
            crate::usage_rollup::ROLLUP_COMPOSITION
        );
        assert_eq!(
            read.runs[0].usage.cost.basis,
            crate::usage_rollup::ROLLUP_COST_BASIS
        );
    }

    fn reader_state_fixture(archived: &[(&str, Option<&str>)]) -> ReaderState {
        use crate::reader_state::set_reader_state;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("reader-state.jsonl");
        for (flow_run, tag) in archived {
            set_reader_state(
                &path,
                flow_run,
                crate::reader_state::ReaderStateUpdate {
                    archived: Some(true),
                    triage_tag: tag.map(|tag| Some(tag.to_owned())),
                },
            )
            .unwrap();
        }
        ReaderState::read(&path).unwrap()
    }

    #[test]
    fn apply_reader_state_to_run_exposes_archived_and_tag_without_changing_state() {
        let flow_run = "00000000-0000-4000-8000-000000000249";
        let node = flow_node_detail(flow_run, RowStatus::Completed);
        let witness = terminal_witness(
            &node.task_uuid,
            Verdict::Pass,
            node.orchestration.clone().unwrap(),
        );
        let mut view = query_run(
            flow_run,
            std::slice::from_ref(&node),
            &[],
            &history(),
            std::slice::from_ref(&witness),
            parse_timestamp("2026-08-01T10:00:12.000Z").unwrap(),
            &FlowMembership::default(),
            &AttestationEvidence::unavailable(),
        )
        .unwrap();
        assert_eq!(view.state, RunState::Complete);
        assert!(!view.archived);
        assert_eq!(view.triage_tag, None);

        let reader_state = reader_state_fixture(&[(flow_run, Some("needs-followup"))]);
        apply_reader_state_to_run(&mut view, &reader_state);
        assert!(view.archived);
        assert_eq!(view.triage_tag.as_deref(), Some("needs-followup"));
        // Reader-state is not a fact about execution: it must never move
        // `state` the way a durable rollover does.
        assert_eq!(view.state, RunState::Complete);

        let mut unrelated = view.clone();
        unrelated.flow_run_id = "00000000-0000-4000-8000-0000000002ff".to_owned();
        unrelated.archived = false;
        unrelated.triage_tag = None;
        apply_reader_state_to_run(&mut unrelated, &reader_state);
        assert!(!unrelated.archived);
        assert_eq!(unrelated.triage_tag, None);
    }

    #[test]
    fn apply_reader_state_to_jobs_hides_archived_run_jobs_by_default_and_counts_hidden() {
        let archived_run = "00000000-0000-4000-8000-000000000260";
        let live_run = "00000000-0000-4000-8000-000000000261";
        let hidden_node = flow_node_detail(archived_run, RowStatus::Completed);
        let mut visible_node = flow_node_detail(live_run, RowStatus::Completed);
        visible_node.task_uuid = "00000000-0000-4000-8000-000000000262".to_owned();
        let mut items = vec![
            build_summary(
                &hidden_node.task_uuid,
                Some(&hidden_node),
                None,
                &[],
                &[],
                Vec::new(),
                &BTreeMap::new(),
            ),
            build_summary(
                &visible_node.task_uuid,
                Some(&visible_node),
                None,
                &[],
                &[],
                Vec::new(),
                &BTreeMap::new(),
            ),
        ];
        let reader_state = reader_state_fixture(&[(archived_run, None)]);

        let mut default_view = items.clone();
        let hidden = apply_reader_state_to_jobs(
            &mut default_view,
            &reader_state,
            JobsReaderStateMode::Broad {
                include_archived: false,
            },
        );
        assert_eq!(hidden, 1);
        assert_eq!(default_view.len(), 1);
        assert_eq!(default_view[0].anchor, visible_node.task_uuid);
        assert!(!default_view[0].archived);

        let mut included_view = items.clone();
        let hidden = apply_reader_state_to_jobs(
            &mut included_view,
            &reader_state,
            JobsReaderStateMode::Broad {
                include_archived: true,
            },
        );
        assert_eq!(hidden, 0);
        assert_eq!(included_view.len(), 2);
        let flagged = included_view
            .iter()
            .find(|item| item.anchor == hidden_node.task_uuid)
            .unwrap();
        assert!(flagged.archived);
        let unflagged = included_view
            .iter()
            .find(|item| item.anchor == visible_node.task_uuid)
            .unwrap();
        assert!(!unflagged.archived);

        let mut explicit_view = items.clone();
        let hidden = apply_reader_state_to_jobs(
            &mut explicit_view,
            &reader_state,
            JobsReaderStateMode::ExplicitLookup {
                flow_run: archived_run,
            },
        );
        assert_eq!(hidden, 0);
        assert_eq!(explicit_view.len(), 2);
        assert!(
            explicit_view
                .iter()
                .find(|item| item.anchor == hidden_node.task_uuid)
                .unwrap()
                .archived
        );

        // The returned hidden count always equals what was actually removed
        // from the list beside it -- never a separately recomputed number.
        items.clear();
        assert_eq!(
            apply_reader_state_to_jobs(
                &mut items,
                &reader_state,
                JobsReaderStateMode::Broad {
                    include_archived: false,
                },
            ),
            0
        );
    }

    #[test]
    fn explicit_flow_run_projects_membership_only_identity_and_selected_archive_state() {
        let archived_run = "00000000-0000-4000-8000-000000000415";
        let member = "00000000-0000-4000-8000-000000000416";
        let mut membership = FlowMembership::default();
        membership.insert(crate::flow_membership::FlowMembershipRecord::new(
            archived_run.to_owned(),
            member.to_owned(),
            crate::flow_membership::MembershipDisposition::Attached,
            Some(1),
            Some("membership-only".to_owned()),
        ));

        let mut result = query_jobs(
            &[],
            &[],
            &history(),
            &[],
            &BTreeMap::new(),
            &JobsFilter {
                flow_run: Some(archived_run.to_owned()),
                ..JobsFilter::default()
            },
            &membership,
        )
        .unwrap();
        assert_eq!(result.flow_run_tasks, Some(1));
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].anchor, member);
        assert_eq!(result.items[0].task_uuid.as_deref(), Some(member));
        assert!(result.items[0].orchestration.is_none());

        let reader_state = reader_state_fixture(&[(archived_run, None)]);
        assert_eq!(
            apply_reader_state_to_jobs(
                &mut result.items,
                &reader_state,
                JobsReaderStateMode::ExplicitLookup {
                    flow_run: archived_run,
                },
            ),
            0
        );
        assert_eq!(result.items.len(), 1);
        assert!(result.items[0].archived);
    }

    #[test]
    fn apply_reader_state_to_standup_hides_entries_and_runs_and_the_hidden_count_matches_what_it_removed(
    ) {
        let archived_run = "00000000-0000-4000-8000-000000000270";
        let live_run = "00000000-0000-4000-8000-000000000271";
        let hidden_task = "00000000-0000-4000-8000-000000000272";
        let visible_task = "00000000-0000-4000-8000-000000000273";
        let mut hidden_detail = detail(RowStatus::Completed);
        hidden_detail.task_uuid = hidden_task.to_owned();
        hidden_detail.orchestration =
            Some(flow_orchestration(archived_run, 1, "agent-hidden", None));
        let mut visible_detail = detail(RowStatus::Completed);
        visible_detail.task_uuid = visible_task.to_owned();
        visible_detail.orchestration = Some(flow_orchestration(live_run, 1, "agent-visible", None));
        let details = [hidden_detail, visible_detail];

        let empty_usage = || {
            roll_up(
                std::iter::empty::<&str>(),
                &AttestationEvidence::unavailable(),
            )
        };

        let mut digest = standup_fixture(hidden_task);
        digest.completed.push(crate::query::CompletedEntry {
            task_uuid: Some(visible_task.to_owned()),
            task_ref: None,
            gpu_seconds: None,
            verdict: Verdict::Pass,
            session_ref: None,
            gh_origin: None,
        });
        digest.runs = vec![
            StandupRunUsage {
                flow_run_id: archived_run.to_owned(),
                usage: empty_usage(),
            },
            StandupRunUsage {
                flow_run_id: live_run.to_owned(),
                usage: empty_usage(),
            },
        ];
        let before_completed = digest.completed.len();

        let reader_state = reader_state_fixture(&[(archived_run, None)]);
        let hidden =
            apply_reader_state_to_standup(&mut digest, &details, &[], &reader_state, false);

        assert_eq!(hidden, 1);
        assert_eq!(digest.archived_hidden, 1);
        // Exactly one `runs` row was archived too, and it must be counted
        // separately from the task-entry count above.
        assert_eq!(digest.archived_runs_hidden, 1);
        // What was actually removed: exactly the archived run's entry, and
        // exactly one entry short of the pre-filter count -- the hidden
        // count must not be able to disagree with the rows beside it.
        assert_eq!(digest.completed.len(), before_completed - 1);
        assert!(digest
            .completed
            .iter()
            .all(|entry| entry.task_uuid.as_deref() != Some(hidden_task)));
        assert_eq!(
            digest
                .runs
                .iter()
                .map(|run| run.flow_run_id.as_str())
                .collect::<Vec<_>>(),
            [live_run]
        );

        // With include_archived, nothing is hidden and both counts say so.
        let mut included = standup_fixture(hidden_task);
        included.runs = vec![StandupRunUsage {
            flow_run_id: archived_run.to_owned(),
            usage: empty_usage(),
        }];
        let hidden =
            apply_reader_state_to_standup(&mut included, &details, &[], &reader_state, true);
        assert_eq!(hidden, 0);
        assert_eq!(included.archived_hidden, 0);
        assert_eq!(included.archived_runs_hidden, 0);
        assert_eq!(included.completed.len(), 1);
        assert_eq!(included.runs.len(), 1);
    }

    #[test]
    fn archived_only_reused_task_contributes_zero_by_default_and_one_forty_two_when_included() {
        let archived_run = "00000000-0000-4000-8000-000000000274";
        let task = "00000000-0000-4000-8000-000000000275";
        let mut task_detail = detail(RowStatus::Completed);
        task_detail.task_uuid = task.to_owned();
        task_detail.orchestration =
            Some(flow_orchestration(archived_run, 1, "agent-archived", None));
        let details = [task_detail];
        let mut head = ChainHead::default();
        let witness = [
            standup_aggregate_witness(&mut head, task, archived_run, 1, LaborClass::Fresh, 42.0),
            standup_aggregate_witness(&mut head, task, archived_run, 2, LaborClass::Reused, 999.0),
        ];
        let reader_state = reader_state_fixture(&[(archived_run, None)]);

        let mut default_view = standup_fixture(task);
        default_view.completed[0].verdict = Verdict::Reused;
        default_view.completed[0].gpu_seconds = Some(999.0);
        default_view.reused = 1;
        default_view.canonical_gpu_seconds = 42.0;
        apply_reader_state_to_standup(&mut default_view, &details, &witness, &reader_state, false);
        assert!(default_view.completed.is_empty());
        assert_eq!(default_view.reused, 0);
        assert_eq!(default_view.canonical_gpu_seconds, 0.0);

        let mut included_view = standup_fixture(task);
        included_view.completed[0].verdict = Verdict::Reused;
        included_view.completed[0].gpu_seconds = Some(999.0);
        apply_reader_state_to_standup(&mut included_view, &details, &witness, &reader_state, true);
        assert_eq!(included_view.completed.len(), 1);
        assert_eq!(included_view.reused, 1);
        assert_eq!(included_view.canonical_gpu_seconds, 42.0);
    }

    #[test]
    fn mixed_archive_filter_keeps_only_visible_witness_contributions() {
        let archived_run = "00000000-0000-4000-8000-000000000276";
        let visible_run = "00000000-0000-4000-8000-000000000277";
        let archived_task = "00000000-0000-4000-8000-000000000278";
        let visible_task = "00000000-0000-4000-8000-000000000279";
        let mut archived_detail = detail(RowStatus::Completed);
        archived_detail.task_uuid = archived_task.to_owned();
        archived_detail.orchestration =
            Some(flow_orchestration(archived_run, 1, "agent-archived", None));
        let mut visible_detail = detail(RowStatus::Completed);
        visible_detail.task_uuid = visible_task.to_owned();
        visible_detail.orchestration =
            Some(flow_orchestration(visible_run, 1, "agent-visible", None));
        let details = [archived_detail, visible_detail];
        let mut head = ChainHead::default();
        let witness = [
            standup_aggregate_witness(
                &mut head,
                archived_task,
                archived_run,
                1,
                LaborClass::Fresh,
                42.0,
            ),
            standup_aggregate_witness(
                &mut head,
                archived_task,
                archived_run,
                2,
                LaborClass::Reused,
                999.0,
            ),
            standup_aggregate_witness(
                &mut head,
                visible_task,
                visible_run,
                1,
                LaborClass::Fresh,
                7.0,
            ),
            standup_aggregate_witness(
                &mut head,
                visible_task,
                visible_run,
                2,
                LaborClass::Reused,
                999.0,
            ),
        ];
        let mut digest = standup_fixture(archived_task);
        digest.completed[0].verdict = Verdict::Reused;
        digest.completed[0].gpu_seconds = Some(999.0);
        digest.completed.push(crate::query::CompletedEntry {
            task_uuid: Some(visible_task.to_owned()),
            task_ref: None,
            gpu_seconds: Some(999.0),
            verdict: Verdict::Reused,
            session_ref: None,
            gh_origin: None,
        });
        digest.reused = 2;
        digest.canonical_gpu_seconds = 49.0;

        let reader_state = reader_state_fixture(&[(archived_run, None)]);
        apply_reader_state_to_standup(&mut digest, &details, &witness, &reader_state, false);

        assert_eq!(digest.completed.len(), 1);
        assert_eq!(digest.completed[0].task_uuid.as_deref(), Some(visible_task));
        assert_eq!(digest.reused, 1);
        assert_eq!(digest.canonical_gpu_seconds, 7.0);
    }

    /// The L3/L7 seam: `usage_basis` is present exactly when `runs` is
    /// non-empty, across the COMPOSITION of the two calls the `query.standup`
    /// handler makes — not just within either one.
    ///
    /// `apply_standup_usage` (#404) sets the basis when it leaves a non-empty
    /// `runs`; `apply_reader_state_to_standup` (#389) can then hide every run.
    /// Both lanes tested their own function in isolation and neither gate
    /// could see the pair, so this asserts the invariant where a consumer
    /// actually observes it: on the digest the handler hands out, and on the
    /// wire, where both fields are `skip_serializing_if`-omitted and a basis
    /// surviving alone would be a statement about runs the payload does not
    /// contain.
    #[test]
    fn standup_usage_basis_is_present_exactly_when_runs_is_after_reader_state_is_applied() {
        let run = "00000000-0000-4000-8000-0000000004c0";
        let task = "00000000-0000-4000-8000-000000000250";
        let details = vec![reconciliation_detail(run)];
        let records = [scrape_attestation(1, task, 1, 32_842)];
        let evidence = AttestationEvidence::new(true, &records);

        // As the handler builds it: usage first, so the digest carries a run
        // and the basis that describes how it was summed.
        let mut digest = standup_fixture(task);
        apply_standup_usage(
            &mut digest,
            &details,
            &[],
            &FlowMembership::default(),
            &evidence,
        );
        assert_eq!(digest.runs.len(), 1);
        assert!(
            digest.usage_basis.is_some(),
            "precondition: a digest with runs states a basis"
        );

        // Then reader-state, hiding the only run there is.
        let reader_state = reader_state_fixture(&[(run, None)]);
        apply_reader_state_to_standup(&mut digest, &details, &[], &reader_state, false);

        assert!(digest.runs.is_empty());
        assert_eq!(digest.archived_runs_hidden, 1);
        assert!(
            digest.usage_basis.is_none(),
            "a digest that shows no run must state no basis for summing runs"
        );

        // The wire shape a consumer sees: neither key, and the count that
        // says the runs were hidden rather than never there.
        let wire = serde_json::to_value(&digest).unwrap();
        assert!(wire.get("runs").is_none());
        assert!(wire.get("usageBasis").is_none());
        assert_eq!(wire["archivedRunsHidden"], serde_json::json!(1));

        // The other half of "exactly when": a run that survives keeps the
        // basis, so the clear is conditional on emptiness and not on
        // reader-state having run at all.
        let mut kept = standup_fixture(task);
        apply_standup_usage(
            &mut kept,
            &details,
            &[],
            &FlowMembership::default(),
            &evidence,
        );
        let untouched = reader_state_fixture(&[]);
        apply_reader_state_to_standup(&mut kept, &details, &[], &untouched, false);
        assert_eq!(kept.runs.len(), 1);
        assert_eq!(kept.archived_runs_hidden, 0);
        assert!(kept.usage_basis.is_some());
    }

    /// HIGH-3's exact reproduction: a run that only ATTACHED a task (the
    /// W-316 shape -- present in `digest.runs` via `apply_standup_usage`'s
    /// membership union, per its own doc comment, without being that task's
    /// *creating* run) is archived. Task-entry attribution is
    /// creating-run-only by design, so `archived_hidden` correctly stays 0
    /// -- but the attached run's cost row is still removed from `runs`, and
    /// that removal must not be invisible.
    #[test]
    fn apply_reader_state_to_standup_counts_an_attach_only_archived_run_that_hides_no_task_entry() {
        let creating_run = "00000000-0000-4000-8000-000000000290";
        let attach_only_run = "00000000-0000-4000-8000-000000000291";
        let task = "00000000-0000-4000-8000-000000000292";
        let mut only_detail = detail(RowStatus::Completed);
        only_detail.task_uuid = task.to_owned();
        only_detail.orchestration = Some(flow_orchestration(creating_run, 1, "agent-only", None));
        let details = [only_detail];

        let empty_usage = || {
            roll_up(
                std::iter::empty::<&str>(),
                &AttestationEvidence::unavailable(),
            )
        };
        let mut digest = standup_fixture(task);
        digest.runs = vec![
            StandupRunUsage {
                flow_run_id: creating_run.to_owned(),
                usage: empty_usage(),
            },
            StandupRunUsage {
                flow_run_id: attach_only_run.to_owned(),
                usage: empty_usage(),
            },
        ];

        let reader_state = reader_state_fixture(&[(attach_only_run, None)]);
        let hidden =
            apply_reader_state_to_standup(&mut digest, &details, &[], &reader_state, false);

        assert_eq!(hidden, 0, "no task entry's CREATING run is archived");
        assert_eq!(digest.archived_hidden, 0);
        assert_eq!(
            digest.archived_runs_hidden, 1,
            "the attach-only run's cost row was removed from `runs` and must be counted"
        );
        assert_eq!(
            digest
                .runs
                .iter()
                .map(|run| run.flow_run_id.as_str())
                .collect::<Vec<_>>(),
            [creating_run]
        );
    }

    /// HIGH-4: proves `archived_hidden` is a before/after difference over the
    /// digest's own collections, never a recount over `details`. A detail
    /// whose run is archived but which produced no digest entry at all
    /// (still pending, dropped by a `source` filter, or otherwise never
    /// bucketed by `query_standup`) must contribute nothing to the count --
    /// nothing was hidden from a reader who never saw it. A recount over
    /// `details` cannot express that and gets it wrong, which is exactly
    /// what mutation M1 in the round-1 eval substituted and which survived
    /// 676/676 tests. This test is the one named in this function's own doc
    /// comment as failing against that mutation.
    #[test]
    fn apply_reader_state_to_standup_never_counts_an_archived_detail_that_produced_no_digest_entry()
    {
        let archived_run = "00000000-0000-4000-8000-000000000295";
        let phantom_task = "00000000-0000-4000-8000-000000000296";
        let mut phantom = detail(RowStatus::Pending);
        phantom.task_uuid = phantom_task.to_owned();
        phantom.orchestration = Some(flow_orchestration(archived_run, 1, "agent-phantom", None));
        let details = [phantom];

        // The digest never mentions `phantom_task` at all: exactly the
        // "produced no bucket entry" case.
        let mut digest = standup_fixture("00000000-0000-4000-8000-000000000297");
        let reader_state = reader_state_fixture(&[(archived_run, None)]);

        let hidden =
            apply_reader_state_to_standup(&mut digest, &details, &[], &reader_state, false);
        assert_eq!(
            hidden, 0,
            "the archived detail never appeared in any digest bucket, so nothing was hidden"
        );
        assert_eq!(digest.archived_hidden, 0);

        // What a recount over `details` -- mutation M1's exact shape --
        // would compute instead, and get wrong:
        let recount = details
            .iter()
            .filter(|item| {
                item.orchestration
                    .as_ref()
                    .is_some_and(|o| reader_state.is_archived(o.flow_run_id()))
            })
            .count();
        assert_eq!(recount, 1);
        assert_ne!(
            recount, digest.archived_hidden,
            "a recount over `details` disagrees with what was actually hidden from the \
             digest -- this is why archived_hidden must stay a before/after difference, \
             never a recount"
        );
    }

    /// Round-2 HIGH-11: `archived_hidden` claims to cover four collections,
    /// and before this test only `completed` was pinned -- dropping
    /// `cancelled`, `gate_fails` or `in_flight` from the count left all 679
    /// tests green while the digest under-reported what it withheld. One
    /// archived entry is placed in EVERY filtered collection, so no single
    /// collection can be dropped from the count without this going red.
    #[test]
    fn apply_reader_state_to_standup_counts_a_removal_from_every_collection_it_filters() {
        let archived_run = "00000000-0000-4000-8000-0000000002a0";
        let empty_usage = || {
            roll_up(
                std::iter::empty::<&str>(),
                &AttestationEvidence::unavailable(),
            )
        };

        // One task per bucket, every one created by the archived run.
        let tasks = [
            "00000000-0000-4000-8000-0000000002a1",
            "00000000-0000-4000-8000-0000000002a2",
            "00000000-0000-4000-8000-0000000002a3",
            "00000000-0000-4000-8000-0000000002a4",
        ];
        let details = tasks
            .iter()
            .enumerate()
            .map(|(index, task)| {
                let mut detail = detail(RowStatus::Completed);
                detail.task_uuid = (*task).to_owned();
                detail.orchestration = Some(flow_orchestration(
                    archived_run,
                    u64::try_from(index).unwrap(),
                    "agent",
                    None,
                ));
                detail
            })
            .collect::<Vec<_>>();

        let completed_entry = |task: &str| crate::query::CompletedEntry {
            task_uuid: Some(task.to_owned()),
            task_ref: None,
            gpu_seconds: None,
            verdict: Verdict::Pass,
            session_ref: None,
            gh_origin: None,
        };
        let mut digest = standup_fixture(tasks[0]);
        assert_eq!(digest.completed.len(), 1, "fixture seeds `completed`");
        digest.gate_fails = vec![completed_entry(tasks[1])];
        digest.cancelled = vec![completed_entry(tasks[2])];
        digest.in_flight = vec![crate::query::InFlightEntry {
            task_uuid: Some(tasks[3].to_owned()),
            task_ref: None,
            session_ref: None,
            state: "running".to_owned(),
            last_event_at: None,
            gh_origin: None,
        }];
        digest.runs = vec![StandupRunUsage {
            flow_run_id: archived_run.to_owned(),
            usage: empty_usage(),
        }];

        let reader_state = reader_state_fixture(&[(archived_run, None)]);
        let hidden =
            apply_reader_state_to_standup(&mut digest, &details, &[], &reader_state, false);

        // One entry removed from each of the four task collections. Drop any
        // single collection from the count and this is 3, not 4.
        assert_eq!(hidden, 4);
        assert_eq!(digest.archived_hidden, 4);
        assert_eq!(digest.archived_runs_hidden, 1);
        assert!(digest.completed.is_empty());
        assert!(digest.gate_fails.is_empty());
        assert!(digest.cancelled.is_empty());
        assert!(digest.in_flight.is_empty());
        assert!(digest.runs.is_empty());
    }

    /// A *further* predicate on an already-enumerated collection — one this
    /// test was not written for — must still be caught: the conservation
    /// check inside `apply_reader_state_to_standup` compares
    /// `filterable_entries` before and after, so a removal there that
    /// reaches no counter fails. Round-2 MUT-C is exactly that shape and
    /// this test catches it.
    ///
    /// Note the reach honestly. This test computes `before` and `removed`
    /// by calling `filterable_entries` itself, so it is exactly as blind as
    /// that enumerator: it cannot catch a removal from a collection the
    /// enumerator does not name (round-3 MUTATION H). What stops that case
    /// is not this test but the exhaustive destructure in
    /// `filterable_entries`, which turns a new `StandupDigest` field into a
    /// compile error. Also exercises the opted-in path, where the correct
    /// answer is "nothing removed, both counts zero".
    #[test]
    fn apply_reader_state_to_standup_conserves_entries_between_removals_and_counts() {
        let archived_run = "00000000-0000-4000-8000-0000000002b0";
        let task = "00000000-0000-4000-8000-0000000002b1";
        let mut only = detail(RowStatus::Completed);
        only.task_uuid = task.to_owned();
        only.orchestration = Some(flow_orchestration(archived_run, 1, "agent", None));
        let details = [only];
        // Carries a triage tag as well as `archived`, so a future edit that
        // filters on any *other* reader-state property is inside this
        // test's reach rather than only the daemon-level one's.
        let reader_state = reader_state_fixture(&[(archived_run, Some("needs-followup"))]);
        let empty_usage = || {
            roll_up(
                std::iter::empty::<&str>(),
                &AttestationEvidence::unavailable(),
            )
        };

        for include_archived in [false, true] {
            let mut digest = standup_fixture(task);
            digest.runs = vec![StandupRunUsage {
                flow_run_id: archived_run.to_owned(),
                usage: empty_usage(),
            }];
            let before = filterable_entries(&digest);

            apply_reader_state_to_standup(
                &mut digest,
                &details,
                &[],
                &reader_state,
                include_archived,
            );

            let removed = before - filterable_entries(&digest);
            assert_eq!(
                removed,
                digest.archived_hidden + digest.archived_runs_hidden,
                "every removal must reach a counter (include_archived={include_archived})"
            );
            if include_archived {
                assert_eq!(removed, 0);
                assert_eq!(digest.archived_hidden, 0);
                assert_eq!(digest.archived_runs_hidden, 0);
            } else {
                assert_eq!(removed, 2, "one task entry and one run row");
            }
        }
    }
}
