use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::history::{LifecycleRecord, LifecycleSnapshot, RetentionMetadata};
use crate::journal::TallyEvent;
use crate::provenance::{Orchestration, TaskRef};
use crate::query::{
    GhOriginProjection, HeadroomSignal, RowStatus, QUERY_PROTOCOL_VERSION, QUERY_SCHEMA_VERSION,
};
use crate::taskdb::{
    related_trigger_from_gh_origin, AdmissionOrigin, ProducerOrigin, RelatedTrigger, RowSeed,
    WorkspaceMetadata,
};
use crate::witness::{
    counts_toward_canonical_gpu_seconds, AttestationRecord, AuthorshipSession, AuthorshipStatus,
    Charge, LaborClass, Verdict, VerifyReport, WitnessRecord,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FactAuthority {
    DurableAdmissionFact,
    TallyLifecycleObservation,
    CanonicalWitnessFact,
    AdvisoryAttestation,
    AdvisoryProviderCapture,
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
    pub snapshot: QuerySnapshotMetadata,
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
}

pub fn query_jobs(
    details: &[RowDetailFact],
    live: &[LiveJobFact],
    history: &LifecycleSnapshot,
    witness: &[WitnessRecord],
    pool_signals: &BTreeMap<String, HeadroomSignal>,
    filter: &JobsFilter,
) -> Result<CollectionEnvelope<JobSummary>, ObservabilityError> {
    let since = filter.since.as_deref().map(parse_timestamp).transpose()?;
    let until = filter.until.as_deref().map(parse_timestamp).transpose()?;
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

    let detail_by_task = details
        .iter()
        .map(|detail| (detail.task_uuid.as_str(), detail))
        .collect::<BTreeMap<_, _>>();
    let live_by_task = live
        .iter()
        .map(|fact| (fact.anchor.as_str(), fact))
        .collect::<BTreeMap<_, _>>();
    let mut items = Vec::new();
    for anchor in anchors {
        let events = history
            .records
            .iter()
            .filter(|record| record.fields.task_uuid == anchor)
            .collect::<Vec<_>>();
        let witnesses = witness
            .iter()
            .filter(|record| {
                record.task_uuid.as_deref() == Some(anchor.as_str())
                    || (record.task_uuid.is_none() && anchor == format!("witness:{}", record.seq))
            })
            .collect::<Vec<_>>();
        let summary = build_summary(
            &anchor,
            detail_by_task.get(anchor.as_str()).copied(),
            live_by_task.get(anchor.as_str()).copied(),
            &events,
            &witnesses,
            children.get(&anchor).cloned().unwrap_or_default(),
            pool_signals,
        );
        if matches_jobs_filter(&summary, filter, since, until) {
            items.push(summary);
        }
    }
    Ok(CollectionEnvelope {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        items,
        next_cursor: None,
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
) -> Result<CollectionEnvelope<LifecycleEventProjection>, ObservabilityError> {
    let since = filter.since.as_deref().map(parse_timestamp).transpose()?;
    let until = filter.until.as_deref().map(parse_timestamp).transpose()?;
    let flow_tasks = filter
        .flow_run
        .as_deref()
        .map(|flow_run| flow_run_tasks(flow_run, details, witness));
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
    pub counts: RunTaskCounts,
    pub tasks: Vec<RunTaskProjection>,
    pub current_nodes: Vec<RunNodeProjection>,
    pub failures: Vec<RunFailureProjection>,
    pub snapshot: QuerySnapshotMetadata,
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

/// Project one flow run without returning the large argv, brief, evidence, and
/// provenance fields carried by the general job collection. For spec-build,
/// the schema-validated reconciliation result is the task-state source; live
/// rows and canonical terminal witnesses only advance or block that view.
pub fn query_run(
    flow_run: &str,
    details: &[RowDetailFact],
    live: &[LiveJobFact],
    history: &LifecycleSnapshot,
    witness: &[WitnessRecord],
    now: DateTime<Utc>,
) -> Result<RunView, ObservabilityError> {
    let flow_tasks = flow_run_tasks(flow_run, details, witness);
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
    if flow_tasks.is_empty()
        && parent_detail.is_none()
        && parent_live.is_none()
        && parent_events.is_empty()
        && parent_witness.is_empty()
    {
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
        });
    }
    failures.sort_by_key(|failure| (failure.ordinal, failure.task_uuid.clone()));

    let run_active = parent
        .as_ref()
        .is_some_and(|parent| parent.live_state.is_some())
        || !current_nodes.is_empty();
    let mut tasks = Vec::new();
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
    } else if !failures.is_empty() || parent_failed {
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
        counts,
        tasks,
        current_nodes,
        failures,
        snapshot: snapshot_metadata(history, witness),
    })
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
) -> Result<CollectionEnvelope<ProofView>, ObservabilityError> {
    let mut nodes = details
        .iter()
        .filter_map(|detail| {
            let orchestration = detail.orchestration.as_ref()?;
            (orchestration.flow_run_id() == flow_run)
                .then(|| (orchestration.node_ordinal(), detail.task_uuid.clone()))
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
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> bool {
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
        || filter.flow_run.as_deref().is_some_and(|value| {
            job.orchestration
                .as_ref()
                .is_none_or(|orchestration| orchestration.flow_run_id() != value)
        })
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

/// Every task UUID a flow run admitted, from durable rows and the witness chain.
///
/// A lifecycle event carries no orchestration capsule, so a `--flow-run` filter
/// has to resolve the run's nodes from the two records that do carry one.
fn flow_run_tasks(
    flow_run: &str,
    details: &[RowDetailFact],
    witness: &[WitnessRecord],
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
        build_record(
            WitnessBody {
                task_uuid: Some(task_uuid.to_owned()),
                transition_timestamp: "2026-08-01T10:00:12.000Z".to_owned(),
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
                result_revision: None,
                authorship: None,
                authorship_sessions: None,
            },
            &ChainHead::default(),
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
    fn protocol_4_authority_vocabulary_is_byte_stable() {
        assert_eq!(QUERY_PROTOCOL_VERSION, 4);
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
        )
        .unwrap();
        assert!(finished.tasks.is_empty());
        assert_eq!(finished.state, RunState::Complete);
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
                    "ref": "refs/tags/tally/spec-build/v1/crm/c01",
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
                "ref": "refs/tags/tally/spec-build/v1/crm/c02",
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
        )
        .unwrap();
        let projected = jobs.items[0].authorship.as_ref().unwrap();
        assert_eq!(jobs.protocol_version, 4);
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
}
