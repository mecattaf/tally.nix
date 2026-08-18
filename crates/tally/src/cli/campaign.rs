use super::text::compact_text;
use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsString};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::process::{Command as ProcessCommand, Stdio};

use chrono::{DateTime, SecondsFormat, TimeDelta};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tally_core::adapters::{AdapterConfig, AdapterHardening, ScrapeMode, ScrapeStream};
use tally_core::attempt_receipts::{
    validate_attempt_receipt_stamp, AttemptReceiptAuthorityV1, ATTEMPT_RECEIPT_AUTHORITY_FILE,
    ATTEMPT_RECEIPT_SCHEMA_VERSION, LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION,
    MAX_TASK_LIFETIME_ATTEMPTS,
};
use tally_core::campaign_contract::{
    admit_manifest_value, task_completion_revision, task_input_epoch, task_input_hash,
    validate_agent, validate_argv, validate_gates, CampaignAgent, CampaignGate, CampaignManifest,
    CampaignRepository, CampaignSteward, CanonicalCampaignGraphV1, CanonicalCampaignTaskV1,
    BRIEF_SENTINEL, CAMPAIGN_SCHEMA_VERSION, DEFAULT_AGENT_PRIORITY, DEFAULT_AGENT_RUNTIME_MAX_SEC,
    DEFAULT_DRIVER_RUNTIME_MAX_SEC, DEFAULT_MAX_TASKS, DEFAULT_STEWARD_FINAL_MESSAGE_PATTERN,
    DEFAULT_STEWARD_RUNTIME_MAX_SEC, MAX_CAMPAIGN_TASKS,
};
use tally_core::campaign_folds::{
    campaign_digest, render_campaign_summary, stable_publish_branch, stage_scoped_summary_ref,
    BlockedFact, CampaignDigest, CampaignReconciliation, CampaignSource, CheckpointFact,
    DeferralFact, DiagnosisFact, MergedFact, ReconciledTask, RetryFact, TALLY_REVISION_PREFIX,
    TALLY_TASK_PREFIX,
};
use tally_core::campaign_lease::{
    lease_disposition, CampaignActivation, CampaignLapseV1, CampaignLeaseDisposition,
    CampaignLeaseError, CampaignLeaseFacts, CampaignLeaseGuard, CampaignLeaseStore,
    CampaignLeaseTask,
};
use tally_core::campaign_poll::{CampaignDigestMismatch, CampaignPollEvent, CampaignPollStatus};
use tally_core::campaign_registry::{
    CampaignRegistration, CampaignRegistrationV4, CampaignRegistry, REGISTRY_SCHEMA_VERSION,
};
use tally_core::config::{PoolConfig, ResourceKind};
use tally_core::gate_budget::{resolve_gate_budget, GateBudget, GATE_BUDGET_UNOBSERVED_SEC};
use tally_core::lease::{is_campaign_pool_name, CAMPAIGN_POOL_PREFIX};

const COMPLETION_TRAILER_PREFIXES: [&str; 2] = ["Tally-Task:", "Tally-Revision:"];
const APPROVED_GRAPH_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const MAX_APPROVED_GRAPH_SNAPSHOT_BYTES: u64 = 32 * 1024 * 1024;
const CAMPAIGN_STEERING_SCHEMA_VERSION: u32 = 1;
const CAMPAIGN_INBOX_SCHEMA_VERSION: u32 = 1;
const CAMPAIGN_STEERING_CURSOR_SCHEMA_VERSION: u32 = 1;
const CAMPAIGN_STEERING_EMBARGO_MILLISECONDS: i64 = 1_000;
const MAX_CAMPAIGN_STEERING_LOG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CAMPAIGN_STEERING_BODY_CHARS: usize = 64_000;
const MAX_CAMPAIGN_STEERING_PER_TARGET: usize = 1_000;
const ATTEMPT_RECEIPTS_FILE: &str = "attempt-receipts-v1.jsonl";
const MAX_ATTEMPT_RECEIPTS_LOG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DIAGNOSIS_CHARS: usize = 12_000;
const MAX_RETRY_CHARS: usize = 2_000;
/// The longest firing a gate-observation receipt may claim, in seconds.
///
/// A gate cannot have run longer than the campaign budget that supervised it,
/// and `DEFAULT_AGENT_RUNTIME_MAX_SEC` (four hours) is the largest such budget
/// this contract ships. The bound exists so one corrupt duration cannot derive
/// an unbounded budget for every later pass.
const MAX_GATE_OBSERVATION_SEC: u64 = DEFAULT_AGENT_RUNTIME_MAX_SEC;
const LOCAL_CAMPAIGN_ISSUE_NUMBER: u64 = 1;
const LOCAL_ALLOWED_ACTOR: &str = "local";
const RELEASE_PLAN_SCHEMA_VERSION: u32 = 2;
const RELEASE_RECORD_SCHEMA_VERSION: u32 = 1;
const RELEASE_ARTIFACTS_SCHEMA_VERSION: u32 = 1;
const RELEASE_PROBE_RECEIPT_SCHEMA_VERSION: u32 = 1;
const RELEASE_SUMMARY_SCHEMA_VERSION: u32 = 1;
const MAX_RELEASE_REGISTRATION_BYTES: u64 = 1024 * 1024;
const MAX_RELEASE_RECORD_BYTES: u64 = 1024 * 1024;
const MAX_RELEASE_PAYLOAD_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RELEASE_GIT_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_RELEASE_FORGE_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const RELEASE_RECORD_FILE: &str = "release-record-v1.json";
const RELEASE_NOTES_FILE: &str = "release-notes.md";
const RELEASE_ARTIFACTS_FILE: &str = "release-artifacts-v1.json";
const RELEASE_PROBE_RECEIPT_FILE: &str = "probe-receipt-v1.json";
const RELEASE_PROBE_PREFIX: &str = "tally-probe-";
const RELEASE_PROBE_TTL_DAYS: i64 = 7;
const COMPLETE_SUMMARY_MARKER_PREFIX: &str = "<!-- tally:campaign-complete:v1 source=";
#[cfg(test)]
const TEST_RELEASE_CRASH_CHILD_ENV: &str = "TALLY_TEST_RELEASE_CRASH_CHILD";
#[cfg(test)]
const TEST_RELEASE_CRASH_AFTER_ENV: &str = "TALLY_TEST_RELEASE_CRASH_AFTER";
#[cfg(test)]
const TEST_RELEASE_CRASH_STATE_ENV: &str = "TALLY_TEST_RELEASE_CRASH_STATE";
#[cfg(test)]
const TEST_RELEASE_CRASH_GH_ENV: &str = "TALLY_TEST_RELEASE_CRASH_GH";

#[derive(Debug, Clone)]
struct WorklistTask {
    id: String,
    kind: String,
    title: String,
    body: String,
    ownership_lint_inputs: Vec<OwnershipLintInput>,
    issue: Option<u64>,
    dependencies: Vec<String>,
    conflict_domains: Option<Vec<String>>,
    argv: Option<Vec<String>>,
    runtime_max_sec: Option<u64>,
}

#[derive(Debug, Clone)]
struct OwnershipLintInput {
    context: String,
    text: String,
}

#[derive(Debug, Clone)]
struct ValidatedWorklist {
    manifest: CampaignManifest,
    tasks: Vec<WorklistTask>,
}

#[derive(Debug, Clone)]
struct CommittedLocalWorklist {
    document: Value,
    source_path: String,
    source_sha256: String,
}

/// A gate exactly as a worklist may author it.
///
/// The only difference from the admitted `CampaignGate` is the one that matters
/// here: `runtimeMaxSec` is optional, and its absence is a statement rather than
/// an omission to be papered over. It says the gate's own receipts decide.
/// Nothing downstream ever sees this type; `resolve_worklist_gate_budgets`
/// turns it into an admitted gate carrying a resolved number.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum WorklistGate {
    #[serde(rename = "command", rename_all = "camelCase")]
    Command {
        id: String,
        preflight_argv: Vec<String>,
        argv: Vec<String>,
        #[serde(default)]
        runtime_max_sec: Option<u64>,
    },
    #[serde(rename = "forbidPaths", rename_all = "camelCase")]
    ForbidPaths {
        id: String,
        forbid_paths: Vec<String>,
        #[serde(default)]
        runtime_max_sec: Option<u64>,
    },
}

impl WorklistGate {
    fn id(&self) -> &str {
        match self {
            Self::Command { id, .. } | Self::ForbidPaths { id, .. } => id,
        }
    }

    const fn declared_runtime_max_sec(&self) -> Option<u64> {
        match self {
            Self::Command {
                runtime_max_sec, ..
            }
            | Self::ForbidPaths {
                runtime_max_sec, ..
            } => *runtime_max_sec,
        }
    }

    fn resolved(&self, runtime_max_sec: u64) -> CampaignGate {
        match self {
            Self::Command {
                id,
                preflight_argv,
                argv,
                ..
            } => CampaignGate::Command {
                id: id.clone(),
                preflight_argv: preflight_argv.clone(),
                argv: argv.clone(),
                runtime_max_sec,
            },
            Self::ForbidPaths {
                id, forbid_paths, ..
            } => CampaignGate::ForbidPaths {
                id: id.clone(),
                forbid_paths: forbid_paths.clone(),
                runtime_max_sec,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WorklistCampaignPolicy {
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_worklist_max_tasks")]
    max_tasks: usize,
    #[serde(default = "default_worklist_max_parallel")]
    max_parallel: usize,
    #[serde(default = "default_worklist_merge_method")]
    merge_method: String,
    #[serde(default = "default_worklist_driver_runtime_max_sec")]
    driver_runtime_max_sec: u64,
    #[serde(default)]
    runtime_max_sec: Option<u64>,
    #[serde(default = "default_worklist_agent")]
    agent: CampaignAgent,
    #[serde(default)]
    steward: Option<String>,
    #[serde(default)]
    steward_argv: Vec<String>,
    #[serde(default = "default_worklist_steward_runtime_max_sec")]
    steward_runtime_max_sec: u64,
    #[serde(default)]
    gates: Vec<WorklistGate>,
}

struct CheckpointBrief<'a> {
    campaign_name: &'a str,
    argv: &'a [String],
    runtime_max_sec: u64,
    dependencies: &'a [String],
}

#[derive(Debug, Clone)]
struct CampaignGraph {
    canonical: CanonicalCampaignGraphV1,
    ownership_preflight_warnings: Vec<String>,
    worklist_sha256: String,
    task_input_hashes: BTreeMap<String, String>,
    /// The writer's tuple for each task, computed once at admission.
    ///
    /// Carried rather than recomputed downstream for the same reason the gate
    /// budgets are: the identity the driver stamps into a merge trailer and
    /// the identity `release` recomputes from the same admitted graph must be
    /// the same bytes, and the only way to promise that is to compute them
    /// once, here.
    task_completion_revisions: BTreeMap<String, String>,
    /// How each gate's admitted budget was arrived at, in manifest order.
    ///
    /// Carried rather than recomputed so the receipt an operator reads and the
    /// number the graph admitted cannot disagree.
    gate_budgets: Vec<GateBudget>,
}

const fn default_worklist_max_tasks() -> usize {
    DEFAULT_MAX_TASKS
}

const fn default_worklist_max_parallel() -> usize {
    1
}

fn default_worklist_merge_method() -> String {
    "squash".to_owned()
}

const fn default_worklist_driver_runtime_max_sec() -> u64 {
    DEFAULT_DRIVER_RUNTIME_MAX_SEC
}

fn default_worklist_agent() -> CampaignAgent {
    CampaignAgent {
        adapter: "codex".to_owned(),
        argv: vec![BRIEF_SENTINEL.to_owned()],
        priority: DEFAULT_AGENT_PRIORITY.to_owned(),
        runtime_max_sec: Some(DEFAULT_AGENT_RUNTIME_MAX_SEC),
        // A worklist that names no policy gets none here either: the selected
        // adapter answers for its own launch vocabulary.
        approval_policy: None,
        sandbox_policy: None,
        diagnosis_sandbox_policy: None,
        model: None,
    }
}

const fn default_worklist_steward_runtime_max_sec() -> u64 {
    DEFAULT_STEWARD_RUNTIME_MAX_SEC
}

/// The six-field object an implementation brief has always consumed.
///
/// Keep this object closed and nested inside the append-only envelope. The
/// transport is local now; the worker-facing steering record is deliberately
/// byte-for-byte the prior `authorizedComments` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LocalSteeringCommentV1 {
    id: u64,
    url: String,
    author: String,
    body: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LocalSteeringRecordV1 {
    schema_version: u32,
    sequence: u64,
    registration_id: String,
    task_id: Option<String>,
    do_not_dispatch_before: String,
    comment: LocalSteeringCommentV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LocalSteeringCursorV1 {
    schema_version: u32,
    registration_id: String,
    high_water: u64,
    dispatched_at: String,
    observation: String,
}

/// The trusted source descriptor handed to the separately packaged driver.
/// Absolute paths make the same verb usable through SSH: the remote shell
/// writes coordinator-local state and the worker later reads that exact state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalSteeringSourceV1 {
    schema_version: u32,
    kind: &'static str,
    registration_id: String,
    local_actor: String,
    log_path: PathBuf,
    lock_path: PathBuf,
    prepared_cursor: u64,
}

#[derive(Debug, Clone)]
struct LocalSteeringSnapshot {
    steering: CampaignSteering,
    source: LocalSteeringSourceV1,
    do_not_dispatch_before: Option<DateTime<chrono::FixedOffset>>,
}

#[derive(Debug, Clone)]
struct LocalSteeringPaths {
    directory: PathBuf,
    log: PathBuf,
    lock: PathBuf,
    cursor: PathBuf,
}

struct LocalSteeringDispatch {
    lock: fs::File,
    cursor_path: PathBuf,
    directory: PathBuf,
    registration_id: String,
    snapshot: Box<LocalSteeringSnapshot>,
}

enum LocalSteeringDispatchState {
    Embargoed(DateTime<chrono::FixedOffset>),
    Ready(LocalSteeringDispatch),
}

/// The prior executable graph needed to interpret a later amendment.
///
/// This graph snapshot is generation-scoped beside authority, so
/// publishing arm N+1 cannot make an arm-N reader observe a graph that
/// disagrees with its authority digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ApprovedGraphSnapshotV1 {
    schema_version: u32,
    registration_id: String,
    arm_serial: u64,
    graph: CanonicalCampaignGraphV1,
}

#[derive(Debug, Clone, PartialEq)]
enum LocalAttemptReceiptV1 {
    Diagnosis {
        sequence: u64,
        task_id: String,
        attempt: u8,
        input_epoch: Option<String>,
        blocks_task: bool,
        /// The typed question itself, kept verbatim. The escalation folds
        /// ignore it; the inbox is the reader that hands it to a human.
        diagnosis: String,
        /// The prepared amendment the driver minted beside its diagnosis,
        /// when it had one. Carried as authored so the entry an operator
        /// approves is the diff the machine wrote, not a re-rendering.
        proposal: Option<Box<Value>>,
        written_at: Option<String>,
    },
    Retry {
        task_id: String,
        input_epoch: Option<String>,
    },
    WorkerOutcome(LocalWorkerOutcome),
    Escalation,
    Pardon {
        tasks: Option<BTreeSet<String>>,
    },
    /// One recorded firing of one gate id, in seconds.
    ///
    /// This is the evidence an absent `runtimeMaxSec` derives from. It is
    /// deliberately campaign-scoped and gate-scoped and carries no task: a gate
    /// costs what it costs regardless of which lane provoked it.
    GateObservation {
        gate_id: String,
        duration_sec: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalWorkerOutcome {
    sequence: u64,
    task_id: String,
    task_revision: String,
    task_uuid: String,
    input_epoch: Option<String>,
    outcome: WorkerOutcomePayload,
    written_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkerOutcomePayload {
    NeedsAuthority { paths: Vec<String> },
    Impossible { reason: String },
}

impl WorkerOutcomePayload {
    const fn class(&self) -> &'static str {
        match self {
            Self::NeedsAuthority { .. } => "needs-authority",
            Self::Impossible { .. } => "impossible",
        }
    }
}

#[derive(Debug, Clone)]
enum ReleaseAttemptReceipt {
    Diagnosis {
        task_id: String,
        attempt: u64,
        diagnosis: String,
        input_epoch: Option<String>,
    },
    Retry {
        task_id: String,
        attempt: u64,
        reason: String,
        input_epoch: Option<String>,
    },
    WorkerOutcome,
    Escalation,
    Pardon {
        sequence: u64,
        tasks: Option<BTreeSet<String>>,
    },
    /// Gate cost evidence. It is read at admission to derive gate budgets and
    /// projects nothing into a release: no task claims it.
    GateObservation,
}

/// One durable attestation that some actor acted on one task.
///
/// The stamp is the whole point: a receipt that cannot say who wrote it and
/// when witnesses nothing. Legacy schema-1 receipts carry no stamp, so they
/// contribute no witness rather than an anonymous one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseWitnessStamp {
    task_id: String,
    kind: String,
    actor: String,
    written_at: String,
}

#[derive(Debug)]
struct ReleaseAttemptLog {
    path: PathBuf,
    present: bool,
    bytes: Vec<u8>,
    records: Vec<ReleaseAttemptReceipt>,
    witnesses: Vec<ReleaseWitnessStamp>,
}

#[derive(Debug, Clone)]
struct ReleaseGitRef {
    object_id: String,
    object_type: String,
    tree_id: Option<String>,
    reference: String,
}

#[derive(Debug, Clone)]
struct ReleaseCommit {
    object_id: String,
    tree_id: String,
    parents: Vec<String>,
    committed_at: i64,
    message: String,
    task_values: Vec<String>,
    revision_values: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ReleaseCompletionOracle {
    Exact,
    Bridge,
}

impl ReleaseCompletionOracle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Bridge => "bridge",
        }
    }
}

#[derive(Debug, Clone)]
struct ReleaseMergedCommit {
    task_id: String,
    commit: ReleaseCommit,
    oracle: ReleaseCompletionOracle,
    bridge_ref: Option<String>,
}

#[derive(Debug, Clone)]
struct ReleaseCheckpoint {
    task_id: String,
    reference: String,
    revision: String,
    source_sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReleaseClosingSummaryV1 {
    schema_version: u32,
    kind: String,
    campaign: String,
    issue_number: String,
    outcome: String,
    body: String,
}

#[derive(Debug, Clone)]
struct ReleaseSummaryRef {
    reference: String,
    object_id: String,
    source_sha256: Option<String>,
    summary: ReleaseClosingSummaryV1,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleaseNote {
    task_id: String,
    commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_commit: Option<String>,
    header: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    breaking: bool,
    summary: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleaseGateProof {
    task_id: String,
    reference: String,
    revision: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleaseSummaryProof {
    reference: String,
    object_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignReleaseCompletionProof {
    task_id: String,
    commit: String,
    oracle: ReleaseCompletionOracle,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<String>,
}

/// One row of the coverage join release renders from the durable record: one
/// admitted task, the completion it claims, and the witnesses that carry it.
///
/// Every task the lapse fact names appears, witnessed or not. A coverage table
/// that shows only what is covered is the judgment this census exists to
/// delete, and it is the judgment a hand-authored table cannot help making.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleaseCoverageRow {
    task_id: String,
    kind: String,
    title: String,
    /// The completion identity claimed for this task: the writer's tuple for
    /// an implementation task, the proven source digest for a checkpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    claim: Option<String>,
    /// Where that claim is durable: a merge commit, or a checkpoint ref.
    #[serde(skip_serializing_if = "Option::is_none")]
    proof: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oracle: Option<ReleaseCompletionOracle>,
    /// Durable attestations naming this task, in the order they were written
    /// and across every epoch this identity served. A census counts the acts
    /// that happened, not only the ones the current epoch would re-do.
    witnesses: Vec<String>,
}

/// The lapse fact's own proof of the revision the campaign finished on.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleaseLapseProof {
    task_id: String,
    reference: String,
}

/// The coverage story, rendered from durable facts alone.
///
/// It exists only for a lapsed identity, because the lapse fact is what makes
/// the record complete: the facts remain for release whenever, by anyone, with
/// no live pass, no armed registration, and no operator retelling in between.
/// `intent` is the one thing here that is not derived — the operator's own
/// closing summary, carried verbatim when they wrote one.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleaseCoverage {
    source: &'static str,
    lapsed_at: String,
    arm_serial: u64,
    graph_digest: String,
    sha: String,
    proven_by: CampaignReleaseLapseProof,
    rows: Vec<CampaignReleaseCoverageRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    intent: Option<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleaseArtifact {
    kind: String,
    locator: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleasePlan {
    schema_version: u32,
    mode: &'static str,
    campaign: String,
    registration_id: String,
    repository: String,
    worklist: String,
    version: String,
    revision: String,
    integration_ref: String,
    closing_summary: CampaignReleaseSummaryProof,
    completion_proofs: Vec<CampaignReleaseCompletionProof>,
    /// Present exactly when this identity's lease has lapsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<CampaignReleaseCoverage>,
    release_notes: Vec<CampaignReleaseNote>,
    gate_proof: CampaignReleaseGateProof,
    artifacts: Vec<CampaignReleaseArtifact>,
    digest: CampaignDigest,
    campaign_summary: String,
}

/// The forge executable is invocation configuration, never ambient process
/// state. Keeping it in this small value makes every forge write take the
/// injected capability explicitly and leaves `gh` on PATH as the sole
/// production fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CampaignReleaseExecutionConfig {
    gh_program: PathBuf,
}

impl CampaignReleaseExecutionConfig {
    fn resolve(gh_program: Option<PathBuf>) -> Result<Self> {
        let gh_program = gh_program.unwrap_or_else(|| PathBuf::from("gh"));
        if gh_program.as_os_str().is_empty() {
            return Err(invalid("--gh-program must not be empty"));
        }
        Ok(Self { gh_program })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignReleaseStepsV1 {
    tag: bool,
    release_notes: bool,
    artifacts: bool,
}

/// Local authority for release execution. Public release text is deliberately
/// absent: whether a step ran is answered only by this record.
///
/// The plan document and the completion proofs live here too. `planSha256`
/// pins the bytes, but a digest is not evidence — a reader asking what this
/// release actually claimed, and through which oracle, would otherwise have to
/// re-render a plan against a tree that has since moved on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignReleaseRecordV1 {
    schema_version: u32,
    registration_id: String,
    campaign: String,
    repository: String,
    worklist: String,
    version: String,
    revision: String,
    plan_sha256: String,
    /// The rendered plan the digest above pins, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan: Option<Value>,
    #[serde(default)]
    completion_proofs: Vec<CampaignReleaseCompletionProof>,
    steps: CampaignReleaseStepsV1,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleaseExecutionReceipt {
    schema_version: u32,
    mode: &'static str,
    status: &'static str,
    repository: String,
    version: String,
    record: PathBuf,
    executed_steps: Vec<&'static str>,
    skipped_steps: Vec<&'static str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignReleaseProbeRepository {
    name_with_owner: String,
    created_at: DateTime<Utc>,
    is_fork: bool,
    is_private: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleaseProbeReceipt {
    schema_version: u32,
    mode: &'static str,
    status: &'static str,
    source_repository: String,
    probe_repository: String,
    version: String,
    started_at: String,
    completed_at: String,
    expired_repositories_deleted: usize,
    repository_created: bool,
    release_complete: bool,
    teardown_complete: bool,
    release_record: PathBuf,
    receipt: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignReleaseArtifactsV1<'a> {
    schema_version: u32,
    campaign: &'a str,
    repository: &'a str,
    version: &'a str,
    revision: &'a str,
    closing_summary: &'a CampaignReleaseSummaryProof,
    gate_proof: &'a CampaignReleaseGateProof,
    artifacts: &'a [CampaignReleaseArtifact],
}

struct CampaignReleasePayloads {
    notes: PathBuf,
    artifacts: PathBuf,
}

#[derive(Debug)]
enum CampaignPollAttempt {
    Dispatched,
    /// Nothing to do, and the sentence saying why when the lease knows one.
    Unchanged {
        detail: Option<String>,
    },
    /// Another pass holds this identity's lease.
    Deferred {
        detail: String,
    },
    /// The lease lapsed: this identity is finished on a published head.
    Complete(CampaignLapseV1),
}

pub(super) async fn run_campaign(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    command: CampaignCommand,
) -> Result<()> {
    match command {
        CampaignCommand::Scaffold(args) => run_campaign_scaffold(args),
        CampaignCommand::Arm(args) => {
            run_campaign_arm(socket, config_path, rpc_timeout, args).await
        }
        CampaignCommand::Steer(args) => run_campaign_steer(args),
        CampaignCommand::Inbox(args) => run_campaign_inbox(args),
        CampaignCommand::Release(args) => run_campaign_release(args),
        CampaignCommand::Poll(args) => {
            run_campaign_poll(socket, config_path, rpc_timeout, args).await
        }
        CampaignCommand::Status(args) => {
            run_campaign_status(socket, config_path, rpc_timeout, args).await
        }
        CampaignCommand::List(args) => run_campaign_list(args),
        CampaignCommand::Quiescent(args) => run_campaign_quiescent(args),
        CampaignCommand::Disarm(args) => run_campaign_disarm(args),
    }
}

fn run_campaign_release(args: CampaignReleaseArgs) -> Result<()> {
    let CampaignReleaseArgs {
        code_repository,
        worklist_pattern,
        plan: plan_only,
        probe,
        gh_program,
        state_dir,
    } = args;
    let (code_repository, worklist_pattern) =
        campaign_identity(&code_repository, &worklist_pattern)?;
    let state_dir = resolve_state_dir(state_dir)?;
    let plan = render_campaign_release_plan(&state_dir, &code_repository, &worklist_pattern)?;

    if plan_only {
        // The compact first line is deliberately stable for scripts. Human
        // text follows after a blank line, so plan mode serves both consumers
        // without growing a second rendering path.
        outln!("{}", serde_json::to_string(&plan)?);
        outln!();
        outln!("{}", render_campaign_release_human(&plan).trim_end());
    } else if probe {
        let config = CampaignReleaseExecutionConfig::resolve(gh_program)?;
        let registration =
            read_release_registration(&state_dir, &code_repository, &worklist_pattern)?;
        if registration.registration_id != plan.registration_id {
            bail!(
                "campaign registration changed while preparing the release probe; render the probe again"
            );
        }
        let receipt =
            execute_campaign_release_probe(&state_dir, &registration.checkout, &plan, &config)?;
        outln!("{}", serde_json::to_string(&receipt)?);
    } else {
        let config = CampaignReleaseExecutionConfig::resolve(gh_program)?;
        let receipt = execute_campaign_release(&state_dir, &plan, &config)?;
        outln!("{}", serde_json::to_string(&receipt)?);
    }
    Ok(())
}

fn execute_campaign_release(
    state_dir: &Path,
    plan: &CampaignReleasePlan,
    config: &CampaignReleaseExecutionConfig,
) -> Result<CampaignReleaseExecutionReceipt> {
    let directory = campaign_release_directory(state_dir, &plan.registration_id)?;
    execute_campaign_release_in_directory(&directory, plan, config)
}

fn execute_campaign_release_in_directory(
    directory: &Path,
    plan: &CampaignReleasePlan,
    config: &CampaignReleaseExecutionConfig,
) -> Result<CampaignReleaseExecutionReceipt> {
    fs::create_dir_all(directory).with_context(|| {
        format!(
            "cannot create campaign release directory {}",
            directory.display()
        )
    })?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "cannot secure campaign release directory {}",
            directory.display()
        )
    })?;
    let lock_path = directory.join("release.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)
        .with_context(|| format!("cannot open campaign release lock {}", lock_path.display()))?;
    let lock_metadata = lock.metadata()?;
    if !lock_metadata.is_file() || lock_metadata.nlink() != 1 {
        bail!(
            "campaign release lock {} is not a private regular file",
            lock_path.display()
        );
    }
    FileExt::lock_exclusive(&lock)
        .with_context(|| format!("cannot lock campaign release state {}", lock_path.display()))?;

    let execution = execute_campaign_release_locked(directory, plan, config);
    let unlock = FileExt::unlock(&lock).with_context(|| {
        format!(
            "cannot unlock campaign release state {}",
            lock_path.display()
        )
    });
    match (execution, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(receipt), Ok(())) => Ok(receipt),
    }
}

fn execute_campaign_release_locked(
    directory: &Path,
    plan: &CampaignReleasePlan,
    config: &CampaignReleaseExecutionConfig,
) -> Result<CampaignReleaseExecutionReceipt> {
    let record_path = directory.join(RELEASE_RECORD_FILE);
    let plan_sha256 = release_plan_sha256(plan)?;
    let mut record = match read_campaign_release_record(&record_path)? {
        Some(record) => {
            validate_campaign_release_record(&record, plan, &plan_sha256, &record_path)?;
            record
        }
        None => {
            let record = CampaignReleaseRecordV1 {
                schema_version: RELEASE_RECORD_SCHEMA_VERSION,
                registration_id: plan.registration_id.clone(),
                campaign: plan.campaign.clone(),
                repository: plan.repository.clone(),
                worklist: plan.worklist.clone(),
                version: plan.version.clone(),
                revision: plan.revision.clone(),
                plan_sha256,
                plan: Some(serde_json::to_value(plan)?),
                completion_proofs: plan.completion_proofs.clone(),
                steps: CampaignReleaseStepsV1::default(),
            };
            write_campaign_release_record(directory, &record_path, &record)?;
            record
        }
    };
    let payloads = materialize_campaign_release_payloads(directory, plan)?;
    let mut executed_steps = Vec::new();
    let mut skipped_steps = Vec::new();

    if record.steps.tag {
        skipped_steps.push("tag");
    } else {
        create_campaign_release_tag(config, plan)?;
        record.steps.tag = true;
        write_campaign_release_record(directory, &record_path, &record)?;
        #[cfg(test)]
        release_test_crash_after("tag");
        executed_steps.push("tag");
    }

    if record.steps.release_notes {
        skipped_steps.push("release-notes");
    } else {
        publish_campaign_release_notes(config, plan, &payloads.notes)?;
        record.steps.release_notes = true;
        write_campaign_release_record(directory, &record_path, &record)?;
        #[cfg(test)]
        release_test_crash_after("release-notes");
        executed_steps.push("release-notes");
    }

    if record.steps.artifacts {
        skipped_steps.push("artifacts");
    } else {
        attach_campaign_release_artifacts(config, plan, &payloads.artifacts)?;
        record.steps.artifacts = true;
        write_campaign_release_record(directory, &record_path, &record)?;
        #[cfg(test)]
        release_test_crash_after("artifacts");
        executed_steps.push("artifacts");
    }

    Ok(CampaignReleaseExecutionReceipt {
        schema_version: RELEASE_RECORD_SCHEMA_VERSION,
        mode: "execute",
        status: "complete",
        repository: plan.repository.clone(),
        version: plan.version.clone(),
        record: record_path,
        executed_steps,
        skipped_steps,
    })
}

#[cfg(test)]
fn release_test_crash_after(step: &str) {
    if std::env::var(TEST_RELEASE_CRASH_CHILD_ENV).as_deref() == Ok("1")
        && std::env::var(TEST_RELEASE_CRASH_AFTER_ENV).as_deref() == Ok(step)
    {
        // This runs only in the isolated child test below. `abort` models a
        // process disappearing without unwinding after the fsynced record is
        // published and before the next release step begins.
        std::process::abort();
    }
}

fn campaign_release_directory(state_dir: &Path, registration_id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(registration_id)
        .context("campaign release registration ID is not a UUID")?;
    Ok(state_dir.join("campaigns/releases").join(registration_id))
}

fn execute_campaign_release_probe(
    state_dir: &Path,
    checkout: &Path,
    plan: &CampaignReleasePlan,
    config: &CampaignReleaseExecutionConfig,
) -> Result<CampaignReleaseProbeReceipt> {
    let started_at = Utc::now();
    let probe_repository =
        campaign_release_probe_repository(&plan.repository, started_at, uuid::Uuid::now_v7())?;
    let (_, probe_name) =
        validate_campaign_release_probe_repository(&plan.repository, &probe_repository)?;
    let release_directory = campaign_release_directory(state_dir, &plan.registration_id)?;
    let probes_directory = release_directory.join("probes");
    fs::create_dir_all(&probes_directory).with_context(|| {
        format!(
            "cannot create campaign release probe directory {}",
            probes_directory.display()
        )
    })?;
    fs::set_permissions(&probes_directory, fs::Permissions::from_mode(0o700)).with_context(
        || {
            format!(
                "cannot secure campaign release probe directory {}",
                probes_directory.display()
            )
        },
    )?;
    let directory = probes_directory.join(probe_name);
    fs::create_dir(&directory).with_context(|| {
        format!(
            "cannot create unique campaign release probe directory {}",
            directory.display()
        )
    })?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "cannot secure campaign release probe directory {}",
            directory.display()
        )
    })?;

    let source = directory.join(".source");
    let receipt_path = directory.join(RELEASE_PROBE_RECEIPT_FILE);
    let release_record = directory.join(RELEASE_RECORD_FILE);
    let mut expired_repositories_deleted = 0;
    let mut repository_created = false;
    let mut release_complete = false;
    let mut teardown_complete = false;

    let lifecycle = (|| -> Result<()> {
        expired_repositories_deleted =
            sweep_expired_campaign_release_probes(config, &plan.repository, started_at)?;
        prepare_campaign_release_probe_source(checkout, &plan.revision, &source)?;
        create_campaign_release_probe_repository(
            config,
            &plan.repository,
            &probe_repository,
            &source,
        )?;
        repository_created = true;

        let mut probe_plan = plan.clone();
        probe_plan.repository = probe_repository.clone();
        execute_campaign_release_in_directory(&directory, &probe_plan, config)?;
        release_complete = true;
        Ok(())
    })();

    let source_cleanup = match fs::symlink_metadata(&source) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(&source).with_context(|| {
                format!(
                    "cannot remove local campaign release probe source {}",
                    source.display()
                )
            })
        }
        Ok(_) => Err(anyhow::anyhow!(
            "local campaign release probe source {} is not a real directory",
            source.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "cannot inspect local campaign release probe source {}",
                source.display()
            )
        }),
    };

    let teardown = if repository_created {
        delete_campaign_release_probe_repository(
            config,
            &plan.repository,
            &probe_repository,
            "tearing down the campaign release probe repository",
        )
        .map(|()| {
            teardown_complete = true;
        })
    } else {
        Ok(())
    };

    let mut failures = Vec::new();
    if let Err(error) = lifecycle {
        failures.push(compact_text(&format!("{error:#}")));
    }
    if let Err(error) = source_cleanup {
        failures.push(compact_text(&format!("{error:#}")));
    }
    if let Err(error) = teardown {
        failures.push(compact_text(&format!("{error:#}")));
    }
    let passed = failures.is_empty() && repository_created && release_complete && teardown_complete;
    let failure = (!failures.is_empty()).then(|| failures.join("; "));
    let receipt = CampaignReleaseProbeReceipt {
        schema_version: RELEASE_PROBE_RECEIPT_SCHEMA_VERSION,
        mode: "probe",
        status: if passed { "passed" } else { "failed" },
        source_repository: plan.repository.clone(),
        probe_repository: probe_repository.clone(),
        version: plan.version.clone(),
        started_at: started_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        completed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        expired_repositories_deleted,
        repository_created,
        release_complete,
        teardown_complete,
        release_record,
        receipt: receipt_path.clone(),
        failure,
    };
    write_campaign_release_probe_receipt(&directory, &receipt_path, &receipt)?;
    if !passed {
        bail!(
            "campaign release probe {} failed: {}; receipt written to {}",
            probe_repository,
            receipt.failure.as_deref().unwrap_or("incomplete lifecycle"),
            receipt_path.display()
        );
    }
    Ok(receipt)
}

fn campaign_release_probe_repository(
    source_repository: &str,
    now: DateTime<Utc>,
    nonce: uuid::Uuid,
) -> Result<String> {
    let (owner, _) = source_repository
        .split_once('/')
        .ok_or_else(|| invalid("campaign release source must use OWNER/REPO form"))?;
    let nonce = nonce.simple().to_string();
    let short = &nonce[nonce.len() - 8..];
    let repository = format!(
        "{owner}/{RELEASE_PROBE_PREFIX}{}-{short}",
        now.format("%Y%m%d")
    );
    validate_campaign_release_probe_repository(source_repository, &repository)?;
    Ok(repository)
}

fn validate_campaign_release_probe_repository<'a>(
    source_repository: &str,
    probe_repository: &'a str,
) -> Result<(&'a str, &'a str)> {
    let source_owner = source_repository
        .split_once('/')
        .map(|(owner, _)| owner)
        .ok_or_else(|| invalid("campaign release source must use OWNER/REPO form"))?;
    let (owner, name) = probe_repository
        .split_once('/')
        .ok_or_else(|| invalid("campaign release probe target must use OWNER/REPO form"))?;
    let Some(suffix) = name.strip_prefix(RELEASE_PROBE_PREFIX) else {
        return Err(invalid(format!(
            "campaign release probe target must use the {RELEASE_PROBE_PREFIX}<date>-<short> prefix"
        )));
    };
    let Some((date, short)) = suffix.split_once('-') else {
        return Err(invalid(
            "campaign release probe target must use tally-probe-<date>-<short> form",
        ));
    };
    let valid_date = date.len() == 8
        && date.bytes().all(|byte| byte.is_ascii_digit())
        && chrono::NaiveDate::parse_from_str(date, "%Y%m%d").is_ok();
    let valid_short = (6..=16).contains(&short.len())
        && short
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if !owner.eq_ignore_ascii_case(source_owner)
        || !valid_date
        || !valid_short
        || short.contains('-')
    {
        return Err(invalid(format!(
            "campaign release probe target must match {source_owner}/{RELEASE_PROBE_PREFIX}<date>-<short>"
        )));
    }
    parse_repository(probe_repository)?;
    Ok((owner, name))
}

fn sweep_expired_campaign_release_probes(
    config: &CampaignReleaseExecutionConfig,
    source_repository: &str,
    now: DateTime<Utc>,
) -> Result<usize> {
    let (owner, _) = source_repository
        .split_once('/')
        .ok_or_else(|| invalid("campaign release source must use OWNER/REPO form"))?;
    let output = run_release_gh_capture(
        config,
        &[
            "repo".into(),
            "list".into(),
            owner.into(),
            "--limit".into(),
            "1000".into(),
            "--json".into(),
            "nameWithOwner,createdAt,isFork,isPrivate".into(),
        ],
        "listing campaign release probe repositories for the TTL sweep",
    )?;
    let repositories: Vec<CampaignReleaseProbeRepository> =
        if output.iter().all(|byte| byte.is_ascii_whitespace()) {
            Vec::new()
        } else {
            serde_json::from_slice(&output)
                .context("campaign release probe repository listing is not valid JSON")?
        };
    let cutoff = now - TimeDelta::days(RELEASE_PROBE_TTL_DAYS);
    let mut deleted = 0;
    for repository in repositories {
        let listed_owner = repository
            .name_with_owner
            .split_once('/')
            .map(|(listed_owner, _)| listed_owner);
        if listed_owner.is_none_or(|listed_owner| !listed_owner.eq_ignore_ascii_case(owner))
            || !repository
                .name_with_owner
                .split_once('/')
                .is_some_and(|(_, name)| name.starts_with(RELEASE_PROBE_PREFIX))
            || repository.created_at >= cutoff
        {
            continue;
        }
        validate_campaign_release_probe_repository(source_repository, &repository.name_with_owner)?;
        if !repository.is_private || repository.is_fork {
            bail!(
                "refusing to expire campaign release probe {} because it is not a private non-fork repository",
                repository.name_with_owner
            );
        }
        delete_campaign_release_probe_repository(
            config,
            source_repository,
            &repository.name_with_owner,
            "deleting an expired campaign release probe repository",
        )?;
        deleted += 1;
    }
    Ok(deleted)
}

fn prepare_campaign_release_probe_source(
    checkout: &Path,
    revision: &str,
    source: &Path,
) -> Result<()> {
    run_release_probe_git(
        &[
            "init".into(),
            "--quiet".into(),
            "--initial-branch=main".into(),
            source.as_os_str().to_owned(),
        ],
        "initializing the local campaign release probe source",
    )?;
    run_release_probe_git(
        &[
            "-C".into(),
            source.as_os_str().to_owned(),
            "fetch".into(),
            "--quiet".into(),
            "--no-tags".into(),
            checkout.as_os_str().to_owned(),
            revision.into(),
        ],
        "copying the integrated revision into the campaign release probe source",
    )?;
    run_release_probe_git(
        &[
            "-C".into(),
            source.as_os_str().to_owned(),
            "reset".into(),
            "--quiet".into(),
            "--hard".into(),
            "FETCH_HEAD".into(),
        ],
        "checking out the integrated revision for the campaign release probe",
    )
}

fn run_release_probe_git(arguments: &[OsString], context: &str) -> Result<()> {
    let output = ProcessCommand::new("git")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("cannot execute git while preparing the campaign release probe")?;
    if output.status.success() {
        return Ok(());
    }
    let detail = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    bail!(
        "{context} exited {}: {}",
        output.status,
        compact_text(&String::from_utf8_lossy(detail))
    )
}

fn create_campaign_release_probe_repository(
    config: &CampaignReleaseExecutionConfig,
    source_repository: &str,
    probe_repository: &str,
    source: &Path,
) -> Result<()> {
    validate_campaign_release_probe_repository(source_repository, probe_repository)?;
    run_release_gh(
        config,
        &[
            "repo".into(),
            "create".into(),
            probe_repository.into(),
            "--private".into(),
            "--source".into(),
            source.as_os_str().to_owned(),
            "--remote".into(),
            "origin".into(),
            "--push".into(),
        ],
        "creating the private campaign release probe repository",
    )
}

fn delete_campaign_release_probe_repository(
    config: &CampaignReleaseExecutionConfig,
    source_repository: &str,
    probe_repository: &str,
    context: &str,
) -> Result<()> {
    validate_campaign_release_probe_repository(source_repository, probe_repository)?;
    run_release_gh(
        config,
        &[
            "repo".into(),
            "delete".into(),
            probe_repository.into(),
            "--yes".into(),
        ],
        context,
    )
}

fn write_campaign_release_probe_receipt(
    directory: &Path,
    path: &Path,
    receipt: &CampaignReleaseProbeReceipt,
) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(receipt)?;
    bytes.push(b'\n');
    write_campaign_release_file(directory, path, &bytes, MAX_RELEASE_RECORD_BYTES).with_context(
        || {
            format!(
                "cannot write campaign release probe receipt {}",
                path.display()
            )
        },
    )
}

fn release_plan_sha256(plan: &CampaignReleasePlan) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(plan)?)
    ))
}

fn read_campaign_release_record(path: &Path) -> Result<Option<CampaignReleaseRecordV1>> {
    let mut file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot open campaign release record {}", path.display()))
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > MAX_RELEASE_RECORD_BYTES {
        bail!(
            "campaign release record {} is not a bounded private regular file",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let record = serde_json::from_slice(&bytes)
        .with_context(|| format!("campaign release record {} is invalid", path.display()))?;
    Ok(Some(record))
}

fn validate_campaign_release_record(
    record: &CampaignReleaseRecordV1,
    plan: &CampaignReleasePlan,
    plan_sha256: &str,
    path: &Path,
) -> Result<()> {
    let identity_matches = record.schema_version == RELEASE_RECORD_SCHEMA_VERSION
        && record.registration_id == plan.registration_id
        && record.campaign == plan.campaign
        && record.repository == plan.repository
        && record.worklist == plan.worklist
        && record.version == plan.version
        && record.revision == plan.revision
        && record.plan_sha256 == plan_sha256;
    let monotonic = !record.steps.release_notes || record.steps.tag;
    let monotonic = monotonic && (!record.steps.artifacts || record.steps.release_notes);
    if !identity_matches || !monotonic {
        bail!(
            "campaign release record {} disagrees with the current release plan or step order",
            path.display()
        );
    }
    Ok(())
}

fn write_campaign_release_record(
    directory: &Path,
    path: &Path,
    record: &CampaignReleaseRecordV1,
) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(record)?;
    bytes.push(b'\n');
    write_campaign_release_file(directory, path, &bytes, MAX_RELEASE_RECORD_BYTES)
}

fn write_campaign_release_file(
    directory: &Path,
    path: &Path,
    bytes: &[u8],
    maximum: u64,
) -> Result<()> {
    if u64::try_from(bytes.len())? > maximum {
        bail!("campaign release payload {} is too large", path.display());
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() || metadata.nlink() != 1 => {
            bail!(
                "campaign release payload {} is not a private regular file",
                path.display()
            )
        }
        Ok(_) => {
            let existing = fs::read(path).with_context(|| {
                format!("cannot read campaign release payload {}", path.display())
            })?;
            if existing == bytes {
                return Ok(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("cannot inspect campaign release payload {}", path.display())
            })
        }
    }

    let temporary = directory.join(format!(".release.{}.tmp", uuid::Uuid::now_v7()));
    let write = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .with_context(|| {
                format!(
                    "cannot create campaign release payload {}",
                    temporary.display()
                )
            })?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path).with_context(|| {
            format!("cannot publish campaign release payload {}", path.display())
        })?;
        fs::File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if write.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write
}

fn materialize_campaign_release_payloads(
    directory: &Path,
    plan: &CampaignReleasePlan,
) -> Result<CampaignReleasePayloads> {
    let notes = directory.join(RELEASE_NOTES_FILE);
    let artifacts = directory.join(RELEASE_ARTIFACTS_FILE);
    write_campaign_release_file(
        directory,
        &notes,
        render_campaign_release_notes(plan).as_bytes(),
        MAX_RELEASE_PAYLOAD_BYTES,
    )?;
    let manifest = CampaignReleaseArtifactsV1 {
        schema_version: RELEASE_ARTIFACTS_SCHEMA_VERSION,
        campaign: &plan.campaign,
        repository: &plan.repository,
        version: &plan.version,
        revision: &plan.revision,
        closing_summary: &plan.closing_summary,
        gate_proof: &plan.gate_proof,
        artifacts: &plan.artifacts,
    };
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    manifest_bytes.push(b'\n');
    write_campaign_release_file(
        directory,
        &artifacts,
        &manifest_bytes,
        MAX_RELEASE_PAYLOAD_BYTES,
    )?;
    Ok(CampaignReleasePayloads { notes, artifacts })
}

fn run_release_gh(
    config: &CampaignReleaseExecutionConfig,
    arguments: &[OsString],
    context: &str,
) -> Result<()> {
    run_release_gh_capture(config, arguments, context).map(|_| ())
}

fn run_release_gh_capture(
    config: &CampaignReleaseExecutionConfig,
    arguments: &[OsString],
    context: &str,
) -> Result<Vec<u8>> {
    let output = ProcessCommand::new(&config.gh_program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "cannot execute forge program {}",
                config.gh_program.display()
            )
        })?;
    if output.stdout.len() > MAX_RELEASE_FORGE_OUTPUT_BYTES
        || output.stderr.len() > MAX_RELEASE_FORGE_OUTPUT_BYTES
    {
        bail!(
            "{context} through {} produced more than {} bytes on one output stream",
            config.gh_program.display(),
            MAX_RELEASE_FORGE_OUTPUT_BYTES
        );
    }
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    bail!(
        "{context} through {} exited {}: {}",
        config.gh_program.display(),
        output.status,
        if detail.is_empty() {
            "no output"
        } else {
            &detail
        }
    )
}

fn create_campaign_release_tag(
    config: &CampaignReleaseExecutionConfig,
    plan: &CampaignReleasePlan,
) -> Result<()> {
    run_release_gh(
        config,
        &[
            "api".into(),
            "--method".into(),
            "POST".into(),
            format!("repos/{}/git/refs", plan.repository).into(),
            "--raw-field".into(),
            format!("ref=refs/tags/{}", plan.version).into(),
            "--raw-field".into(),
            format!("sha={}", plan.revision).into(),
        ],
        "creating the release tag",
    )
}

fn publish_campaign_release_notes(
    config: &CampaignReleaseExecutionConfig,
    plan: &CampaignReleasePlan,
    notes: &Path,
) -> Result<()> {
    run_release_gh(
        config,
        &[
            "release".into(),
            "create".into(),
            plan.version.clone().into(),
            "--repo".into(),
            plan.repository.clone().into(),
            "--verify-tag".into(),
            "--title".into(),
            format!("{} {}", plan.campaign, plan.version).into(),
            "--notes-file".into(),
            notes.as_os_str().to_owned(),
        ],
        "publishing the release notes",
    )
}

fn attach_campaign_release_artifacts(
    config: &CampaignReleaseExecutionConfig,
    plan: &CampaignReleasePlan,
    artifacts: &Path,
) -> Result<()> {
    run_release_gh(
        config,
        &[
            "release".into(),
            "upload".into(),
            plan.version.clone().into(),
            artifacts.as_os_str().to_owned(),
            "--repo".into(),
            plan.repository.clone().into(),
            "--clobber".into(),
        ],
        "attaching the release artifacts",
    )
}

fn render_campaign_release_plan(
    state_dir: &Path,
    code_repository: &str,
    worklist_pattern: &str,
) -> Result<CampaignReleasePlan> {
    let registration = read_release_registration(state_dir, code_repository, worklist_pattern)?;
    let graph = read_approved_graph_snapshot(state_dir, &registration)?.ok_or_else(|| {
        invalid(format!(
            "campaign {code_repository}/{worklist_pattern} has no approved graph snapshot; re-arm it before rendering a release"
        ))
    })?;
    validate_release_graph(&registration, &graph)?;

    let campaign = graph.manifest.name.as_str();
    let integration_branch =
        stable_publish_branch(campaign, &registration.registration_id, "integration", None);
    let integration_ref = format!("refs/heads/{integration_branch}");
    let state_prefix = campaign_state_ref_prefix(campaign, LOCAL_CAMPAIGN_ISSUE_NUMBER);
    let refs = release_local_refs(
        &registration.checkout,
        &["refs/heads/tally/".to_owned(), format!("{state_prefix}/")],
    )?;
    let integration = release_required_ref(&refs, &integration_ref, "commit")?;
    let history = release_integration_history(&registration.checkout, &integration_ref)?;
    let revisions = graph_completion_revisions(&graph)?;
    let merged_commits = release_merged_commits(&graph, &revisions, &history, &refs)?;
    let source_revision = merged_commits
        .first()
        .and_then(|merged| merged.commit.parents.first())
        .cloned()
        .unwrap_or_else(|| integration.object_id.clone());

    let all_checkpoints =
        release_checkpoint_refs(&graph, &refs, &state_prefix, &integration.object_id)?;
    let gate_checkpoint =
        release_gate_checkpoint(&graph, &all_checkpoints, &integration.object_id)?;
    let checkpoints =
        release_current_checkpoints(&graph, &all_checkpoints, &gate_checkpoint.source_sha256)?;

    let summaries = release_summary_refs(&registration.checkout, &refs, &state_prefix, campaign)?;
    let closing_summary = release_closing_summary(
        &summaries,
        &state_prefix,
        &gate_checkpoint.source_sha256,
        &graph.executable_digest,
    )?;
    let attempt_log = read_release_attempt_log(state_dir, campaign, LOCAL_CAMPAIGN_ISSUE_NUMBER)?;

    let task_ids = graph
        .manifest
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<BTreeSet<_>>();
    let steering = read_existing_local_steering(state_dir, &registration)?;
    let task_input_hashes = canonical_task_input_hashes(&graph)?;
    let input_epochs = current_task_input_epochs(&task_input_hashes, &steering)?;
    let (diagnoses, retries, mut warnings) =
        release_attempt_facts(&attempt_log.records, &task_ids, &input_epochs);
    warnings.extend(release_bridge_warnings(&merged_commits));
    let merged = merged_commits
        .iter()
        .map(|merged| MergedFact {
            task_id: merged.task_id.clone(),
            pull_request: format!(
                "local://{code_repository}/{}",
                merged
                    .bridge_ref
                    .as_deref()
                    .and_then(|reference| reference.strip_prefix("refs/heads/"))
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        stable_publish_branch(
                            campaign,
                            &registration.registration_id,
                            &merged.task_id,
                            revisions.get(&merged.task_id).map(String::as_str),
                        )
                    })
            ),
            merge_commit: merged.commit.object_id.clone(),
        })
        .collect::<Vec<_>>();
    let checkpoint_facts = checkpoints
        .iter()
        .map(|checkpoint| CheckpointFact {
            task_id: checkpoint.task_id.clone(),
            revision: checkpoint.revision.clone(),
        })
        .collect::<Vec<_>>();
    let reconciliation = CampaignReconciliation {
        campaign: campaign.to_owned(),
        repository: code_repository.to_owned(),
        source: CampaignSource {
            path: Some(worklist_pattern.to_owned()),
            sha256: gate_checkpoint.source_sha256.clone(),
            revision: source_revision,
            repository: None,
            extra: serde_json::Map::new(),
        },
        base_revision: integration.object_id.clone(),
        tasks: graph
            .manifest
            .tasks
            .iter()
            .zip(&graph.tasks)
            .map(|(reference, content)| ReconciledTask {
                id: reference.id.clone(),
                title: content.title.clone(),
            })
            .collect(),
        merged,
        checkpoints: checkpoint_facts,
        remaining: Vec::new(),
        diagnoses,
        retries,
        deferrals: Vec::<DeferralFact>::new(),
        blocked: Vec::<BlockedFact>::new(),
        warnings,
    };
    let digest = campaign_digest(&reconciliation, "complete");
    if digest.merged.len() + digest.checkpoints.len() != digest.task_count {
        bail!(
            "completed campaign {campaign:?} has only {} durable merge/checkpoint fact(s) for {} task(s)",
            digest.merged.len() + digest.checkpoints.len(),
            digest.task_count
        );
    }
    let campaign_summary = render_campaign_summary(&digest);

    let completion_proofs = release_completion_proofs(&merged_commits);
    // Release reads the identity's own durable lease, never a registration
    // that happens to still exist: a lapsed campaign is finished whether or
    // not anything is armed, and the coverage below is rendered from that
    // fact and the facts it points at.
    let coverage = CampaignLeaseStore::new(state_dir, code_repository, worklist_pattern)
        .read()?
        .and_then(|lease| lease.lapse)
        .map(|lapse| {
            release_coverage(
                &lapse,
                &graph,
                &merged_commits,
                &checkpoints,
                &attempt_log.witnesses,
                Some(closing_summary.summary.body.as_str())
                    .filter(|intent| !intent.trim().is_empty()),
            )
        });
    let release_notes = release_notes(&registration, &graph, &refs, &revisions, &merged_commits)?;
    let artifacts = release_artifacts(
        integration,
        &release_notes,
        &refs,
        &checkpoints,
        closing_summary,
        &summaries,
        &attempt_log,
    );
    let committed_at = history
        .iter()
        .find(|commit| commit.object_id == gate_checkpoint.revision)
        .map(|commit| commit.committed_at)
        .map(Ok)
        .unwrap_or_else(|| {
            release_commit_timestamp(&registration.checkout, &gate_checkpoint.revision)
        })?;
    let version = release_version(committed_at, &gate_checkpoint.revision)?;

    Ok(CampaignReleasePlan {
        schema_version: RELEASE_PLAN_SCHEMA_VERSION,
        mode: "plan",
        campaign: campaign.to_owned(),
        registration_id: registration.registration_id.clone(),
        repository: code_repository.to_owned(),
        worklist: worklist_pattern.to_owned(),
        version,
        revision: integration.object_id.clone(),
        integration_ref,
        closing_summary: CampaignReleaseSummaryProof {
            reference: closing_summary.reference.clone(),
            object_id: closing_summary.object_id.clone(),
        },
        completion_proofs,
        coverage,
        release_notes,
        gate_proof: CampaignReleaseGateProof {
            task_id: gate_checkpoint.task_id,
            reference: gate_checkpoint.reference,
            revision: gate_checkpoint.revision,
        },
        artifacts,
        digest,
        campaign_summary,
    })
}

fn read_release_registration(
    state_dir: &Path,
    code_repository: &str,
    worklist_pattern: &str,
) -> Result<CampaignRegistration> {
    let path = release_registration_path(state_dir, code_repository, worklist_pattern);
    let metadata = fs::symlink_metadata(&path).with_context(|| {
        format!(
            "cannot read campaign registration {}; arm the campaign before rendering its release",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || metadata.len() > MAX_RELEASE_REGISTRATION_BYTES
    {
        bail!(
            "campaign registration {} is not a bounded private regular file",
            path.display()
        );
    }
    let bytes = fs::read(&path)
        .with_context(|| format!("cannot read campaign registration {}", path.display()))?;
    let authority: CampaignRegistrationV4 = serde_json::from_slice(&bytes)
        .with_context(|| format!("campaign registration {} is invalid", path.display()))?;
    if authority.schema_version != REGISTRY_SCHEMA_VERSION
        || authority.code_repository != code_repository
        || authority.worklist_pattern != worklist_pattern
        || authority.arm_serial == 0
        || !authority.checkout.is_absolute()
        || !is_sha256_identity(&authority.approved_graph_digest)
        || uuid::Uuid::parse_str(&authority.registration_id).is_err()
    {
        bail!(
            "campaign registration {} is not valid schema-{} authority for {code_repository}/{worklist_pattern}",
            path.display(),
            REGISTRY_SCHEMA_VERSION
        );
    }
    Ok(CampaignRegistration::new(authority, None))
}

fn release_registration_path(
    state_dir: &Path,
    code_repository: &str,
    worklist_pattern: &str,
) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(code_repository.as_bytes());
    hasher.update([0]);
    hasher.update(worklist_pattern.as_bytes());
    state_dir
        .join("campaigns/armed")
        .join(format!("{:x}.json", hasher.finalize()))
}

fn validate_release_graph(
    registration: &CampaignRegistration,
    graph: &CanonicalCampaignGraphV1,
) -> Result<()> {
    let repository = &graph.manifest.repository;
    if graph.executable_digest != registration.approved_graph_digest
        || repository.checkout != registration.checkout
        || repository.base_branch != registration.base_branch
        || repository.remote != registration.remote
        || repository.forge != "local"
        || !safe_component(&graph.manifest.name)
    {
        bail!(
            "campaign approved graph disagrees with registration {} arm {}",
            registration.registration_id,
            registration.arm_serial
        );
    }
    Ok(())
}

fn is_sha256_identity(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn is_git_object_id(value: &str) -> bool {
    (40..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Run one of the deliberately tiny local Git read primitives used by plan
/// mode. Keeping the allowlist here makes an accidental fetch, push, ls-remote,
/// or forge helper structurally unavailable to the renderer.
fn release_git_read(checkout: &Path, arguments: &[String], context: &str) -> Result<Vec<u8>> {
    if !matches!(
        arguments.first().map(String::as_str),
        Some("for-each-ref" | "log" | "cat-file" | "show")
    ) {
        bail!("internal release renderer attempted a non-read-only Git operation");
    }
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .output()
        .with_context(|| format!("cannot run local Git while {context}"))?;
    if !output.status.success() {
        bail!(
            "local Git failed while {context}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if output.stdout.len() > MAX_RELEASE_GIT_OUTPUT_BYTES {
        bail!("local Git output exceeded 8 MiB while {context}");
    }
    Ok(output.stdout)
}

fn release_campaign_generation_ref_prefix(campaign: &str) -> String {
    let sentinel = stable_publish_branch(campaign, "generation", "task", None);
    let prefix = sentinel
        .strip_suffix("generation/task")
        .expect("the stable publish branch preserves the fixed sentinel suffix");
    format!("refs/heads/{prefix}")
}

fn release_local_refs(checkout: &Path, prefixes: &[String]) -> Result<Vec<ReleaseGitRef>> {
    let mut arguments = vec![
        "for-each-ref".to_owned(),
        "--format=%(objectname)%09%(objecttype)%09%(tree)%09%(refname)".to_owned(),
    ];
    arguments.extend(prefixes.iter().cloned());
    let stdout = release_git_read(checkout, &arguments, "listing local campaign refs")?;
    let stdout = String::from_utf8(stdout).context("local campaign refs were not UTF-8")?;
    let mut refs = Vec::new();
    for line in stdout.lines().filter(|line| !line.is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4
            || !is_git_object_id(fields[0])
            || !matches!(fields[1], "blob" | "commit")
            || !fields[3].starts_with("refs/")
        {
            bail!("local campaign ref listing returned a malformed row");
        }
        let tree_id = match fields[1] {
            "commit" if is_git_object_id(fields[2]) => Some(fields[2].to_owned()),
            "blob" if fields[2].is_empty() => None,
            _ => bail!("local campaign ref listing returned a malformed row"),
        };
        refs.push(ReleaseGitRef {
            object_id: fields[0].to_owned(),
            object_type: fields[1].to_owned(),
            tree_id,
            reference: fields[3].to_owned(),
        });
    }
    refs.sort_by(|left, right| left.reference.cmp(&right.reference));
    Ok(refs)
}

fn release_required_ref<'a>(
    refs: &'a [ReleaseGitRef],
    reference: &str,
    object_type: &str,
) -> Result<&'a ReleaseGitRef> {
    let matches = refs
        .iter()
        .filter(|candidate| candidate.reference == reference)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [candidate] if candidate.object_type == object_type => Ok(candidate),
        [candidate] => bail!(
            "campaign ref {reference:?} must point directly to a {object_type}, not a {}",
            candidate.object_type
        ),
        [] => bail!(
            "completed campaign is missing local ref {reference:?}; restore its durable refs before rendering a release"
        ),
        _ => bail!("local campaign ref {reference:?} appeared more than once"),
    }
}

fn release_integration_history(checkout: &Path, reference: &str) -> Result<Vec<ReleaseCommit>> {
    let task_key = TALLY_TASK_PREFIX.trim_end_matches(':');
    let revision_key = TALLY_REVISION_PREFIX.trim_end_matches(':');
    let format = format!(
        "%H%x00%T%x00%P%x00%ct%x00%B%x00%(trailers:key={task_key},valueonly,unfold=true,separator=%x1f)%x00%(trailers:key={revision_key},valueonly,unfold=true,separator=%x1f)"
    );
    let arguments = vec![
        "log".to_owned(),
        "--first-parent".to_owned(),
        "-z".to_owned(),
        format!("--format={format}"),
        reference.to_owned(),
    ];
    let stdout = release_git_read(checkout, &arguments, "reading local integration trailers")?;
    let stdout = String::from_utf8(stdout).context("local integration history was not UTF-8")?;
    let mut fields = stdout.split('\0').collect::<Vec<_>>();
    if fields.last() == Some(&"") {
        fields.pop();
    }
    if fields.len() % 7 != 0 {
        bail!("local integration trailer listing returned malformed output");
    }
    fields
        .chunks_exact(7)
        .map(|fields| {
            let object_id = fields[0];
            let tree_id = fields[1];
            let parents = fields[2]
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if !is_git_object_id(object_id)
                || !is_git_object_id(tree_id)
                || parents.iter().any(|parent| !is_git_object_id(parent))
            {
                bail!("local integration history returned a malformed commit");
            }
            let committed_at = fields[3]
                .parse::<i64>()
                .context("local integration history returned a malformed timestamp")?;
            Ok(ReleaseCommit {
                object_id: object_id.to_owned(),
                tree_id: tree_id.to_owned(),
                parents,
                committed_at,
                message: fields[4].to_owned(),
                task_values: split_release_trailers(fields[5]),
                revision_values: split_release_trailers(fields[6]),
            })
        })
        .collect()
}

fn split_release_trailers(value: &str) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        value.split('\u{1f}').map(str::to_owned).collect()
    }
}

fn graph_completion_revisions(
    graph: &CanonicalCampaignGraphV1,
) -> Result<BTreeMap<String, String>> {
    if graph.manifest.tasks.len() != graph.tasks.len() {
        bail!("campaign approved graph has mismatched task references and content");
    }
    graph
        .manifest
        .tasks
        .iter()
        .zip(&graph.tasks)
        .map(|(reference, content)| {
            Ok((
                reference.id.clone(),
                task_completion_revision(&graph.manifest, reference, content)?,
            ))
        })
        .collect()
}

fn release_merged_commits(
    graph: &CanonicalCampaignGraphV1,
    revisions: &BTreeMap<String, String>,
    history: &[ReleaseCommit],
    refs: &[ReleaseGitRef],
) -> Result<Vec<ReleaseMergedCommit>> {
    let implementation_ids = graph
        .manifest
        .tasks
        .iter()
        .filter(|task| task.kind == "implementation")
        .map(|task| task.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut claims = BTreeMap::<(String, String), Vec<&ReleaseCommit>>::new();
    for commit in history {
        if let ([task_id], [revision]) = (
            commit.task_values.as_slice(),
            commit.revision_values.as_slice(),
        ) {
            if safe_task_id(task_id)
                && is_sha256_identity(revision)
                && has_canonical_completion_trailer_pair(&commit.message, task_id, revision)
            {
                claims
                    .entry((task_id.clone(), revision.clone()))
                    .or_default()
                    .push(commit);
            }
        }
    }
    let mut selected = BTreeMap::<String, ReleaseMergedCommit>::new();
    for task_id in &implementation_ids {
        let revision = revisions
            .get(*task_id)
            .expect("every graph task has a computed revision");
        match claims.get(&(String::from(*task_id), revision.clone())) {
            Some(matches) if matches.len() == 1 => {
                selected.insert(
                    String::from(*task_id),
                    ReleaseMergedCommit {
                        task_id: String::from(*task_id),
                        commit: matches[0].clone(),
                        oracle: ReleaseCompletionOracle::Exact,
                        bridge_ref: None,
                    },
                );
            }
            Some(_) => bail!(
                "multiple local integration commits claim campaign task {task_id:?} revision {revision}"
            ),
            None => {
                let mut bridge_candidates = BTreeMap::<String, (&ReleaseCommit, String)>::new();
                for ((claim_task_id, claim_revision), commits) in &claims {
                    if claim_task_id != *task_id {
                        continue;
                    }
                    for commit in commits {
                        if let Some(reference) = release_completion_bridge_ref(
                            &graph.manifest.name,
                            task_id,
                            claim_revision,
                            commit,
                            refs,
                        ) {
                            bridge_candidates
                                .entry(commit.object_id.clone())
                                .or_insert((*commit, reference.reference.clone()));
                        }
                    }
                }
                match bridge_candidates.len() {
                    0 => bail!(
                        "completed campaign is missing the {TALLY_TASK_PREFIX} {task_id} / {TALLY_REVISION_PREFIX} {revision} trailer proof"
                    ),
                    1 => {
                        let (commit, reference) = bridge_candidates
                            .into_values()
                            .next()
                            .expect("one bridge candidate was counted");
                        selected.insert(
                            String::from(*task_id),
                            ReleaseMergedCommit {
                                task_id: String::from(*task_id),
                                commit: commit.clone(),
                                oracle: ReleaseCompletionOracle::Bridge,
                                bridge_ref: Some(reference),
                            },
                        );
                    }
                    count => bail!(
                        "multiple local integration commits ({count}) carry completion-ref bridge proofs for campaign task {task_id:?}"
                    ),
                }
            }
        }
    }

    let mut merged = Vec::new();
    for commit in history.iter().rev() {
        if let Some(proof) = selected
            .values()
            .find(|proof| proof.commit.object_id == commit.object_id)
        {
            merged.push(proof.clone());
        }
    }
    if merged.len() != implementation_ids.len() {
        bail!(
            "completed campaign trailer oracle found {} of {} implementation task(s)",
            merged.len(),
            implementation_ids.len()
        );
    }
    Ok(merged)
}

fn release_completion_bridge_ref<'a>(
    campaign: &str,
    task_id: &str,
    revision: &str,
    commit: &ReleaseCommit,
    refs: &'a [ReleaseGitRef],
) -> Option<&'a ReleaseGitRef> {
    let digest = revision.strip_prefix("sha256:")?;
    let revision_prefix = digest.get(..16)?;
    let expected_leaf = format!("{task_id}-{revision_prefix}");
    let generation_prefix = release_campaign_generation_ref_prefix(campaign);
    refs.iter().find(|reference| {
        reference.object_type == "commit"
            && reference.tree_id.as_deref() == Some(commit.tree_id.as_str())
            && reference
                .reference
                .strip_prefix(&generation_prefix)
                .and_then(|suffix| suffix.split_once('/'))
                .is_some_and(|(generation, leaf)| {
                    safe_component(generation) && leaf == expected_leaf
                })
    })
}

fn release_completion_proofs(
    merged_commits: &[ReleaseMergedCommit],
) -> Vec<CampaignReleaseCompletionProof> {
    merged_commits
        .iter()
        .map(|merged| CampaignReleaseCompletionProof {
            task_id: merged.task_id.clone(),
            commit: merged.commit.object_id.clone(),
            oracle: merged.oracle,
            reference: merged.bridge_ref.clone(),
        })
        .collect()
}

/// Name the legacy bridge out loud whenever it answers.
///
/// The bridge exists for one case: a task whose durable trailer carries an
/// identity this graph no longer computes. That is a legacy proof, and a
/// release that quietly accepts one reads exactly like a release that proved
/// its completions exactly. So it says which task, which ref, and that the
/// exact writer-tuple oracle found nothing.
fn release_bridge_warnings(merged_commits: &[ReleaseMergedCommit]) -> Vec<String> {
    merged_commits
        .iter()
        .filter(|merged| merged.oracle == ReleaseCompletionOracle::Bridge)
        .map(|merged| {
            let reference = merged.bridge_ref.as_deref().unwrap_or("an unnamed task ref");
            format!(
                "campaign task {:?} was proven by the legacy completion bridge through {reference}; the exact writer-tuple oracle found no matching {TALLY_REVISION_PREFIX} trailer",
                merged.task_id
            )
        })
        .collect()
}

/// Render the claim/task/witness join from the durable record alone.
///
/// The lapse fact is the spine: it names every task the finished graph
/// carried, so the census is the campaign's own list rather than the list of
/// tasks that happened to leave a proof behind. Everything joined onto it is
/// equally durable — merge commits on the integration line, checkpoint refs,
/// and the stamped attempt receipts that say who acted and when.
fn release_coverage(
    lapse: &CampaignLapseV1,
    graph: &CanonicalCampaignGraphV1,
    merged_commits: &[ReleaseMergedCommit],
    checkpoints: &[ReleaseCheckpoint],
    witnesses: &[ReleaseWitnessStamp],
    intent: Option<&str>,
) -> CampaignReleaseCoverage {
    let admitted = graph
        .manifest
        .tasks
        .iter()
        .zip(&graph.tasks)
        .map(|(reference, content)| (reference.id.as_str(), (reference, content)))
        .collect::<BTreeMap<_, _>>();
    let mut rows = Vec::new();
    for task_id in &lapse.tasks {
        let admitted_task = admitted.get(task_id.as_str());
        let merged = merged_commits
            .iter()
            .find(|merged| &merged.task_id == task_id);
        let checkpoint = checkpoints
            .iter()
            .find(|checkpoint| &checkpoint.task_id == task_id);
        let (claim, proof, oracle) = match (merged, checkpoint) {
            (Some(merged), _) => (
                merged.commit.revision_values.first().cloned(),
                Some(merged.commit.object_id.clone()),
                Some(merged.oracle),
            ),
            (None, Some(checkpoint)) => (
                Some(checkpoint.source_sha256.clone()),
                Some(checkpoint.reference.clone()),
                None,
            ),
            (None, None) => (None, None, None),
        };
        let mut task_witnesses = witnesses
            .iter()
            .filter(|witness| &witness.task_id == task_id)
            .map(|witness| {
                format!(
                    "{} receipt written by {} at {}",
                    witness.kind, witness.actor, witness.written_at
                )
            })
            .collect::<Vec<_>>();
        if &lapse.proven_by.task_id == task_id {
            task_witnesses.push(format!(
                "gate proof {} on {}",
                lapse.proven_by.reference, lapse.sha
            ));
        }
        rows.push(CampaignReleaseCoverageRow {
            task_id: task_id.clone(),
            kind: admitted_task
                .map(|(reference, _)| reference.kind.clone())
                .unwrap_or_else(|| "unknown".to_owned()),
            title: admitted_task
                .map(|(_, content)| content.title.clone())
                .unwrap_or_default(),
            claim,
            proof,
            oracle,
            witnesses: task_witnesses,
        });
    }

    let mut warnings = Vec::new();
    if lapse.graph_digest != graph.executable_digest {
        warnings.push(format!(
            "the durable lapse fact finished graph {} while this release renders graph {}",
            lapse.graph_digest, graph.executable_digest
        ));
    }
    let lapsed_ids = lapse
        .tasks
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for task_id in admitted.keys().filter(|id| !lapsed_ids.contains(*id)) {
        warnings.push(format!(
            "admitted task {task_id:?} is absent from the durable lapse fact"
        ));
    }
    for task_id in lapsed_ids.iter().filter(|id| !admitted.contains_key(*id)) {
        warnings.push(format!(
            "the durable lapse fact names task {task_id:?}, which the admitted graph does not carry"
        ));
    }

    CampaignReleaseCoverage {
        source: "durable-facts",
        lapsed_at: lapse.lapsed_at.clone(),
        arm_serial: lapse.arm_serial,
        graph_digest: lapse.graph_digest.clone(),
        sha: lapse.sha.clone(),
        proven_by: CampaignReleaseLapseProof {
            task_id: lapse.proven_by.task_id.clone(),
            reference: lapse.proven_by.reference.clone(),
        },
        rows,
        intent: intent.map(str::to_owned),
        warnings,
    }
}

fn release_checkpoint_refs(
    graph: &CanonicalCampaignGraphV1,
    refs: &[ReleaseGitRef],
    state_prefix: &str,
    integration_tip: &str,
) -> Result<Vec<ReleaseCheckpoint>> {
    let checkpoint_ids = graph
        .manifest
        .tasks
        .iter()
        .filter(|task| task.kind == "checkpoint")
        .map(|task| task.id.as_str())
        .collect::<BTreeSet<_>>();
    let prefix = format!("{state_prefix}/checkpoint/");
    let mut checkpoints = Vec::new();
    for reference in refs
        .iter()
        .filter(|reference| reference.reference.starts_with(&prefix))
    {
        let suffix = reference
            .reference
            .strip_prefix(&prefix)
            .expect("checkpoint ref was filtered by prefix");
        let Some((identity, named_revision)) = suffix.split_once('/') else {
            continue;
        };
        if named_revision.contains('/') || !is_git_object_id(named_revision) {
            continue;
        }
        let Some((task_id, source_digest)) = identity.rsplit_once('-') else {
            continue;
        };
        if !checkpoint_ids.contains(task_id)
            || source_digest.len() != 64
            || !source_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            continue;
        }
        if reference.object_type != "commit" || reference.object_id != named_revision {
            bail!(
                "checkpoint ref {:?} must point directly to its named commit",
                reference.reference
            );
        }
        checkpoints.push(ReleaseCheckpoint {
            task_id: task_id.to_owned(),
            reference: reference.reference.clone(),
            revision: reference.object_id.clone(),
            source_sha256: format!("sha256:{source_digest}"),
        });
    }
    if checkpoints.is_empty() {
        bail!(
            "completed campaign has no local checkpoint refs below {prefix}; restore the gate proof before rendering a release"
        );
    }
    if !checkpoints
        .iter()
        .any(|checkpoint| checkpoint.revision == integration_tip)
    {
        bail!(
            "completed campaign has no checkpoint ref for integration tip {integration_tip}; restore the gate proof before rendering a release"
        );
    }
    checkpoints.sort_by(|left, right| left.reference.cmp(&right.reference));
    Ok(checkpoints)
}

fn release_gate_checkpoint(
    graph: &CanonicalCampaignGraphV1,
    checkpoints: &[ReleaseCheckpoint],
    integration_tip: &str,
) -> Result<ReleaseCheckpoint> {
    let implementation_ids = graph
        .manifest
        .tasks
        .iter()
        .filter(|task| task.kind == "implementation")
        .map(|task| task.id.as_str())
        .collect::<BTreeSet<_>>();
    let dependencies = graph
        .manifest
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task.dependencies.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let gate_ids = graph
        .manifest
        .tasks
        .iter()
        .filter(|task| task.kind == "checkpoint")
        .filter_map(|task| {
            let mut closure = BTreeSet::new();
            let mut stack = vec![task.id.as_str()];
            while let Some(task_id) = stack.pop() {
                if !closure.insert(task_id) {
                    continue;
                }
                if let Some(items) = dependencies.get(task_id) {
                    stack.extend(items.iter().map(String::as_str));
                }
            }
            implementation_ids
                .is_subset(&closure)
                .then_some(task.id.as_str())
        })
        .collect::<BTreeSet<_>>();
    if gate_ids.is_empty() {
        bail!("completed campaign has no checkpoint covering every implementation task");
    }
    let candidates = checkpoints
        .iter()
        .filter(|checkpoint| {
            checkpoint.revision == integration_tip && gate_ids.contains(checkpoint.task_id.as_str())
        })
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [checkpoint] => Ok((*checkpoint).clone()),
        [] => bail!(
            "completed campaign has no gate-proof checkpoint ref at integration tip {integration_tip}; restore the gate proof before rendering a release"
        ),
        _ => bail!(
            "completed campaign has multiple gate-proof checkpoint refs at integration tip {integration_tip}"
        ),
    }
}

fn release_current_checkpoints(
    graph: &CanonicalCampaignGraphV1,
    checkpoints: &[ReleaseCheckpoint],
    source_sha256: &str,
) -> Result<Vec<ReleaseCheckpoint>> {
    graph
        .manifest
        .tasks
        .iter()
        .filter(|task| task.kind == "checkpoint")
        .map(|task| {
            let matches = checkpoints
                .iter()
                .filter(|checkpoint| {
                    checkpoint.task_id == task.id && checkpoint.source_sha256 == source_sha256
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [checkpoint] => Ok((*checkpoint).clone()),
                [] => bail!(
                    "completed campaign is missing checkpoint ref for task {:?} and source {}; restore its durable checkpoint ref before rendering a release",
                    task.id,
                    source_sha256
                ),
                _ => bail!(
                    "completed campaign has multiple checkpoint refs for task {:?} and source {}",
                    task.id,
                    source_sha256
                ),
            }
        })
        .collect()
}

fn release_summary_refs(
    checkout: &Path,
    refs: &[ReleaseGitRef],
    state_prefix: &str,
    campaign: &str,
) -> Result<Vec<ReleaseSummaryRef>> {
    refs.iter()
        .filter(|reference| is_release_summary_ref(&reference.reference, state_prefix))
        .map(|reference| {
            if reference.object_type != "blob" {
                bail!(
                    "campaign summary ref {:?} must point directly to a blob",
                    reference.reference
                );
            }
            let bytes = release_git_read(
                checkout,
                &[
                    "cat-file".to_owned(),
                    "blob".to_owned(),
                    reference.object_id.clone(),
                ],
                "reading a local campaign summary",
            )?;
            let summary: ReleaseClosingSummaryV1 =
                serde_json::from_slice(&bytes).with_context(|| {
                    format!(
                        "campaign summary ref {:?} does not contain valid closing-summary JSON",
                        reference.reference
                    )
                })?;
            if summary.schema_version != RELEASE_SUMMARY_SCHEMA_VERSION
                || summary.kind != "closing-summary"
                || summary.campaign != campaign
                || summary.issue_number != LOCAL_CAMPAIGN_ISSUE_NUMBER.to_string()
                || !matches!(summary.outcome.as_str(), "complete" | "quiescent")
                || summary.body.chars().count() > 60_000
            {
                bail!(
                    "campaign summary ref {:?} has invalid identity or shape",
                    reference.reference
                );
            }
            let source_sha256 = complete_summary_source(&summary.body)?;
            Ok(ReleaseSummaryRef {
                reference: reference.reference.clone(),
                object_id: reference.object_id.clone(),
                source_sha256,
                summary,
            })
        })
        .collect()
}

fn is_release_summary_ref(reference: &str, state_prefix: &str) -> bool {
    let Some(suffix) = reference
        .strip_prefix(state_prefix)
        .and_then(|suffix| suffix.strip_prefix('/'))
    else {
        return false;
    };
    if matches!(suffix, "summary/complete" | "summary/quiescent")
        || suffix
            .strip_prefix("summary/archive/")
            .is_some_and(|tag| !tag.is_empty())
    {
        return true;
    }
    let Some((digest, summary)) = suffix.split_once('/') else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && matches!(summary, "summary/complete" | "summary/quiescent")
}

fn complete_summary_source(body: &str) -> Result<Option<String>> {
    let Some(first_line) = body.lines().next() else {
        return Ok(None);
    };
    let Some(value) = first_line
        .strip_prefix(COMPLETE_SUMMARY_MARKER_PREFIX)
        .and_then(|value| value.strip_suffix(" -->"))
    else {
        return Ok(None);
    };
    if !is_sha256_identity(value) {
        bail!("campaign complete summary carries a malformed source identity");
    }
    Ok(Some(value.to_owned()))
}

fn release_closing_summary<'a>(
    summaries: &'a [ReleaseSummaryRef],
    state_prefix: &str,
    source_sha256: &str,
    admitted_graph_digest: &str,
) -> Result<&'a ReleaseSummaryRef> {
    let matches = summaries
        .iter()
        .filter(|summary| {
            summary.summary.outcome == "complete"
                && summary.source_sha256.as_deref() == Some(source_sha256)
        })
        .collect::<Vec<_>>();
    let mut current = vec![
        stage_scoped_summary_ref(state_prefix, admitted_graph_digest, "complete")?,
        stage_scoped_summary_ref(state_prefix, source_sha256, "complete")?,
        format!("{state_prefix}/summary/complete"),
    ];
    current.dedup();
    for reference in current {
        if let Some(summary) = matches
            .iter()
            .find(|summary| summary.reference == reference)
        {
            return Ok(*summary);
        }
    }
    match matches.as_slice() {
        [] => bail!(
            "completed campaign has no local complete-summary ref for source {source_sha256}; restore its durable summary before rendering a release"
        ),
        [summary] => Ok(*summary),
        _ => bail!(
            "completed campaign has multiple historical complete summaries for source {source_sha256}"
        ),
    }
}

fn read_release_attempt_log(
    state_dir: &Path,
    campaign: &str,
    issue_number: u64,
) -> Result<ReleaseAttemptLog> {
    let path = local_attempt_receipts_path(state_dir, campaign)?;
    let mut file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReleaseAttemptLog {
                path,
                present: false,
                bytes: Vec::new(),
                records: Vec::new(),
                witnesses: Vec::new(),
            })
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot open attempt-receipts log {}", path.display()))
        }
    };
    FileExt::lock_shared(&file)
        .with_context(|| format!("cannot lock attempt-receipts log {}", path.display()))?;
    let read = (|| {
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() > MAX_ATTEMPT_RECEIPTS_LOG_BYTES
        {
            bail!(
                "attempt-receipts log is not a bounded private regular file: {}",
                path.display()
            );
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            bail!(
                "attempt-receipts log {} has a truncated final record; repair the durable log before rendering a release",
                path.display()
            );
        }
        let mut records = Vec::new();
        let mut witnesses = Vec::new();
        let complete = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        for (index, line) in complete
            .split(|byte| *byte == b'\n')
            .filter(|line| !complete.is_empty() || !line.is_empty())
            .enumerate()
        {
            if line.is_empty() {
                bail!(
                    "attempt-receipts log {} contains a blank record at line {}",
                    path.display(),
                    index + 1
                );
            }
            let sequence = u64::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(1))
                .ok_or_else(|| invalid("attempt-receipts sequence is exhausted"))?;
            let value: Value = serde_json::from_slice(line).with_context(|| {
                format!(
                    "attempt receipt {sequence} in {} is invalid JSON",
                    path.display()
                )
            })?;
            let validated = validate_local_attempt_receipt(
                value.clone(),
                &path,
                sequence,
                campaign,
                issue_number,
            )?;
            let object = value
                .as_object()
                .expect("validated attempt receipt is an object");
            let record = match validated {
                LocalAttemptReceiptV1::Diagnosis {
                    task_id,
                    attempt,
                    input_epoch,
                    ..
                } => ReleaseAttemptReceipt::Diagnosis {
                    task_id,
                    attempt: u64::from(attempt),
                    diagnosis: object["diagnosis"]
                        .as_str()
                        .expect("validated diagnosis is text")
                        .to_owned(),
                    input_epoch,
                },
                LocalAttemptReceiptV1::Retry {
                    task_id,
                    input_epoch,
                } => ReleaseAttemptReceipt::Retry {
                    task_id,
                    attempt: object["attempt"]
                        .as_u64()
                        .expect("validated retry attempt is an integer"),
                    reason: object["reason"]
                        .as_str()
                        .expect("validated retry reason is text")
                        .to_owned(),
                    input_epoch,
                },
                LocalAttemptReceiptV1::WorkerOutcome(_) => ReleaseAttemptReceipt::WorkerOutcome,
                LocalAttemptReceiptV1::Escalation => ReleaseAttemptReceipt::Escalation,
                LocalAttemptReceiptV1::GateObservation { .. } => {
                    ReleaseAttemptReceipt::GateObservation
                }
                LocalAttemptReceiptV1::Pardon { tasks } => {
                    ReleaseAttemptReceipt::Pardon { sequence, tasks }
                }
            };
            if let (Some(task_id), Some(actor), Some(written_at)) = (
                object.get("taskId").and_then(Value::as_str),
                object.get("actor").and_then(Value::as_str),
                object.get("writtenAt").and_then(Value::as_str),
            ) {
                witnesses.push(ReleaseWitnessStamp {
                    task_id: task_id.to_owned(),
                    kind: object["kind"]
                        .as_str()
                        .expect("validated receipt kind is text")
                        .to_owned(),
                    actor: actor.to_owned(),
                    written_at: written_at.to_owned(),
                });
            }
            records.push(record);
        }
        Ok((bytes, records, witnesses))
    })();
    let unlock = FileExt::unlock(&file)
        .with_context(|| format!("cannot unlock attempt-receipts log {}", path.display()));
    let (bytes, records, witnesses) = match (read, unlock) {
        (Err(error), _) => return Err(error),
        (Ok(_), Err(error)) => return Err(error),
        (Ok(value), Ok(())) => value,
    };
    Ok(ReleaseAttemptLog {
        path,
        present: true,
        bytes,
        records,
        witnesses,
    })
}

fn release_attempt_facts(
    records: &[ReleaseAttemptReceipt],
    task_ids: &BTreeSet<String>,
    current_epochs: &BTreeMap<String, String>,
) -> (Vec<DiagnosisFact>, Vec<RetryFact>, Vec<String>) {
    let mut diagnoses = Vec::<DiagnosisFact>::new();
    let mut retries = Vec::<RetryFact>::new();
    let mut warnings = Vec::new();
    for record in records {
        match record {
            ReleaseAttemptReceipt::Diagnosis {
                task_id,
                attempt,
                diagnosis,
                input_epoch,
            } if task_ids.contains(task_id)
                && receipt_epoch_is_current(task_id, input_epoch.as_deref(), current_epochs) =>
            {
                diagnoses.push(DiagnosisFact {
                    task_id: task_id.clone(),
                    attempt: *attempt,
                    diagnosis: diagnosis.clone(),
                })
            }
            ReleaseAttemptReceipt::Retry {
                task_id,
                attempt,
                reason,
                input_epoch,
            } if task_ids.contains(task_id)
                && receipt_epoch_is_current(task_id, input_epoch.as_deref(), current_epochs) =>
            {
                retries.push(RetryFact {
                    task_id: task_id.clone(),
                    attempt: *attempt,
                    reason: reason.clone(),
                })
            }
            ReleaseAttemptReceipt::Pardon { sequence, tasks } => {
                let before = diagnoses.len() + retries.len();
                match tasks {
                    None => {
                        diagnoses.clear();
                        retries.clear();
                    }
                    Some(scope) => {
                        diagnoses.retain(|fact| !scope.contains(&fact.task_id));
                        retries.retain(|fact| !scope.contains(&fact.task_id));
                    }
                }
                let pardoned = before - diagnoses.len() - retries.len();
                if pardoned > 0 {
                    warnings.push(format!(
                        "campaign pardon at attempt receipt {sequence} removed {pardoned} earlier machine receipt(s) from this release projection"
                    ));
                }
            }
            ReleaseAttemptReceipt::Diagnosis { .. }
            | ReleaseAttemptReceipt::Retry { .. }
            | ReleaseAttemptReceipt::WorkerOutcome
            | ReleaseAttemptReceipt::Escalation
            | ReleaseAttemptReceipt::GateObservation => {}
        }
    }
    (diagnoses, retries, warnings)
}

fn release_notes(
    registration: &CampaignRegistration,
    graph: &CanonicalCampaignGraphV1,
    refs: &[ReleaseGitRef],
    revisions: &BTreeMap<String, String>,
    merged_commits: &[ReleaseMergedCommit],
) -> Result<Vec<CampaignReleaseNote>> {
    let scopes = graph
        .manifest
        .tasks
        .iter()
        .filter_map(|task| task.conflict_domains.as_ref())
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
    let titles = graph
        .manifest
        .tasks
        .iter()
        .zip(&graph.tasks)
        .map(|(reference, content)| (reference.id.as_str(), content.title.as_str()))
        .collect::<BTreeMap<_, _>>();
    merged_commits
        .iter()
        .map(|merged| {
            let task_id = &merged.task_id;
            let commit = &merged.commit;
            let reference = merged.bridge_ref.clone().unwrap_or_else(|| {
                let branch = stable_publish_branch(
                    &graph.manifest.name,
                    &registration.registration_id,
                    task_id,
                    revisions.get(task_id).map(String::as_str),
                );
                format!("refs/heads/{branch}")
            });
            let source = refs
                .iter()
                .find(|candidate| candidate.reference == reference);
            let (source_ref, source_commit, message) = match source {
                Some(source) if source.object_type == "commit" => (
                    Some(source.reference.clone()),
                    Some(source.object_id.clone()),
                    release_commit_message(&registration.checkout, &source.object_id)?,
                ),
                Some(source) => bail!(
                    "campaign task ref {:?} must point directly to a commit, not a {}",
                    source.reference,
                    source.object_type
                ),
                None => (None, None, commit.message.clone()),
            };
            let header = commit_header(&message).to_owned();
            let fallback = titles.get(task_id.as_str()).copied().unwrap_or(task_id);
            let (kind, scope, breaking, summary) =
                validated_release_header(&message, &scopes, fallback);
            Ok(CampaignReleaseNote {
                task_id: task_id.clone(),
                commit: commit.object_id.clone(),
                source_ref,
                source_commit,
                header,
                kind,
                scope,
                breaking,
                summary,
            })
        })
        .collect()
}

fn release_commit_message(checkout: &Path, object_id: &str) -> Result<String> {
    let output = release_git_read(
        checkout,
        &[
            "show".to_owned(),
            "--no-patch".to_owned(),
            "--format=%B".to_owned(),
            object_id.to_owned(),
        ],
        "reading a merged task commit message",
    )?;
    String::from_utf8(output).context("merged task commit message was not UTF-8")
}

fn validated_release_header(
    message: &str,
    scopes: &BTreeSet<String>,
    fallback: &str,
) -> (String, Option<String>, bool, String) {
    let validation = validate_commit_message(message, scopes);
    if !validation.is_valid() {
        return ("other".to_owned(), None, false, fallback.to_owned());
    }
    let Some(header) = validation.header else {
        return ("other".to_owned(), None, false, fallback.to_owned());
    };
    (header.kind, header.scope, header.breaking, header.subject)
}

fn release_artifacts(
    integration: &ReleaseGitRef,
    release_notes: &[CampaignReleaseNote],
    refs: &[ReleaseGitRef],
    checkpoints: &[ReleaseCheckpoint],
    closing_summary: &ReleaseSummaryRef,
    summaries: &[ReleaseSummaryRef],
    attempt_log: &ReleaseAttemptLog,
) -> Vec<CampaignReleaseArtifact> {
    let mut artifacts = vec![CampaignReleaseArtifact {
        kind: "integration".to_owned(),
        locator: integration.reference.clone(),
        object_id: Some(integration.object_id.clone()),
        sha256: None,
        bytes: None,
    }];
    for note in release_notes {
        let (Some(source_ref), Some(source_commit)) =
            (note.source_ref.as_deref(), note.source_commit.as_deref())
        else {
            continue;
        };
        if refs.iter().any(|reference| {
            reference.reference == source_ref && reference.object_id == source_commit
        }) {
            artifacts.push(CampaignReleaseArtifact {
                kind: "task-commit".to_owned(),
                locator: source_ref.to_owned(),
                object_id: Some(source_commit.to_owned()),
                sha256: None,
                bytes: None,
            });
        }
    }
    artifacts.extend(
        checkpoints
            .iter()
            .map(|checkpoint| CampaignReleaseArtifact {
                kind: "checkpoint".to_owned(),
                locator: checkpoint.reference.clone(),
                object_id: Some(checkpoint.revision.clone()),
                sha256: None,
                bytes: None,
            }),
    );
    artifacts.push(CampaignReleaseArtifact {
        kind: "closing-summary".to_owned(),
        locator: closing_summary.reference.clone(),
        object_id: Some(closing_summary.object_id.clone()),
        sha256: None,
        bytes: None,
    });
    artifacts.extend(
        summaries
            .iter()
            .filter(|summary| summary.reference != closing_summary.reference)
            .map(|summary| CampaignReleaseArtifact {
                kind: "archived-summary".to_owned(),
                locator: summary.reference.clone(),
                object_id: Some(summary.object_id.clone()),
                sha256: None,
                bytes: None,
            }),
    );
    if attempt_log.present {
        artifacts.push(CampaignReleaseArtifact {
            kind: "attempt-receipts".to_owned(),
            locator: attempt_log.path.display().to_string(),
            object_id: None,
            sha256: Some(format!("sha256:{:x}", Sha256::digest(&attempt_log.bytes))),
            bytes: u64::try_from(attempt_log.bytes.len()).ok(),
        });
    }
    artifacts
}

fn release_commit_timestamp(checkout: &Path, object_id: &str) -> Result<i64> {
    let output = release_git_read(
        checkout,
        &[
            "show".to_owned(),
            "--no-patch".to_owned(),
            "--format=%ct".to_owned(),
            object_id.to_owned(),
        ],
        "reading the gate-proof timestamp",
    )?;
    String::from_utf8(output)
        .context("gate-proof timestamp was not UTF-8")?
        .trim()
        .parse::<i64>()
        .context("gate-proof timestamp was malformed")
}

fn release_version(committed_at: i64, revision: &str) -> Result<String> {
    let timestamp = DateTime::<Utc>::from_timestamp(committed_at, 0)
        .ok_or_else(|| invalid("gate-proof commit timestamp is outside the supported range"))?;
    let short_revision = revision.chars().take(7).collect::<String>();
    if short_revision.len() != 7 {
        return Err(invalid(
            "gate-proof revision is too short for a release version",
        ));
    }
    Ok(format!(
        "0.0.0+{}.{}",
        timestamp.format("%Y%m%d%H%M%S"),
        short_revision
    ))
}

fn render_campaign_release_human(plan: &CampaignReleasePlan) -> String {
    let mut lines = vec![
        format!("Release plan  {}  {}", plan.repository, plan.version),
        format!("Campaign      {} ({})", plan.campaign, plan.registration_id),
        format!("Revision      {}", plan.revision),
        format!(
            "Gate proof    {}  {}",
            plan.gate_proof.task_id, plan.gate_proof.reference
        ),
        String::new(),
        "Completion proofs".to_owned(),
    ];
    if plan.completion_proofs.is_empty() {
        lines.push("- No implementation tasks.".to_owned());
    } else {
        lines.extend(plan.completion_proofs.iter().map(|proof| {
            let reference = proof
                .reference
                .as_deref()
                .map(|reference| format!(" via {reference}"))
                .unwrap_or_default();
            format!(
                "- {}: {} [{}]{}",
                proof.task_id,
                proof.oracle.as_str(),
                &proof.commit[..7],
                reference
            )
        }));
    }
    if let Some(coverage) = &plan.coverage {
        lines.extend([
            String::new(),
            format!(
                "Coverage from durable facts  lapsed {}  arm {}",
                coverage.lapsed_at, coverage.arm_serial
            ),
        ]);
        lines.extend(coverage.rows.iter().map(|row| {
            let claim = row.claim.as_deref().unwrap_or("no durable claim");
            let witnesses = if row.witnesses.is_empty() {
                "unwitnessed".to_owned()
            } else {
                row.witnesses.join("; ")
            };
            format!("- {} [{}]: {claim} — {witnesses}", row.task_id, row.kind)
        }));
        lines.extend(
            coverage
                .warnings
                .iter()
                .map(|warning| format!("! {warning}")),
        );
    }
    lines.extend([String::new(), "Release notes".to_owned()]);
    if plan.release_notes.is_empty() {
        lines.push("- No implementation commits.".to_owned());
    } else {
        lines.extend(
            plan.release_notes
                .iter()
                .map(|note| format!("- {} [{}]", note.header, &note.commit[..7])),
        );
    }
    lines.extend([String::new(), "Artifacts".to_owned()]);
    lines.extend(
        plan.artifacts
            .iter()
            .map(|artifact| format!("- {}: {}", artifact.kind, artifact.locator)),
    );
    lines.extend([
        String::new(),
        "Campaign receipt".to_owned(),
        String::new(),
        plan.campaign_summary.trim_end().to_owned(),
    ]);
    let mut rendered = lines.join("\n");
    rendered.push('\n');
    rendered
}

fn render_campaign_release_notes(plan: &CampaignReleasePlan) -> String {
    let mut lines = vec![
        format!("# {}", plan.version),
        String::new(),
        "## Changes".to_owned(),
        String::new(),
    ];
    if plan.release_notes.is_empty() {
        lines.push("- No implementation changes.".to_owned());
    } else {
        lines.extend(
            plan.release_notes
                .iter()
                .map(|note| format!("- {}", note.header)),
        );
    }
    if let Some(coverage) = &plan.coverage {
        lines.extend([String::new(), "## Coverage".to_owned(), String::new()]);
        lines.extend(coverage.rows.iter().map(|row| {
            format!(
                "- `{}` ({}): {} — {}",
                row.task_id,
                row.kind,
                row.claim.as_deref().unwrap_or("no durable claim"),
                if row.witnesses.is_empty() {
                    "unwitnessed".to_owned()
                } else {
                    row.witnesses.join("; ")
                }
            )
        }));
    }
    lines.extend([
        String::new(),
        "## Verification".to_owned(),
        String::new(),
        format!("- Revision: `{}`", plan.revision),
        format!(
            "- Gate: `{}` at `{}`",
            plan.gate_proof.task_id, plan.gate_proof.revision
        ),
        String::new(),
        "## Artifacts".to_owned(),
        String::new(),
    ]);
    lines.extend(
        plan.artifacts
            .iter()
            .map(|artifact| format!("- {}: `{}`", artifact.kind, artifact.locator)),
    );
    let mut rendered = lines.join("\n");
    rendered.push('\n');
    rendered
}

fn parse_repository(value: &str) -> Result<String> {
    let parts = value.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || !safe_repo_part(parts[0]) || !safe_repo_part(parts[1]) {
        return Err(invalid("--repo must use safe OWNER/REPO form"));
    }
    Ok(value.to_owned())
}

fn parse_worklist_pattern(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 1_024
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\0')
        || value
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(invalid(
            "campaign worklist must be a relative pattern without empty, '.' or '..' components",
        ));
    }
    Ok(value.to_owned())
}

fn campaign_identity(code_repository: &str, worklist_pattern: &str) -> Result<(String, String)> {
    Ok((
        parse_repository(code_repository)?,
        parse_worklist_pattern(worklist_pattern)?,
    ))
}

fn campaign_issue_url(code_repository: &str, worklist_pattern: &str) -> String {
    format!("local://{code_repository}/{worklist_pattern}")
}

fn safe_repo_part(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value != "."
        && value != ".."
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn safe_task_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn safe_github_login(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn local_actor() -> String {
    // SAFETY: `geteuid` has no preconditions and does not mutate process state.
    format!("uid:{}", unsafe { libc::geteuid() })
}

fn normalize_allowed_actors(values: &[String], authenticated: &str) -> Result<Vec<String>> {
    let mut actors = if values.is_empty() {
        BTreeSet::from([authenticated.to_owned()])
    } else {
        values
            .iter()
            .map(|value| {
                if !safe_github_login(value) {
                    return Err(invalid(format!(
                        "campaign --allow-actor value {value:?} is not a valid GitHub login"
                    )));
                }
                Ok(value.to_ascii_lowercase())
            })
            .collect::<Result<BTreeSet<_>>>()?
    };
    actors.insert(authenticated.to_owned());
    Ok(actors.into_iter().collect())
}

fn require_local_actor(registration: &CampaignRegistration) -> Result<()> {
    let actor = local_actor();
    if actor != registration.local_actor {
        bail!(
            "armed campaign {}/{} was approved by local actor {:?}, but the current local actor is {:?}; run the verb as the arming operator",
            registration.code_repository,
            registration.worklist_pattern,
            registration.local_actor,
            actor
        );
    }
    Ok(())
}

/// The receipt an arm prints, including the one under `--no-enqueue`.
///
/// `--no-enqueue` is the admission rehearsal: it registers and validates and
/// dispatches nothing, so this object is the only place an operator can read
/// which budget each gate will bind before a pass runs under it. Every gate
/// appears, declared and derived alike, with the sentence that produced it.
fn arm_receipt(result: &Value, warnings: &[String], gate_budgets: &[GateBudget]) -> Value {
    let mut value = if result.is_object() {
        result.clone()
    } else {
        json!({"result": result})
    };
    let object = value.as_object_mut().expect("arm receipt is an object");
    let mut combined = object
        .remove("warnings")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    combined.extend(warnings.iter().map(|warning| json!(warning)));
    object.insert("warnings".to_owned(), Value::Array(combined));
    object.insert("gateBudgets".to_owned(), json!(gate_budgets));
    value
}

/// Every steering surface a pass reads: campaign-wide records plus records
/// addressed to one task. Task keys are task IDs, never task numbers.
#[derive(Debug, Clone, Default)]
struct CampaignSteering {
    master: Vec<Value>,
    tasks: BTreeMap<String, Vec<Value>>,
}

fn steering_comment_high_water(comments: &[Value]) -> u64 {
    comments
        .iter()
        .filter_map(|comment| comment.get("id").and_then(Value::as_u64))
        .max()
        .unwrap_or(0)
}

fn current_task_input_epochs(
    task_input_hashes: &BTreeMap<String, String>,
    steering: &CampaignSteering,
) -> Result<BTreeMap<String, String>> {
    let campaign_high_water = steering_comment_high_water(&steering.master);
    task_input_hashes
        .iter()
        .map(|(task_id, input_hash)| {
            let task_high_water = steering
                .tasks
                .get(task_id)
                .map_or(0, |comments| steering_comment_high_water(comments));
            Ok((
                task_id.clone(),
                task_input_epoch(input_hash, campaign_high_water.max(task_high_water))?,
            ))
        })
        .collect()
}

fn receipt_epoch_is_current(
    task_id: &str,
    receipt_epoch: Option<&str>,
    current_epochs: &BTreeMap<String, String>,
) -> bool {
    current_epochs.is_empty()
        || receipt_epoch.is_none()
        || current_epochs.get(task_id).map(String::as_str) == receipt_epoch
}

fn local_steering_paths(state_dir: &Path, registration_id: &str) -> LocalSteeringPaths {
    let directory = state_dir.join("campaigns/steering").join(registration_id);
    LocalSteeringPaths {
        log: directory.join("steering-v1.jsonl"),
        lock: directory.join("steering.lock"),
        cursor: directory.join("dispatch-cursor-v1.json"),
        directory,
    }
}

fn open_local_steering_lock(paths: &LocalSteeringPaths, exclusive: bool) -> Result<fs::File> {
    fs::create_dir_all(&paths.directory).with_context(|| {
        format!(
            "cannot create campaign steering directory {}",
            paths.directory.display()
        )
    })?;
    fs::set_permissions(&paths.directory, fs::Permissions::from_mode(0o700)).with_context(
        || {
            format!(
                "cannot secure campaign steering directory {}",
                paths.directory.display()
            )
        },
    )?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&paths.lock)
        .with_context(|| {
            format!(
                "cannot open campaign steering lock {}",
                paths.lock.display()
            )
        })?;
    if !lock.metadata()?.is_file() {
        bail!(
            "campaign steering lock {} is not a regular file",
            paths.lock.display()
        );
    }
    if exclusive {
        FileExt::lock_exclusive(&lock)
    } else {
        FileExt::lock_shared(&lock)
    }
    .with_context(|| {
        format!(
            "cannot lock campaign steering source {}",
            paths.lock.display()
        )
    })?;
    Ok(lock)
}

fn open_bounded_local_steering_log(path: &Path) -> Result<Option<fs::File>> {
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot open campaign steering log {}", path.display()))
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_CAMPAIGN_STEERING_LOG_BYTES {
        bail!(
            "campaign steering log {} is not a bounded regular file",
            path.display()
        );
    }
    Ok(Some(file))
}

fn parse_steering_time(value: &str, context: &str) -> Result<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value)
        .with_context(|| format!("{context} is not an RFC 3339 timestamp"))
}

fn valid_steering_observation(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn contains_completion_trailer(value: &str) -> bool {
    value.lines().any(|line| {
        COMPLETION_TRAILER_PREFIXES.iter().any(|prefix| {
            line.get(..prefix.len())
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
        })
    })
}

fn validate_local_steering_record(
    record: &LocalSteeringRecordV1,
    expected_sequence: u64,
    registration: &CampaignRegistration,
) -> Result<DateTime<chrono::FixedOffset>> {
    if record.schema_version != CAMPAIGN_STEERING_SCHEMA_VERSION
        || record.sequence != expected_sequence
        || record.registration_id != registration.registration_id
        || record.comment.id != record.sequence
        || record.comment.author != registration.local_actor
        || record
            .task_id
            .as_deref()
            .is_some_and(|task_id| !safe_task_id(task_id))
        || record.comment.body.contains('\0')
        || record.comment.body.chars().count() > MAX_CAMPAIGN_STEERING_BODY_CHARS
        || contains_completion_trailer(&record.comment.body)
    {
        bail!(
            "campaign steering record {} violates steering-v1 invariants",
            expected_sequence
        );
    }
    let expected_url = format!(
        "local://campaign/{}/steering/{}",
        registration.registration_id, record.sequence
    );
    if record.comment.url != expected_url {
        bail!(
            "campaign steering record {} has an invalid local URL",
            record.sequence
        );
    }
    let created = parse_steering_time(
        &record.comment.created_at,
        &format!("campaign steering record {} createdAt", record.sequence),
    )?;
    let updated = parse_steering_time(
        &record.comment.updated_at,
        &format!("campaign steering record {} updatedAt", record.sequence),
    )?;
    let embargo = parse_steering_time(
        &record.do_not_dispatch_before,
        &format!(
            "campaign steering record {} doNotDispatchBefore",
            record.sequence
        ),
    )?;
    if updated != created
        || embargo
            != created + chrono::Duration::milliseconds(CAMPAIGN_STEERING_EMBARGO_MILLISECONDS)
    {
        bail!(
            "campaign steering record {} has inconsistent append-only timestamps",
            record.sequence
        );
    }
    Ok(embargo)
}

fn read_local_steering_records_locked(
    paths: &LocalSteeringPaths,
    registration: &CampaignRegistration,
) -> Result<Vec<LocalSteeringRecordV1>> {
    let Some(mut file) = open_bounded_local_steering_log(&paths.log)? else {
        return Ok(Vec::new());
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bail!(
            "campaign steering log {} has an incomplete final record",
            paths.log.display()
        );
    }
    let mut records = Vec::new();
    let mut per_target = BTreeMap::<Option<String>, usize>::new();
    let mut prior_embargo = None;
    if bytes.is_empty() {
        return Ok(records);
    }
    for (index, line) in bytes[..bytes.len() - 1]
        .split(|byte| *byte == b'\n')
        .enumerate()
    {
        if line.is_empty() {
            bail!(
                "campaign steering log {} has an empty record at line {}",
                paths.log.display(),
                index + 1
            );
        }
        let value: Value = serde_json::from_slice(line).with_context(|| {
            format!(
                "campaign steering log {} has an invalid record at line {}",
                paths.log.display(),
                index + 1
            )
        })?;
        if !value
            .as_object()
            .is_some_and(|record| record.contains_key("taskId"))
        {
            bail!(
                "campaign steering log {} record at line {} omits taskId",
                paths.log.display(),
                index + 1
            );
        }
        let record: LocalSteeringRecordV1 = serde_json::from_value(value).with_context(|| {
            format!(
                "campaign steering log {} has an invalid record at line {}",
                paths.log.display(),
                index + 1
            )
        })?;
        let sequence = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| invalid("campaign steering sequence is exhausted"))?;
        let embargo = validate_local_steering_record(&record, sequence, registration)?;
        if prior_embargo.is_some_and(|prior| embargo <= prior) {
            bail!(
                "campaign steering record {} does not advance doNotDispatchBefore",
                sequence
            );
        }
        prior_embargo = Some(embargo);
        let count = per_target.entry(record.task_id.clone()).or_default();
        *count += 1;
        if *count > MAX_CAMPAIGN_STEERING_PER_TARGET {
            bail!(
                "campaign steering target {:?} has more than {} records",
                record.task_id,
                MAX_CAMPAIGN_STEERING_PER_TARGET
            );
        }
        records.push(record);
    }
    Ok(records)
}

fn read_local_steering_cursor_locked(
    paths: &LocalSteeringPaths,
    registration: &CampaignRegistration,
    records: &[LocalSteeringRecordV1],
) -> Result<Option<LocalSteeringCursorV1>> {
    let mut file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&paths.cursor)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "cannot open campaign steering cursor {}",
                    paths.cursor.display()
                )
            })
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        bail!(
            "campaign steering cursor {} is not a bounded regular file",
            paths.cursor.display()
        );
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let cursor: LocalSteeringCursorV1 = serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "campaign steering cursor {} is invalid",
            paths.cursor.display()
        )
    })?;
    let log_high_water = records.last().map_or(0, |record| record.sequence);
    if cursor.schema_version != CAMPAIGN_STEERING_CURSOR_SCHEMA_VERSION
        || cursor.registration_id != registration.registration_id
        || cursor.high_water > log_high_water
        || !valid_steering_observation(&cursor.observation)
    {
        bail!(
            "campaign steering cursor {} violates cursor-v1 invariants",
            paths.cursor.display()
        );
    }
    let dispatched_at = parse_steering_time(
        &cursor.dispatched_at,
        "campaign steering cursor dispatchedAt",
    )?;
    if cursor.high_water > 0 {
        let index = usize::try_from(cursor.high_water - 1)
            .map_err(|_| invalid("campaign steering cursor high-water is exhausted"))?;
        let embargo = parse_steering_time(
            &records[index].do_not_dispatch_before,
            "campaign steering cursor high-water embargo",
        )?;
        if dispatched_at < embargo {
            bail!(
                "campaign steering cursor {} advanced before its embargo",
                paths.cursor.display()
            );
        }
    }
    Ok(Some(cursor))
}

fn local_steering_snapshot_from_records(
    paths: &LocalSteeringPaths,
    registration: &CampaignRegistration,
    records: &[LocalSteeringRecordV1],
) -> Result<LocalSteeringSnapshot> {
    let high_water = records.last().map_or(0, |record| record.sequence);
    let _cursor = read_local_steering_cursor_locked(paths, registration, records)?;
    let mut steering = CampaignSteering::default();
    for record in records {
        let comment = serde_json::to_value(&record.comment)?;
        match &record.task_id {
            Some(task_id) => steering
                .tasks
                .entry(task_id.clone())
                .or_default()
                .push(comment),
            None => steering.master.push(comment),
        }
    }
    Ok(LocalSteeringSnapshot {
        steering,
        source: LocalSteeringSourceV1 {
            schema_version: CAMPAIGN_STEERING_SCHEMA_VERSION,
            kind: "local-jsonl",
            registration_id: registration.registration_id.clone(),
            local_actor: registration.local_actor.clone(),
            log_path: paths.log.clone(),
            lock_path: paths.lock.clone(),
            prepared_cursor: high_water,
        },
        do_not_dispatch_before: records.last().map(|record| {
            parse_steering_time(
                &record.do_not_dispatch_before,
                "campaign steering doNotDispatchBefore",
            )
            .expect("validated steering timestamp must parse")
        }),
    })
}

fn read_local_steering_snapshot(
    state_dir: &Path,
    registration: &CampaignRegistration,
) -> Result<LocalSteeringSnapshot> {
    let paths = local_steering_paths(state_dir, &registration.registration_id);
    let lock = open_local_steering_lock(&paths, false)?;
    let records = read_local_steering_records_locked(&paths, registration)?;
    let snapshot = local_steering_snapshot_from_records(&paths, registration, &records);
    let unlock = FileExt::unlock(&lock).with_context(|| {
        format!(
            "cannot unlock campaign steering source {}",
            paths.lock.display()
        )
    });
    match (snapshot, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(snapshot), Ok(())) => Ok(snapshot),
    }
}

/// Read steering for projections without creating the steering directory or
/// lock file. A real source always has its lock; a partially present source is
/// rejected rather than read without synchronization.
///
/// The projection runs under the shared lock, so every reader — the epoch
/// derivation and the inbox alike — sees one consistent prefix of the
/// append-only log.
fn read_existing_local_steering_with<T: Default>(
    state_dir: &Path,
    registration: &CampaignRegistration,
    project: impl FnOnce(&LocalSteeringPaths, &[LocalSteeringRecordV1]) -> Result<T>,
) -> Result<T> {
    fn path_is_present(path: &Path) -> Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("cannot inspect steering path {}", path.display())),
        }
    }

    let paths = local_steering_paths(state_dir, &registration.registration_id);
    let lock = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&paths.lock)
    {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if path_is_present(&paths.log)? || path_is_present(&paths.cursor)? {
                bail!(
                    "campaign steering source exists without its lock: {}",
                    paths.lock.display()
                );
            }
            return Ok(T::default());
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "cannot open campaign steering lock {}",
                    paths.lock.display()
                )
            })
        }
    };
    if !lock.metadata()?.is_file() {
        bail!(
            "campaign steering lock {} is not a regular file",
            paths.lock.display()
        );
    }
    FileExt::lock_shared(&lock).with_context(|| {
        format!(
            "cannot lock campaign steering source {}",
            paths.lock.display()
        )
    })?;
    let read = (|| {
        let records = read_local_steering_records_locked(&paths, registration)?;
        project(&paths, &records)
    })();
    let unlock = FileExt::unlock(&lock).with_context(|| {
        format!(
            "cannot unlock campaign steering source {}",
            paths.lock.display()
        )
    });
    match (read, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(projected), Ok(())) => Ok(projected),
    }
}

fn read_existing_local_steering(
    state_dir: &Path,
    registration: &CampaignRegistration,
) -> Result<CampaignSteering> {
    read_existing_local_steering_with(state_dir, registration, |paths, records| {
        Ok(local_steering_snapshot_from_records(paths, registration, records)?.steering)
    })
}

/// The ordered steering records themselves, which is what an answer looks
/// like before it is folded into an epoch.
fn read_existing_local_steering_records(
    state_dir: &Path,
    registration: &CampaignRegistration,
) -> Result<Vec<LocalSteeringRecordV1>> {
    read_existing_local_steering_with(state_dir, registration, |_, records| Ok(records.to_vec()))
}

fn ensure_local_steering_log_locked(paths: &LocalSteeringPaths) -> Result<()> {
    let existed = paths.log.exists();
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&paths.log)
        .with_context(|| {
            format!(
                "cannot create campaign steering log {}",
                paths.log.display()
            )
        })?;
    if !file.metadata()?.is_file() {
        bail!(
            "campaign steering log {} is not a regular file",
            paths.log.display()
        );
    }
    if !existed {
        file.sync_all()?;
        fs::File::open(&paths.directory)?.sync_all()?;
    }
    Ok(())
}

fn append_local_steering_at(
    state_dir: &Path,
    registration: &CampaignRegistration,
    task_id: Option<String>,
    body: String,
    now: DateTime<Utc>,
) -> Result<LocalSteeringRecordV1> {
    if body.contains('\0') || body.chars().count() > MAX_CAMPAIGN_STEERING_BODY_CHARS {
        return Err(invalid(format!(
            "campaign steering text must contain no NUL byte and at most {MAX_CAMPAIGN_STEERING_BODY_CHARS} characters"
        )));
    }
    if body.trim().is_empty() {
        return Err(invalid("campaign steering text must not be empty"));
    }
    if contains_completion_trailer(&body) {
        return Err(invalid(
            "campaign steering text contains a reserved tally completion trailer",
        ));
    }
    if task_id
        .as_deref()
        .is_some_and(|task_id| !safe_task_id(task_id))
    {
        return Err(invalid("campaign steering --task is not a safe task ID"));
    }
    let paths = local_steering_paths(state_dir, &registration.registration_id);
    let lock = open_local_steering_lock(&paths, true)?;
    ensure_local_steering_log_locked(&paths)?;
    let records = read_local_steering_records_locked(&paths, registration)?;
    let high_water = records.last().map_or(0, |record| record.sequence);
    let _cursor = read_local_steering_cursor_locked(&paths, registration, &records)?;
    let target_count = records
        .iter()
        .filter(|record| record.task_id == task_id)
        .count();
    if target_count >= MAX_CAMPAIGN_STEERING_PER_TARGET {
        bail!(
            "campaign steering target {:?} already has the maximum {} records",
            task_id,
            MAX_CAMPAIGN_STEERING_PER_TARGET
        );
    }
    let sequence = high_water
        .checked_add(1)
        .ok_or_else(|| invalid("campaign steering sequence is exhausted"))?;
    let mut created =
        DateTime::parse_from_rfc3339(&now.to_rfc3339_opts(SecondsFormat::Millis, true))
            .expect("UTC timestamp formatted by chrono must parse")
            .with_timezone(&Utc);
    if let Some(last) = records.last() {
        let prior = parse_steering_time(
            &last.comment.created_at,
            "prior campaign steering createdAt",
        )?
        .with_timezone(&Utc);
        if created <= prior {
            created = prior + chrono::Duration::milliseconds(1);
        }
    }
    let created_at = created.to_rfc3339_opts(SecondsFormat::Millis, true);
    let do_not_dispatch_before = (created
        + chrono::Duration::milliseconds(CAMPAIGN_STEERING_EMBARGO_MILLISECONDS))
    .to_rfc3339_opts(SecondsFormat::Millis, true);
    let record = LocalSteeringRecordV1 {
        schema_version: CAMPAIGN_STEERING_SCHEMA_VERSION,
        sequence,
        registration_id: registration.registration_id.clone(),
        task_id,
        do_not_dispatch_before,
        comment: LocalSteeringCommentV1 {
            id: sequence,
            url: format!(
                "local://campaign/{}/steering/{sequence}",
                registration.registration_id
            ),
            author: registration.local_actor.clone(),
            body,
            created_at: created_at.clone(),
            updated_at: created_at,
        },
    };
    validate_local_steering_record(&record, sequence, registration)?;
    let mut encoded = serde_json::to_vec(&record)?;
    encoded.push(b'\n');
    let append_result = (|| -> Result<()> {
        let mut log = fs::OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&paths.log)
            .with_context(|| {
                format!(
                    "cannot append campaign steering log {}",
                    paths.log.display()
                )
            })?;
        let final_size = log
            .metadata()?
            .len()
            .checked_add(u64::try_from(encoded.len())?)
            .ok_or_else(|| invalid("campaign steering log size is exhausted"))?;
        if final_size > MAX_CAMPAIGN_STEERING_LOG_BYTES {
            bail!("campaign steering log exceeds 128 MiB");
        }
        log.write_all(&encoded)?;
        log.sync_all()?;
        Ok(())
    })();
    let unlock = FileExt::unlock(&lock).with_context(|| {
        format!(
            "cannot unlock campaign steering source {}",
            paths.lock.display()
        )
    });
    match (append_result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(record),
    }
}

fn open_local_steering_dispatch_at(
    state_dir: &Path,
    registration: &CampaignRegistration,
    now: DateTime<Utc>,
) -> Result<LocalSteeringDispatchState> {
    let paths = local_steering_paths(state_dir, &registration.registration_id);
    let lock = open_local_steering_lock(&paths, true)?;
    ensure_local_steering_log_locked(&paths)?;
    let records = read_local_steering_records_locked(&paths, registration)?;
    let snapshot = local_steering_snapshot_from_records(&paths, registration, &records)?;
    if let Some(embargo) = snapshot.do_not_dispatch_before {
        if embargo > now.fixed_offset() {
            FileExt::unlock(&lock).with_context(|| {
                format!(
                    "cannot unlock embargoed campaign steering source {}",
                    paths.lock.display()
                )
            })?;
            return Ok(LocalSteeringDispatchState::Embargoed(embargo));
        }
    }
    Ok(LocalSteeringDispatchState::Ready(LocalSteeringDispatch {
        lock,
        cursor_path: paths.cursor,
        directory: paths.directory,
        registration_id: registration.registration_id.clone(),
        snapshot: Box::new(snapshot),
    }))
}

fn write_local_steering_cursor(
    path: &Path,
    directory: &Path,
    cursor: &LocalSteeringCursorV1,
) -> Result<()> {
    let temporary = directory.join(format!(".dispatch-cursor.{}.tmp", uuid::Uuid::now_v7()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary)
        .with_context(|| {
            format!(
                "cannot create campaign steering cursor {}",
                temporary.display()
            )
        })?;
    serde_json::to_writer(&mut file, cursor)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("cannot publish campaign steering cursor {}", path.display()))?;
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

impl LocalSteeringDispatch {
    fn commit(self, observation: &str, now: DateTime<Utc>) -> Result<()> {
        if !valid_steering_observation(observation) {
            return Err(invalid(
                "campaign steering dispatch observation is not a sha256 digest",
            ));
        }
        if self
            .snapshot
            .do_not_dispatch_before
            .is_some_and(|embargo| now.fixed_offset() < embargo)
        {
            return Err(invalid(
                "campaign steering cursor cannot advance before the steering embargo",
            ));
        }
        let cursor = LocalSteeringCursorV1 {
            schema_version: CAMPAIGN_STEERING_CURSOR_SCHEMA_VERSION,
            registration_id: self.registration_id.clone(),
            high_water: self.snapshot.source.prepared_cursor,
            dispatched_at: now.to_rfc3339_opts(SecondsFormat::Millis, true),
            observation: observation.to_owned(),
        };
        let write = write_local_steering_cursor(&self.cursor_path, &self.directory, &cursor);
        let unlock = FileExt::unlock(&self.lock).with_context(|| {
            format!(
                "cannot unlock dispatched campaign steering source {}",
                self.lock_path().display()
            )
        });
        match (write, unlock) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    fn lock_path(&self) -> PathBuf {
        self.snapshot.source.lock_path.clone()
    }
}

fn local_attempt_receipts_path(state_dir: &Path, campaign: &str) -> Result<PathBuf> {
    if !safe_component(campaign) {
        return Err(invalid(
            "campaign name cannot identify a local attempt-receipts log",
        ));
    }
    Ok(state_dir
        .join("campaigns/attempt-receipts")
        .join(campaign)
        .join(ATTEMPT_RECEIPTS_FILE))
}

fn local_attempt_receipt_authority_path(state_dir: &Path, campaign: &str) -> Result<PathBuf> {
    Ok(local_attempt_receipts_path(state_dir, campaign)?
        .parent()
        .expect("attempt-receipts path always has a parent")
        .join(ATTEMPT_RECEIPT_AUTHORITY_FILE))
}

fn write_local_attempt_receipt_authority(
    state_dir: &Path,
    graph: &CampaignGraph,
    arm_serial: u64,
) -> Result<()> {
    let campaign = &graph.canonical.manifest.name;
    let path = local_attempt_receipt_authority_path(state_dir, campaign)?;
    let directory = path
        .parent()
        .expect("attempt-receipt authority path always has a parent");
    fs::create_dir_all(directory).with_context(|| {
        format!(
            "cannot create attempt-receipts directory {}",
            directory.display()
        )
    })?;
    let metadata = fs::symlink_metadata(directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "attempt-receipts parent must be a real directory: {}",
            directory.display()
        );
    }
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    let authority = AttemptReceiptAuthorityV1::new(
        campaign,
        LOCAL_CAMPAIGN_ISSUE_NUMBER.to_string(),
        arm_serial,
        graph.worklist_sha256.clone(),
    )
    .map_err(|error| invalid(format!("cannot publish attempt receipt authority: {error}")))?;
    let temporary = directory.join(format!(".receipt-authority.{}.tmp", uuid::Uuid::now_v7()));
    let write = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .with_context(|| {
                format!(
                    "cannot create attempt receipt authority {}",
                    temporary.display()
                )
            })?;
        serde_json::to_writer(&mut file, &authority)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, &path).with_context(|| {
            format!(
                "cannot publish attempt receipt authority {}",
                path.display()
            )
        })?;
        fs::File::open(directory)?.sync_all()?;
        Ok(())
    })();
    if write.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write
}

fn local_attempt_receipt_url(campaign: &str, sequence: u64) -> String {
    format!("local://campaign/{campaign}/attempt-receipts/{sequence}")
}

fn validate_attempt_receipt_text(value: &Value, context: &str, maximum: usize) -> Result<()> {
    let text = value
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| invalid(format!("{context} must be non-empty text")))?;
    if text.chars().count() > maximum {
        return Err(invalid(format!("{context} exceeds {maximum} characters")));
    }
    if text
        .chars()
        .any(|character| character < '\u{20}' && !matches!(character, '\n' | '\t' | '\r'))
    {
        return Err(invalid(format!(
            "{context} contains unsupported control characters"
        )));
    }
    Ok(())
}

fn validate_attempt_receipt_string(value: &Value, context: &str, maximum: usize) -> Result<()> {
    value
        .as_str()
        .filter(|text| {
            !text.is_empty()
                && !text.chars().any(|character| character < '\u{20}')
                && text.chars().count() <= maximum
        })
        .ok_or_else(|| {
            invalid(format!(
                "{context} must be a non-empty bounded string without control characters"
            ))
        })?;
    Ok(())
}

/// The prepared amendment a blocked diagnosis may carry, as the inbox hands
/// it on.
///
/// The driver that writes it already normalizes the object and refuses one
/// beside a non-blocking verdict; this side only has to refuse a shape it
/// could not render — the field stays optional and a null stays absent, so
/// judge-era history reads exactly as it always did.
fn validated_attempt_receipt_proposal(
    value: Option<&Value>,
    context: &str,
) -> Result<Option<Box<Value>>> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    if !value.is_object() {
        return Err(invalid(format!("{context}.proposal must be an object")));
    }
    if serde_json::to_string(value)?.chars().count() > MAX_DIAGNOSIS_CHARS {
        return Err(invalid(format!(
            "{context}.proposal exceeds {MAX_DIAGNOSIS_CHARS} characters"
        )));
    }
    Ok(Some(Box::new(value.clone())))
}

fn validate_local_attempt_receipt(
    candidate: Value,
    path: &Path,
    expected_sequence: u64,
    campaign: &str,
    issue_number: u64,
) -> Result<LocalAttemptReceiptV1> {
    let context = format!("attempt receipt {expected_sequence} in {}", path.display());
    let object = candidate
        .as_object()
        .ok_or_else(|| invalid(format!("{context} must be an object")))?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{context}.kind must be a string")))?;
    let schema_version = object
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(format!("{context}.schemaVersion is invalid")))?;
    if !matches!(
        schema_version,
        LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION | ATTEMPT_RECEIPT_SCHEMA_VERSION
    ) {
        return Err(invalid(format!(
            "{context}.schemaVersion must equal {LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION} or {ATTEMPT_RECEIPT_SCHEMA_VERSION}"
        )));
    }
    let mut common: Vec<&'static str> = vec![
        "schemaVersion",
        "sequence",
        "kind",
        "campaign",
        "issueNumber",
    ];
    if schema_version == ATTEMPT_RECEIPT_SCHEMA_VERSION {
        common.extend(["armSerial", "worklistSha256", "writtenAt", "actor"]);
    }
    let fields = |specific: &[&'static str]| -> (Vec<&'static str>, BTreeSet<&'static str>) {
        let required = common
            .iter()
            .copied()
            .chain(specific.iter().copied())
            .collect::<Vec<_>>();
        let allowed = required.iter().copied().collect::<BTreeSet<_>>();
        (required, allowed)
    };
    let (required, allowed): (Vec<&str>, BTreeSet<&str>) = match kind {
        "diagnosis" => {
            let (required, mut allowed) = fields(&["taskId", "attempt", "diagnosis", "redaction"]);
            // Judge-era schema-1 history and current schema-2 records may
            // carry these fields. This fold does not consume their values.
            allowed.extend(["verdict", "proposal"]);
            (required, allowed)
        }
        "retry" => fields(&["taskId", "attempt", "reason", "redaction"]),
        "worker-outcome" => fields(&[
            "taskId",
            "taskRevision",
            "taskUuid",
            "outcome",
            "paths",
            "reason",
        ]),
        "escalation" => fields(&["body"]),
        "gate-observation" => fields(&["gateId", "durationSec"]),
        "pardon" => {
            let (required, mut allowed) = fields(&["tasks"]);
            allowed.extend(["reason", "actor", "nonce"]);
            (required, allowed)
        }
        _ => return Err(invalid(format!("{context} has unknown kind {kind:?}"))),
    };
    let mut allowed = allowed;
    if schema_version == ATTEMPT_RECEIPT_SCHEMA_VERSION
        && matches!(kind, "diagnosis" | "retry" | "worker-outcome")
    {
        allowed.insert("inputEpoch");
    }
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(field.as_str()))
    {
        return Err(invalid(format!(
            "{context} has unsupported field {field:?}"
        )));
    }
    if let Some(field) = required
        .into_iter()
        .find(|field| !object.contains_key(*field))
    {
        return Err(invalid(format!("{context} is missing field {field:?}")));
    }
    let expected_issue_number = issue_number.to_string();
    if object.get("sequence").and_then(Value::as_u64) != Some(expected_sequence)
        || object.get("campaign").and_then(Value::as_str) != Some(campaign)
        || object.get("issueNumber").and_then(Value::as_str) != Some(expected_issue_number.as_str())
    {
        return Err(invalid(format!(
            "{context} has invalid identity or sequence"
        )));
    }
    if schema_version == ATTEMPT_RECEIPT_SCHEMA_VERSION {
        validate_attempt_receipt_stamp(
            object.get("armSerial").and_then(Value::as_u64),
            object.get("worklistSha256").and_then(Value::as_str),
            object.get("writtenAt").and_then(Value::as_str),
            object.get("actor").and_then(Value::as_str),
        )
        .map_err(|error| invalid(format!("{context} has invalid stamp: {error}")))?;
    }
    let input_epoch = object
        .get("inputEpoch")
        .map(|value| {
            let epoch = value
                .as_str()
                .filter(|epoch| is_sha256_identity(epoch))
                .ok_or_else(|| {
                    invalid(format!(
                        "{context}.inputEpoch must be a lowercase SHA-256 identity"
                    ))
                })?;
            Ok::<String, anyhow::Error>(epoch.to_owned())
        })
        .transpose()?;

    let written_at = object
        .get("writtenAt")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    let task_attempt = |payload: &str, maximum: usize| -> Result<(String, u8, String)> {
        let task_id = object
            .get("taskId")
            .and_then(Value::as_str)
            .filter(|task_id| safe_task_id(task_id))
            .ok_or_else(|| invalid(format!("{context}.taskId is unsafe")))?;
        let attempt = object
            .get("attempt")
            .and_then(Value::as_u64)
            .and_then(|attempt| u8::try_from(attempt).ok())
            .filter(|attempt| matches!(attempt, 1 | 2))
            .ok_or_else(|| invalid(format!("{context}.attempt must equal 1 or 2")))?;
        if !matches!(
            object.get("redaction").and_then(Value::as_str),
            Some("conservative-v1" | "conservative-v2")
        ) {
            return Err(invalid(format!("{context}.redaction is unsupported")));
        }
        let text = object
            .get(payload)
            .expect("receipt payload is required above");
        validate_attempt_receipt_text(text, &format!("{context}.{payload}"), maximum)?;
        Ok((
            task_id.to_owned(),
            attempt,
            text.as_str().expect("validated payload is text").to_owned(),
        ))
    };

    match kind {
        "diagnosis" => {
            let (task_id, attempt, diagnosis) = task_attempt("diagnosis", MAX_DIAGNOSIS_CHARS)?;
            let verdict = match object.get("verdict") {
                None => None,
                Some(Value::String(verdict))
                    if matches!(verdict.as_str(), "retry" | "blocked" | "transient") =>
                {
                    Some(verdict.as_str())
                }
                Some(_) => {
                    return Err(invalid(format!(
                        "{context}.verdict must be retry, blocked, or transient"
                    )))
                }
            };
            let proposal = validated_attempt_receipt_proposal(object.get("proposal"), &context)?;
            Ok(LocalAttemptReceiptV1::Diagnosis {
                sequence: expected_sequence,
                task_id,
                attempt,
                input_epoch,
                blocks_task: attempt == 2 || verdict == Some("blocked"),
                diagnosis,
                proposal,
                written_at,
            })
        }
        "retry" => {
            let (task_id, ..) = task_attempt("reason", MAX_RETRY_CHARS)?;
            Ok(LocalAttemptReceiptV1::Retry {
                task_id,
                input_epoch,
            })
        }
        "worker-outcome" => {
            let task_id = object
                .get("taskId")
                .and_then(Value::as_str)
                .filter(|task_id| safe_task_id(task_id))
                .ok_or_else(|| invalid(format!("{context}.taskId is unsafe")))?
                .to_owned();
            let task_revision = object
                .get("taskRevision")
                .and_then(Value::as_str)
                .filter(|revision| is_sha256_identity(revision))
                .ok_or_else(|| {
                    invalid(format!(
                        "{context}.taskRevision must be a lowercase SHA-256 identity"
                    ))
                })?
                .to_owned();
            let task_uuid = object
                .get("taskUuid")
                .and_then(Value::as_str)
                .and_then(|value| {
                    uuid::Uuid::parse_str(value)
                        .ok()
                        .map(|parsed| (value, parsed))
                })
                .filter(|(value, parsed)| parsed.to_string() == **value)
                .map(|(value, _)| value.to_owned())
                .ok_or_else(|| invalid(format!("{context}.taskUuid must be a canonical UUID")))?;
            let outcome = match object.get("outcome").and_then(Value::as_str) {
                Some("needs-authority") => {
                    if !object.get("reason").is_some_and(Value::is_null) {
                        return Err(invalid(format!(
                            "{context}.reason must be null for needs-authority"
                        )));
                    }
                    let paths = object
                        .get("paths")
                        .and_then(Value::as_array)
                        .filter(|paths| !paths.is_empty() && paths.len() <= 128)
                        .ok_or_else(|| {
                            invalid(format!(
                                "{context}.paths must contain between 1 and 128 paths"
                            ))
                        })?;
                    let mut seen = BTreeSet::new();
                    let mut validated = Vec::with_capacity(paths.len());
                    for (index, value) in paths.iter().enumerate() {
                        validate_attempt_receipt_string(
                            value,
                            &format!("{context}.paths[{index}]"),
                            4_096,
                        )?;
                        let path = value.as_str().expect("validated path is text");
                        let pieces = path.split('/').collect::<Vec<_>>();
                        if path.starts_with('/')
                            || path.ends_with('/')
                            || path == "."
                            || pieces
                                .iter()
                                .any(|piece| piece.is_empty() || matches!(*piece, "." | ".."))
                            || !seen.insert(path.to_owned())
                        {
                            return Err(invalid(format!(
                                "{context}.paths[{index}] must be a unique normalized relative path"
                            )));
                        }
                        validated.push(path.to_owned());
                    }
                    WorkerOutcomePayload::NeedsAuthority { paths: validated }
                }
                Some("impossible") => {
                    if !object.get("paths").is_some_and(Value::is_null) {
                        return Err(invalid(format!(
                            "{context}.paths must be null for impossible"
                        )));
                    }
                    let reason = object
                        .get("reason")
                        .ok_or_else(|| invalid(format!("{context}.reason is required")))?;
                    validate_attempt_receipt_text(
                        reason,
                        &format!("{context}.reason"),
                        MAX_DIAGNOSIS_CHARS,
                    )?;
                    WorkerOutcomePayload::Impossible {
                        reason: reason
                            .as_str()
                            .expect("validated reason is text")
                            .to_owned(),
                    }
                }
                _ => {
                    return Err(invalid(format!(
                        "{context}.outcome must be needs-authority or impossible"
                    )))
                }
            };
            Ok(LocalAttemptReceiptV1::WorkerOutcome(LocalWorkerOutcome {
                sequence: expected_sequence,
                task_id,
                task_revision,
                task_uuid,
                input_epoch,
                outcome,
                written_at,
            }))
        }
        "escalation" => {
            validate_attempt_receipt_text(
                object
                    .get("body")
                    .expect("escalation body is required above"),
                &format!("{context}.body"),
                60_000,
            )?;
            Ok(LocalAttemptReceiptV1::Escalation)
        }
        "gate-observation" => {
            let gate_id = object
                .get("gateId")
                .and_then(Value::as_str)
                .filter(|gate_id| safe_component(gate_id) && gate_id.chars().count() <= 80)
                .ok_or_else(|| invalid(format!("{context}.gateId is unsafe")))?
                .to_owned();
            let duration_sec = object
                .get("durationSec")
                .and_then(Value::as_u64)
                .filter(|seconds| *seconds <= MAX_GATE_OBSERVATION_SEC)
                .ok_or_else(|| {
                    invalid(format!(
                        "{context}.durationSec must be an integer of at most {MAX_GATE_OBSERVATION_SEC} seconds"
                    ))
                })?;
            Ok(LocalAttemptReceiptV1::GateObservation {
                gate_id,
                duration_sec,
            })
        }
        "pardon" => {
            let tasks = match object.get("tasks") {
                Some(Value::Null) => None,
                Some(Value::Array(values)) if !values.is_empty() => {
                    let mut tasks = BTreeSet::new();
                    for (index, value) in values.iter().enumerate() {
                        validate_attempt_receipt_string(
                            value,
                            &format!("{context}.tasks[{index}]"),
                            80,
                        )?;
                        let task_id = value.as_str().expect("validated task id is a string");
                        if !safe_task_id(task_id) || !tasks.insert(task_id.to_owned()) {
                            return Err(invalid(format!(
                                "{context}.tasks must contain unique safe task IDs"
                            )));
                        }
                    }
                    Some(tasks)
                }
                _ => {
                    return Err(invalid(format!(
                        "{context}.tasks must be null or a non-empty array"
                    )))
                }
            };
            if let Some(reason) = object.get("reason") {
                validate_attempt_receipt_text(reason, &format!("{context}.reason"), 4_000)?;
            }
            if let Some(actor) = object.get("actor") {
                validate_attempt_receipt_string(actor, &format!("{context}.actor"), 128)?;
            }
            if let Some(value) = object.get("nonce") {
                validate_attempt_receipt_string(value, &format!("{context}.nonce"), 36)?;
                let nonce = value.as_str().expect("validated pardon nonce is a string");
                uuid::Uuid::parse_str(nonce)
                    .map_err(|_| invalid(format!("{context}.nonce must be a UUID")))?;
            }
            Ok(LocalAttemptReceiptV1::Pardon { tasks })
        }
        _ => unreachable!("receipt kind was checked above"),
    }
}

fn read_local_attempt_receipts_locked(
    file: &mut fs::File,
    path: &Path,
    campaign: &str,
    issue_number: u64,
    repair_tail: bool,
) -> Result<Vec<LocalAttemptReceiptV1>> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() > MAX_ATTEMPT_RECEIPTS_LOG_BYTES
    {
        bail!(
            "attempt-receipts log is not a bounded private regular file: {}",
            path.display()
        );
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    if repair_tail && complete != bytes.len() {
        file.set_len(u64::try_from(complete)?)?;
        file.sync_all()?;
    }
    let mut records = Vec::new();
    if complete == 0 {
        return Ok(records);
    }
    for (index, line) in bytes[..complete - 1]
        .split(|byte| *byte == b'\n')
        .enumerate()
    {
        if line.is_empty() {
            bail!(
                "attempt-receipts log {} contains a blank record at line {}",
                path.display(),
                index + 1
            );
        }
        let candidate: Value = serde_json::from_slice(line).with_context(|| {
            format!(
                "attempt receipt {} in {} is invalid JSON",
                index + 1,
                path.display()
            )
        })?;
        let sequence = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| invalid("attempt-receipts sequence is exhausted"))?;
        records.push(validate_local_attempt_receipt(
            candidate,
            path,
            sequence,
            campaign,
            issue_number,
        )?);
    }
    Ok(records)
}

fn read_local_attempt_receipts(
    state_dir: &Path,
    campaign: &str,
    issue_number: u64,
) -> Result<Vec<LocalAttemptReceiptV1>> {
    let path = local_attempt_receipts_path(state_dir, campaign)?;
    let mut file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot open attempt-receipts log {}", path.display()))
        }
    };
    FileExt::lock_shared(&file)
        .with_context(|| format!("cannot lock attempt-receipts log {}", path.display()))?;
    let read = read_local_attempt_receipts_locked(&mut file, &path, campaign, issue_number, false);
    let unlock = FileExt::unlock(&file)
        .with_context(|| format!("cannot unlock attempt-receipts log {}", path.display()));
    match (read, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(records), Ok(())) => Ok(records),
    }
}

/// Every recorded firing this campaign's receipts hold, grouped by gate id.
///
/// A gate id with no record is absent from the map, which is what tells the
/// derivation that the gate has never fired. Receipts are campaign-scoped, so
/// this is the campaign's own history and nobody else's.
fn recorded_gate_observations(
    state_dir: &Path,
    campaign: &str,
) -> Result<BTreeMap<String, Vec<u64>>> {
    let records = read_local_attempt_receipts(state_dir, campaign, LOCAL_CAMPAIGN_ISSUE_NUMBER)?;
    let mut observations = BTreeMap::<String, Vec<u64>>::new();
    for record in &records {
        if let LocalAttemptReceiptV1::GateObservation {
            gate_id,
            duration_sec,
        } = record
        {
            observations
                .entry(gate_id.clone())
                .or_default()
                .push(*duration_sec);
        }
    }
    Ok(observations)
}

/// Bind every authored gate to the budget it will actually run under.
///
/// A declared budget passes through untouched — the declared number IS the
/// budget, the same permanence `maxParallel` carries. A silent budget is
/// resolved here, once, from this campaign's own receipts, and the resolved
/// number is what the admitted contract carries: an admitted gate never leaves
/// this seam holding a guess, and the flow never has to invent one.
///
/// The resolved number is part of the executable graph, so it is covered by the
/// executable digest and by every task input hash. That is deliberate. A gate
/// budget is global execution policy; when it moves, the proof taken under the
/// old wall clock is stale, and the contract already says so
/// (`task_input_hash` folds `manifest.gates` for exactly this reason).
fn resolve_worklist_gate_budgets(
    gates: &[WorklistGate],
    observations: &BTreeMap<String, Vec<u64>>,
) -> (Vec<CampaignGate>, Vec<GateBudget>) {
    let budgets = gates
        .iter()
        .map(|gate| {
            resolve_gate_budget(
                gate.id(),
                gate.declared_runtime_max_sec(),
                observations
                    .get(gate.id())
                    .map_or(&[][..], |durations| durations.as_slice()),
            )
        })
        .collect::<Vec<_>>();
    let resolved = gates
        .iter()
        .zip(&budgets)
        .map(|(gate, budget)| gate.resolved(budget.runtime_max_sec))
        .collect();
    (resolved, budgets)
}

/// One typed doubt addressed to one task, as the operator surface reads it.
///
/// The entry is a projection, never a second artifact: every field is read
/// out of the campaign's own append-only attempt-receipts log, and the answer
/// out of the campaign's own steering log. That is what makes an unanswered
/// entry survive a daemon restart and a re-admission alike — nothing holds it
/// but the two logs that already had to be durable, and neither of them can
/// be rewritten to make an entry go away.
#[derive(Debug, Clone, PartialEq)]
struct CampaignInboxEntry {
    /// The receipt sequence the entry was folded from — its stable identity,
    /// because an append-only log never renumbers.
    sequence: u64,
    kind: &'static str,
    task_id: String,
    /// Present for a diagnosis; a worker outcome is not attempt-numbered.
    attempt: Option<u8>,
    input_epoch: Option<String>,
    written_at: Option<String>,
    /// The typed question, kept verbatim.
    question: String,
    /// The prepared amendment that rode with the question, when there was one.
    proposal: Option<Box<Value>>,
    /// The paths the doubt is about — the authority surface a needs-authority
    /// outcome named. Empty when the question carries its own evidence.
    evidence: Vec<String>,
    answer: Option<CampaignInboxAnswer>,
}

/// The steering record that answered an entry.
///
/// Answering marks; it never deletes. The entry stays in the projection with
/// the answer beside it, because the log it was folded from is append-only
/// and a fact does not stop having happened.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CampaignInboxAnswer {
    sequence: u64,
    /// `None` for an answer addressed to the whole campaign, which addresses
    /// every task in it.
    task_id: Option<String>,
    created_at: String,
}

impl CampaignInboxEntry {
    const fn state(&self) -> &'static str {
        if self.answer.is_some() {
            "answered"
        } else {
            "open"
        }
    }

    fn value(&self, campaign: &str) -> Value {
        json!({
            "sequence": self.sequence,
            "receipt": local_attempt_receipt_url(campaign, self.sequence),
            "kind": self.kind,
            "taskId": self.task_id,
            "attempt": self.attempt,
            "inputEpoch": self.input_epoch,
            "writtenAt": self.written_at,
            "question": self.question,
            "proposal": self.proposal.as_deref().cloned().unwrap_or(Value::Null),
            "evidence": self.evidence,
            "state": self.state(),
            "answeredBy": self.answer.as_ref().map_or(Value::Null, |answer| {
                json!({
                    "sequence": answer.sequence,
                    "taskId": answer.task_id,
                    "createdAt": answer.created_at,
                })
            }),
        })
    }
}

/// Fold this campaign's receipts and steering into the delivery surface.
///
/// The two logs are joined on time rather than on the epoch, deliberately. An
/// epoch moves for a worklist amendment as readily as for a steer, and an
/// entry that a re-admission quietly retired would be an entry the operator
/// never saw — the record's whole complaint about escalations that live in
/// pass stderr. Ordered steering addressed to the task (or to the campaign,
/// which addresses every task in it) after the entry was written is the one
/// act that marks it, and both timestamps are written once into append-only
/// logs, so the mark never comes undone.
fn campaign_inbox_entries(
    records: &[LocalAttemptReceiptV1],
    steering: &[LocalSteeringRecordV1],
) -> Result<Vec<CampaignInboxEntry>> {
    let answers = steering
        .iter()
        .map(|record| {
            let created = parse_steering_time(
                &record.comment.created_at,
                &format!("campaign steering record {} createdAt", record.sequence),
            )?;
            Ok((
                created,
                CampaignInboxAnswer {
                    sequence: record.sequence,
                    task_id: record.task_id.clone(),
                    created_at: record.comment.created_at.clone(),
                },
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut entries = Vec::new();
    for record in records {
        let entry = match record {
            LocalAttemptReceiptV1::Diagnosis {
                sequence,
                task_id,
                attempt,
                input_epoch,
                blocks_task: true,
                diagnosis,
                proposal,
                written_at,
            } => CampaignInboxEntry {
                sequence: *sequence,
                kind: "blocked",
                task_id: task_id.clone(),
                attempt: Some(*attempt),
                input_epoch: input_epoch.clone(),
                written_at: written_at.clone(),
                question: diagnosis.clone(),
                proposal: proposal.clone(),
                evidence: Vec::new(),
                answer: None,
            },
            LocalAttemptReceiptV1::WorkerOutcome(outcome) => {
                let (question, evidence) = match &outcome.outcome {
                    WorkerOutcomePayload::NeedsAuthority { paths } => (
                        format!(
                            "the lane cannot finish inside its own conflict domains and asks for {}",
                            if paths.len() == 1 {
                                "one path".to_owned()
                            } else {
                                format!("{} paths", paths.len())
                            }
                        ),
                        paths.clone(),
                    ),
                    WorkerOutcomePayload::Impossible { reason } => {
                        (reason.clone(), Vec::new())
                    }
                };
                CampaignInboxEntry {
                    sequence: outcome.sequence,
                    kind: outcome.outcome.class(),
                    task_id: outcome.task_id.clone(),
                    attempt: None,
                    input_epoch: outcome.input_epoch.clone(),
                    written_at: outcome.written_at.clone(),
                    question,
                    proposal: None,
                    evidence,
                    answer: None,
                }
            }
            LocalAttemptReceiptV1::Diagnosis { .. }
            | LocalAttemptReceiptV1::Retry { .. }
            | LocalAttemptReceiptV1::Escalation
            | LocalAttemptReceiptV1::Pardon { .. }
            | LocalAttemptReceiptV1::GateObservation { .. } => continue,
        };
        entries.push(entry);
    }

    for entry in &mut entries {
        // An entry the log cannot place in time is answered by the first
        // steering that addressed it at all: judge-era history carries no
        // stamp, and reading it as never-answered would strand it forever.
        let written = entry
            .written_at
            .as_deref()
            .map(|written| {
                parse_steering_time(
                    written,
                    &format!("attempt receipt {} writtenAt", entry.sequence),
                )
            })
            .transpose()?;
        entry.answer = answers
            .iter()
            .find(|(created, answer)| {
                answer
                    .task_id
                    .as_ref()
                    .is_none_or(|task_id| *task_id == entry.task_id)
                    && written.is_none_or(|written| *created >= written)
            })
            .map(|(_, answer)| answer.clone());
    }
    Ok(entries)
}

/// Everything `tally campaign inbox` prints, for one campaign identity.
fn campaign_inbox_value(
    state_dir: &Path,
    registration: &CampaignRegistration,
    campaign: &str,
) -> Result<Value> {
    let records = read_local_attempt_receipts(state_dir, campaign, LOCAL_CAMPAIGN_ISSUE_NUMBER)?;
    let steering = read_existing_local_steering_records(state_dir, registration)?;
    let entries = campaign_inbox_entries(&records, &steering)?;
    let open = entries
        .iter()
        .filter(|entry| entry.answer.is_none())
        .count();
    Ok(json!({
        "schemaVersion": CAMPAIGN_INBOX_SCHEMA_VERSION,
        "campaign": campaign,
        "codeRepository": registration.code_repository,
        "worklistPattern": registration.worklist_pattern,
        "issue": campaign_issue_url(
            &registration.code_repository,
            &registration.worklist_pattern,
        ),
        "open": open,
        "entries": entries
            .iter()
            .map(|entry| entry.value(campaign))
            .collect::<Vec<_>>(),
    }))
}

/// The one verb the standing operator surface has: read the typed doubt this
/// campaign is holding, and see which of it has already been answered.
fn run_campaign_inbox(args: CampaignInboxArgs) -> Result<()> {
    let (code_repository, worklist_pattern) =
        campaign_identity(&args.code_repository, &args.worklist_pattern)?;
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let registration = registry
        .read_campaign(&code_repository, &worklist_pattern)?
        .ok_or_else(|| {
            invalid(format!(
                "campaign {code_repository}/{worklist_pattern} is not armed; arm it before reading its inbox"
            ))
        })?;
    require_local_actor(&registration)?;
    let campaign = campaign_name_for_status(
        read_approved_graph_snapshot(&state_dir, &registration)?.as_ref(),
        &Value::Null,
        &worklist_pattern,
    );
    let inbox = campaign_inbox_value(&state_dir, &registration, &campaign)?;
    if args.json {
        outln!("{}", serde_json::to_string(&inbox)?);
        return Ok(());
    }
    print_campaign_inbox_human(&inbox)
}

fn print_campaign_inbox_human(inbox: &Value) -> Result<()> {
    let entries = inbox["entries"]
        .as_array()
        .ok_or_else(|| invalid("campaign inbox projection has no entries"))?;
    let open = inbox["open"].as_u64().unwrap_or_default();
    outln!(
        "Inbox {}  {} entr{}, {} open",
        compact_text(inbox["campaign"].as_str().unwrap_or("-")),
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" },
        open,
    );
    for entry in entries {
        outln!(
            "[{}] {} {} ({})",
            entry["sequence"].as_u64().unwrap_or_default(),
            compact_text(entry["kind"].as_str().unwrap_or("-")),
            compact_text(entry["taskId"].as_str().unwrap_or("-")),
            compact_text(entry["state"].as_str().unwrap_or("-")),
        );
        if let Some(attempt) = entry["attempt"].as_u64() {
            outln!("  attempt: {attempt}");
        }
        if let Some(epoch) = entry["inputEpoch"].as_str() {
            outln!("  epoch: {}", compact_text(epoch));
        }
        outln!(
            "  {}",
            compact_text(entry["question"].as_str().unwrap_or("-"))
        );
        if let Some(paths) = entry["evidence"]
            .as_array()
            .filter(|paths| !paths.is_empty())
        {
            outln!(
                "  evidence: {}",
                paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(compact_text)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !entry["proposal"].is_null() {
            outln!("  proposal: {}", serde_json::to_string(&entry["proposal"])?);
        }
        outln!(
            "  receipt: {}",
            compact_text(entry["receipt"].as_str().unwrap_or("-"))
        );
        if let Some(answer) = entry["answeredBy"].as_object() {
            outln!(
                "  answered by steer {} at {}",
                answer["sequence"].as_u64().unwrap_or_default(),
                compact_text(answer["createdAt"].as_str().unwrap_or("-")),
            );
        }
    }
    Ok(())
}

fn active_escalated_tasks_from_receipts(
    records: &[LocalAttemptReceiptV1],
    current_revisions: &BTreeMap<String, String>,
    current_epochs: &BTreeMap<String, String>,
) -> Result<BTreeSet<String>> {
    #[derive(Default)]
    struct Escalation {
        contributors: BTreeSet<String>,
        covered: BTreeSet<String>,
    }

    let mut diagnoses = BTreeMap::<String, Vec<u8>>::new();
    let mut authority_requests = BTreeSet::<String>::new();
    let mut lifetime_attempts = BTreeMap::<String, usize>::new();
    let mut escalations = Vec::<Escalation>::new();
    for record in records {
        match record {
            LocalAttemptReceiptV1::Diagnosis {
                task_id,
                input_epoch,
                blocks_task,
                ..
            } if current_revisions.contains_key(task_id)
                && receipt_epoch_is_current(task_id, input_epoch.as_deref(), current_epochs) =>
            {
                *lifetime_attempts.entry(task_id.clone()).or_default() += 1;
                diagnoses
                    .entry(task_id.clone())
                    .or_default()
                    .push(u8::from(*blocks_task));
            }
            LocalAttemptReceiptV1::Diagnosis { task_id, .. } => {
                *lifetime_attempts.entry(task_id.clone()).or_default() += 1;
            }
            LocalAttemptReceiptV1::Retry { task_id, .. } => {
                *lifetime_attempts.entry(task_id.clone()).or_default() += 1;
            }
            LocalAttemptReceiptV1::WorkerOutcome(outcome)
                if current_revisions.get(&outcome.task_id) == Some(&outcome.task_revision)
                    && receipt_epoch_is_current(
                        &outcome.task_id,
                        outcome.input_epoch.as_deref(),
                        current_epochs,
                    )
                    && matches!(outcome.outcome, WorkerOutcomePayload::NeedsAuthority { .. }) =>
            {
                authority_requests.insert(outcome.task_id.clone());
            }
            LocalAttemptReceiptV1::WorkerOutcome(_) => {}
            LocalAttemptReceiptV1::Escalation => {
                let mut contributors: BTreeSet<_> = diagnoses
                    .iter()
                    .filter(|(_, blocking)| blocking.contains(&1))
                    .map(|(task_id, _)| task_id.clone())
                    .collect();
                contributors.extend(authority_requests.iter().cloned());
                contributors.extend(
                    lifetime_attempts
                        .iter()
                        .filter(|(task_id, attempts)| {
                            current_revisions.contains_key(*task_id)
                                && **attempts >= MAX_TASK_LIFETIME_ATTEMPTS
                        })
                        .map(|(task_id, _)| task_id.clone()),
                );
                if !contributors.is_empty() {
                    escalations.push(Escalation {
                        contributors,
                        covered: BTreeSet::new(),
                    });
                }
            }
            LocalAttemptReceiptV1::GateObservation { .. } => {}
            LocalAttemptReceiptV1::Pardon { tasks: None, .. } => {
                diagnoses.clear();
                authority_requests.clear();
                escalations.clear();
            }
            LocalAttemptReceiptV1::Pardon {
                tasks: Some(scope), ..
            } => {
                for task_id in scope {
                    diagnoses.remove(task_id);
                    authority_requests.remove(task_id);
                }
                for escalation in &mut escalations {
                    escalation
                        .covered
                        .extend(scope.intersection(&escalation.contributors).cloned());
                }
                escalations.retain(|escalation| {
                    escalation.contributors.is_empty()
                        || escalation.contributors != escalation.covered
                });
            }
        }
    }
    if escalations.len() > 1 {
        return Err(invalid("multiple machine escalations claim this campaign"));
    }
    Ok(escalations
        .into_iter()
        .flat_map(|escalation| {
            escalation
                .contributors
                .difference(&escalation.covered)
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|task_id| current_revisions.contains_key(task_id))
        .collect())
}

fn active_local_escalated_tasks(
    state_dir: &Path,
    graph: &CampaignGraph,
    registration: &CampaignRegistration,
) -> Result<BTreeSet<String>> {
    let current_revisions = graph.task_completion_revisions.clone();
    let records = read_local_attempt_receipts(
        state_dir,
        &graph.canonical.manifest.name,
        LOCAL_CAMPAIGN_ISSUE_NUMBER,
    )?;
    let steering = read_local_steering_snapshot(state_dir, registration)?;
    let current_epochs = current_task_input_epochs(&graph.task_input_hashes, &steering.steering)?;
    active_escalated_tasks_from_receipts(&records, &current_revisions, &current_epochs)
}

fn active_worker_outcomes_from_receipts(
    records: &[LocalAttemptReceiptV1],
    current_revisions: &BTreeMap<String, String>,
    current_epochs: &BTreeMap<String, String>,
) -> BTreeMap<String, LocalWorkerOutcome> {
    let mut outcomes = BTreeMap::new();
    for record in records {
        match record {
            LocalAttemptReceiptV1::WorkerOutcome(outcome)
                if current_revisions.get(&outcome.task_id) == Some(&outcome.task_revision)
                    && receipt_epoch_is_current(
                        &outcome.task_id,
                        outcome.input_epoch.as_deref(),
                        current_epochs,
                    ) =>
            {
                outcomes.insert(outcome.task_id.clone(), outcome.clone());
            }
            LocalAttemptReceiptV1::Pardon { tasks: None } => outcomes.clear(),
            LocalAttemptReceiptV1::Pardon { tasks: Some(tasks) } => {
                for task_id in tasks {
                    outcomes.remove(task_id);
                }
            }
            LocalAttemptReceiptV1::Diagnosis { .. }
            | LocalAttemptReceiptV1::Retry { .. }
            | LocalAttemptReceiptV1::WorkerOutcome(_)
            | LocalAttemptReceiptV1::Escalation
            | LocalAttemptReceiptV1::GateObservation { .. } => {}
        }
    }
    outcomes
}

fn worker_outcome_status_value(campaign: &str, outcome: &LocalWorkerOutcome) -> Value {
    let (paths, reason, claim) = match &outcome.outcome {
        WorkerOutcomePayload::NeedsAuthority { paths } => {
            (json!(paths), Value::Null, Value::Bool(false))
        }
        WorkerOutcomePayload::Impossible { reason } => {
            (Value::Null, json!(reason), Value::Bool(true))
        }
    };
    json!({
        "kind": outcome.outcome.class(),
        "receipt": local_attempt_receipt_url(campaign, outcome.sequence),
        "taskRevision": outcome.task_revision,
        "taskUuid": outcome.task_uuid,
        "paths": paths,
        "reason": reason,
        "attemptCost": 0,
        "claim": claim,
    })
}

fn project_campaign_status_outcomes(
    status: &mut Value,
    campaign: &str,
    records: &[LocalAttemptReceiptV1],
    current_revisions: &BTreeMap<String, String>,
    current_epochs: &BTreeMap<String, String>,
) -> Result<()> {
    let active = active_worker_outcomes_from_receipts(records, current_revisions, current_epochs);
    let tasks = status
        .get_mut("tasks")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| invalid("daemon returned an invalid campaign task table"))?;
    let mut projected = Vec::new();
    for task in tasks {
        if task.get("status").and_then(Value::as_str) == Some("done") {
            continue;
        }
        let Some(task_id) = task
            .get("taskRef")
            .and_then(Value::as_str)
            .and_then(|task_ref| {
                task_ref
                    .rsplit_once('/')
                    .map(|(_, task_id)| task_id.to_owned())
            })
        else {
            continue;
        };
        let Some(outcome) = active.get(&task_id) else {
            continue;
        };
        let value = worker_outcome_status_value(campaign, outcome);
        task.as_object_mut()
            .ok_or_else(|| invalid("daemon returned a non-object campaign task"))?
            .insert("outcome".to_owned(), value.clone());
        projected.push(json!({"taskId": task_id, "outcome": value}));
    }
    status
        .as_object_mut()
        .ok_or_else(|| invalid("daemon returned a non-object campaign status"))?
        .insert("outcomes".to_owned(), Value::Array(projected));
    Ok(())
}

fn attach_campaign_status_outcomes(
    state_dir: &Path,
    graph: Option<&CanonicalCampaignGraphV1>,
    registration: Option<&CampaignRegistration>,
    status: &mut Value,
) -> Result<()> {
    let Some(graph) = graph else {
        return Ok(());
    };
    let campaign = &graph.manifest.name;
    let current_revisions = graph_completion_revisions(graph)?;
    let current_epochs = if let Some(registration) = registration {
        let task_input_hashes = canonical_task_input_hashes(graph)?;
        let steering = read_existing_local_steering(state_dir, registration)?;
        current_task_input_epochs(&task_input_hashes, &steering)?
    } else {
        BTreeMap::new()
    };
    let records = read_local_attempt_receipts(state_dir, campaign, LOCAL_CAMPAIGN_ISSUE_NUMBER)?;
    project_campaign_status_outcomes(
        status,
        campaign,
        &records,
        &current_revisions,
        &current_epochs,
    )
}

/// Attach the identity's lease to a status projection.
///
/// The status view renders the reconciled past; the lease is the one durable
/// fact in it that speaks to the present and to the end. A reader asking
/// whether a campaign is still going, or finished and on which revision, now
/// has an answer in the same object instead of a unit listing beside it.
fn attach_campaign_lease(
    state_dir: &Path,
    registration: Option<&CampaignRegistration>,
    status: &mut Value,
) -> Result<()> {
    let Some(registration) = registration else {
        return Ok(());
    };
    let Some(record) = campaign_lease_store(state_dir, registration).read()? else {
        return Ok(());
    };
    status
        .as_object_mut()
        .ok_or_else(|| invalid("daemon returned a non-object campaign status"))?
        .insert("lease".to_owned(), serde_json::to_value(record)?);
    Ok(())
}

fn sha256_json(value: &Value) -> Result<String> {
    let canonical = tally_core::campaign_contract::canonical_json(value)?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
}

fn campaign_state_ref_prefix(campaign: &str, issue_number: u64) -> String {
    let scope = format!("{campaign}\0{issue_number}");
    let digest = format!("{:x}", Sha256::digest(scope.as_bytes()));
    format!("refs/tally/spec-build/v1/{}", &digest[..24])
}

/// Durable Git state that can advance without touching an issue or comment.
///
/// The driver treats the remote base plus its campaign-scoped hidden refs as
/// the source of truth for local merges, checkpoints, and continuation
/// receipts. Polls must read the same facts: otherwise a completed local pass
/// is indistinguishable from an idle campaign when its durable Git state moves.
fn repository_progress_value(graph: &CampaignGraph) -> Result<Value> {
    let repository = &graph.canonical.manifest.repository;
    let base_ref = format!("refs/heads/{}", repository.base_branch);
    let state_prefix =
        campaign_state_ref_prefix(&graph.canonical.manifest.name, LOCAL_CAMPAIGN_ISSUE_NUMBER);
    let state_pattern = format!("{state_prefix}/*");
    let listed = ProcessCommand::new("git")
        .arg("-C")
        .arg(&repository.checkout)
        .args([
            "ls-remote",
            "--refs",
            repository.remote.as_str(),
            base_ref.as_str(),
            state_pattern.as_str(),
        ])
        .output()
        .context("cannot query durable campaign repository state")?;
    if !listed.status.success() {
        bail!(
            "cannot query durable campaign repository state: {}",
            String::from_utf8_lossy(&listed.stderr).trim()
        );
    }
    let stdout =
        String::from_utf8(listed.stdout).context("git ls-remote output was not valid UTF-8")?;
    let mut base = None;
    let mut campaign_refs = BTreeMap::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let (target, name) = line
            .split_once('\t')
            .ok_or_else(|| invalid("campaign repository state contained a malformed ref"))?;
        if !((40..=64).contains(&target.len())
            && target.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(invalid(format!(
                "campaign repository ref {name} returned a malformed object ID"
            )));
        }
        if name == base_ref {
            if base.replace(target.to_owned()).is_some() {
                bail!("campaign repository returned the base ref more than once");
            }
        } else if name
            .strip_prefix(&state_prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
            && campaign_refs
                .insert(name.to_owned(), target.to_owned())
                .is_some()
        {
            bail!("campaign repository returned state ref {name} more than once");
        }
    }
    let base = base.ok_or_else(|| {
        invalid(format!(
            "campaign repository remote has no base ref {base_ref}"
        ))
    })?;
    Ok(json!({
        "base": {
            "ref": base_ref,
            "target": base,
        },
        "campaignRefs": campaign_refs,
    }))
}

fn campaign_observation(
    graph: &CampaignGraph,
    steering: &CampaignSteering,
    repository_progress: &Value,
    arm_serial: u64,
) -> Result<String> {
    sha256_json(&json!({
        "graph": graph.canonical.executable_digest,
        "repositoryProgress": repository_progress,
        "steering": steering.master,
        // A task-addressed local record must nudge the campaign exactly like a
        // campaign-wide record does.
        "taskSteering": steering.tasks,
        "armSerial": arm_serial,
    }))
}

/// What the identity's most recent pass is doing right now.
///
/// The lease is decided against this reading: a pass with nodes in flight
/// renews it, a pass at rest with dispatchable work arms the frontier, and a
/// pass at rest with nothing left to dispatch is the only place completion
/// can be decided at all.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CampaignPassLiveness {
    /// Work is flowing under this lease.
    Live { nodes: usize },
    /// At rest with dispatchable work; the flow run is the liveness arm.
    Dispatchable(String),
    /// At rest with nothing to dispatch.
    AtRest,
}

/// Arm an otherwise unchanged poll only when its latest pass is truly at rest
/// and the admitted graph still contains work that pass can dispatch.
///
/// A terminal failed node projects its task as `blocked` even after the first
/// (retryable) attempt. Dependency blockers and the local escalation ledger
/// are the scheduling authorities, so both `pending` and directly `blocked`
/// tasks remain dispatchable when `blockedBy` is empty and no active
/// escalation names them.
fn dispatchable_poll_liveness_arm(
    graph: &CampaignGraph,
    registration_id: &str,
    escalated: &BTreeSet<String>,
    status: &Value,
) -> Result<CampaignPassLiveness> {
    let state = status["state"]
        .as_str()
        .ok_or_else(|| invalid("daemon returned campaign status without a state"))?;
    let current_nodes = status["currentNodes"]
        .as_array()
        .ok_or_else(|| invalid("daemon returned campaign status without a current-node table"))?;
    let running = status["counts"]["running"]
        .as_u64()
        .ok_or_else(|| invalid("daemon returned campaign status without a running-task count"))?;
    // `currentNodes` excludes the flow root; the campaign state includes it,
    // so a running state with an empty table is still one live node.
    if state == "running" || running != 0 || !current_nodes.is_empty() {
        return Ok(CampaignPassLiveness::Live {
            nodes: usize::try_from(running)
                .unwrap_or(usize::MAX)
                .max(current_nodes.len())
                .max(1),
        });
    }
    let tasks = match status.get("tasks") {
        None => &[][..],
        Some(value) => value
            .as_array()
            .ok_or_else(|| invalid("daemon returned an invalid campaign task table"))?,
    };
    for task in &graph.canonical.manifest.tasks {
        if escalated.contains(&task.id) {
            continue;
        }
        let expected_ref = format!("{registration_id}/{}", task.id);
        let Some(projected) = tasks
            .iter()
            .find(|candidate| candidate["taskRef"].as_str() == Some(expected_ref.as_str()))
        else {
            continue;
        };
        let task_status = projected["status"]
            .as_str()
            .ok_or_else(|| invalid("daemon returned a campaign task without a status"))?;
        let blocked_by = projected["blockedBy"]
            .as_array()
            .ok_or_else(|| invalid("daemon returned a campaign task without blockedBy"))?;
        if matches!(task_status, "pending" | "blocked") && blocked_by.is_empty() {
            let flow_run_id = status["flowRunId"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    invalid("daemon returned dispatchable campaign work without a flow run")
                })?;
            return Ok(CampaignPassLiveness::Dispatchable(flow_run_id.to_owned()));
        }
    }
    Ok(CampaignPassLiveness::AtRest)
}

async fn campaign_poll_liveness_arm(
    host: CampaignHost<'_>,
    graph: &CampaignGraph,
    registration: &CampaignRegistration,
    observation: &str,
) -> Result<CampaignPassLiveness> {
    let params = json!({
        "issueUrl": campaign_issue_url(
            &registration.code_repository,
            &registration.worklist_pattern,
        ),
        "registrationId": &registration.registration_id,
        "latestObservation": observation,
    });
    // Select the unchanged observation explicitly. The status API treats a
    // registered selector without one as a newly armed, never-enqueued
    // campaign; within an observation it still resolves the newest pass, so
    // an enqueue that committed before its registry write is observed here.
    let client = connect_rpc(host.socket, host.config_path).await?;
    let status = client
        .call_with_deadline("__campaign.status", Some(params), host.rpc_timeout)
        .await?;
    let escalated = active_local_escalated_tasks(host.state_dir, graph, registration)?;
    dispatchable_poll_liveness_arm(graph, &registration.registration_id, &escalated, &status)
}

fn resolve_state_dir(value: Option<PathBuf>) -> Result<PathBuf> {
    let path = value.map_or_else(default_state_dir, Ok)?;
    if !path.is_absolute() {
        return Err(invalid("campaign state directory must be absolute"));
    }
    Ok(path)
}

fn campaign_repository_from_arm(args: &CampaignArmArgs) -> Result<CampaignRepository> {
    let requested = args
        .checkout
        .clone()
        .map_or_else(std::env::current_dir, Ok)
        .context("cannot resolve the current directory for campaign --checkout")?;
    let checkout = fs::canonicalize(&requested)
        .with_context(|| format!("cannot resolve campaign checkout {}", requested.display()))?;
    Ok(CampaignRepository {
        checkout,
        base_branch: args.base_branch.clone(),
        remote: args.remote.clone(),
        forge: "local".to_owned(),
    })
}

fn campaign_repository_from_registration(
    registration: &CampaignRegistration,
) -> CampaignRepository {
    CampaignRepository {
        checkout: registration.checkout.clone(),
        base_branch: registration.base_branch.clone(),
        remote: registration.remote.clone(),
        forge: "local".to_owned(),
    }
}

fn packaged_campaign_asset_from_executable(
    executable: &Path,
    relative: &Path,
    role: &str,
) -> Result<PathBuf> {
    let parent = executable.parent().ok_or_else(|| {
        invalid(format!(
            "cannot resolve packaged campaign {role}: tally executable {} has no parent",
            executable.display()
        ))
    })?;
    let probed = parent.join(relative);
    if !probed.is_file() {
        return Err(invalid(format!(
            "packaged campaign {role} is missing; probed {}",
            probed.display()
        )));
    }
    fs::canonicalize(&probed).with_context(|| {
        format!(
            "cannot resolve packaged campaign {role} at {}",
            probed.display()
        )
    })
}

fn resolve_campaign_asset(
    override_path: Option<PathBuf>,
    relative: &Path,
    role: &str,
) -> Result<PathBuf> {
    if let Some(path) = override_path {
        let resolved = fs::canonicalize(&path).with_context(|| {
            format!("cannot resolve campaign {role} override {}", path.display())
        })?;
        if !resolved.is_file() {
            return Err(invalid(format!(
                "campaign {role} override is not a regular file: {}",
                path.display()
            )));
        }
        return Ok(resolved);
    }
    let executable = std::env::current_exe().context("cannot resolve tally executable")?;
    packaged_campaign_asset_from_executable(&executable, relative, role)
}

fn worklist_default_campaign_name(source_path: &str) -> Result<String> {
    let name = Path::new(source_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| safe_component(stem))
        .ok_or_else(|| {
            invalid(format!(
                "campaign worklist path {source_path:?} has no safe UTF-8 file stem; set campaign.name"
            ))
        })?;
    Ok(name.to_owned())
}

fn parse_worklist_campaign_policy(
    document: &Value,
    source_path: &str,
) -> Result<WorklistCampaignPolicy> {
    let empty = json!({});
    let value = document.get("campaign").unwrap_or(&empty);
    if document.get("campaign").is_some() && !value.is_object() {
        return Err(invalid("worklist.campaign must be an object"));
    }
    let mut policy: WorklistCampaignPolicy = serde_json::from_value(value.clone())
        .map_err(|error| invalid(format!("worklist.campaign is invalid: {error}")))?;
    if value.get("name").is_some_and(Value::is_null) {
        return Err(invalid(
            "worklist.campaign.name must be a string when present",
        ));
    }
    if policy.name.is_none() {
        policy.name = Some(worklist_default_campaign_name(source_path)?);
    }
    if !policy.name.as_deref().is_some_and(safe_component) {
        return Err(invalid(
            "worklist.campaign.name must be a safe path component",
        ));
    }
    if !(1..=MAX_CAMPAIGN_TASKS).contains(&policy.max_tasks) {
        return Err(invalid(format!(
            "worklist.campaign.maxTasks must be in 1..={MAX_CAMPAIGN_TASKS}"
        )));
    }
    if !(1..=MAX_CAMPAIGN_TASKS).contains(&policy.max_parallel)
        || policy.max_parallel > policy.max_tasks
    {
        return Err(invalid(format!(
            "worklist.campaign.maxParallel must be in 1..={MAX_CAMPAIGN_TASKS} and not exceed maxTasks"
        )));
    }
    if !matches!(policy.merge_method.as_str(), "merge" | "squash") {
        return Err(invalid(
            "worklist.campaign.mergeMethod must be merge or squash",
        ));
    }
    if policy.driver_runtime_max_sec == 0 || policy.runtime_max_sec == Some(0) {
        return Err(invalid(
            "worklist.campaign runtime limits must be positive when present",
        ));
    }
    validate_agent(&policy.agent)
        .map_err(|error| invalid(format!("worklist.campaign.agent is invalid: {error}")))?;
    if policy.steward.is_none() && !policy.steward_argv.is_empty() {
        return Err(invalid(
            "worklist.campaign.stewardArgv requires a steward adapter",
        ));
    }
    if policy.steward_runtime_max_sec == 0 {
        return Err(invalid(
            "worklist.campaign.stewardRuntimeMaxSec must be positive",
        ));
    }
    if policy
        .steward_argv
        .iter()
        .any(|argument| argument.is_empty() || argument.chars().any(char::is_control))
    {
        return Err(invalid(
            "worklist.campaign.stewardArgv must contain non-empty strings without control characters",
        ));
    }
    // Shape only: ids, argv, and patterns are judged here, and a declared
    // budget of zero is still refused. A silent budget stands in as the
    // never-fired floor for the length of this check and is resolved for real
    // against receipts once the campaign name is known.
    let shaped = policy
        .gates
        .iter()
        .map(|gate| {
            gate.resolved(
                gate.declared_runtime_max_sec()
                    .unwrap_or(GATE_BUDGET_UNOBSERVED_SEC),
            )
        })
        .collect::<Vec<_>>();
    validate_gates(&shaped)
        .map_err(|error| invalid(format!("worklist.campaign.gates are invalid: {error}")))?;
    Ok(policy)
}

fn steward_uses_unsupported_job_configuration(adapter: &AdapterConfig) -> bool {
    adapter.launch.model.is_some()
        || adapter.launch.effort.is_some()
        || !adapter.launch.approval_policies.is_empty()
        || !adapter.launch.sandbox_policies.is_empty()
        || !adapter.hardening.is_none()
        || !adapter.extra_writable_paths.is_empty()
}

fn resolve_worklist_steward(
    policy: &WorklistCampaignPolicy,
    adapters: &BTreeMap<String, AdapterConfig>,
) -> Result<Option<CampaignSteward>> {
    let Some(name) = policy.steward.as_deref() else {
        return Ok(None);
    };
    if !safe_component(name) {
        return Err(invalid(
            "worklist.campaign.steward must be null or a safe adapter name",
        ));
    }
    let adapter = adapters.get(name).ok_or_else(|| {
        invalid(format!(
            "worklist campaign references unknown steward adapter {name:?}"
        ))
    })?;
    if steward_uses_unsupported_job_configuration(adapter) {
        return Err(invalid(format!(
            "worklist campaign steward adapter {name:?} declares launch policies, hardening, or extraWritablePaths, which the direct narration subprocess cannot apply"
        )));
    }
    let final_message_pattern = match adapter.scrape.get("finalMessage") {
        None => DEFAULT_STEWARD_FINAL_MESSAGE_PATTERN.to_owned(),
        Some(capture)
            if capture.stream == ScrapeStream::Stdout
                && capture.mode == ScrapeMode::Regex
                && !capture.pattern.is_empty() =>
        {
            capture.pattern.clone()
        }
        Some(_) => {
            return Err(invalid(format!(
                "worklist campaign steward adapter {name:?} must declare scrape.finalMessage as a non-empty stdout regex"
            )))
        }
    };
    let mut argv = adapter.argv.clone();
    argv.extend(policy.steward_argv.clone());
    Ok(Some(CampaignSteward {
        adapter: name.to_owned(),
        argv,
        env: adapter.env.clone(),
        final_message_pattern,
        runtime_max_sec: Some(policy.steward_runtime_max_sec),
    }))
}

fn manifest_config_from_worklist(
    state_dir: &Path,
    committed: &CommittedLocalWorklist,
    repository: &CampaignRepository,
    code_repository: &str,
    adapters: &BTreeMap<String, AdapterConfig>,
) -> Result<(Value, Vec<GateBudget>)> {
    let policy = parse_worklist_campaign_policy(&committed.document, &committed.source_path)?;
    let Some(adapter) = adapters.get(&policy.agent.adapter) else {
        return Err(invalid(format!(
            "worklist campaign references unknown agent adapter {:?}",
            policy.agent.adapter
        )));
    };
    let pool = format!("campaign/{code_repository}");
    debug_assert!(is_campaign_pool_name(&pool));
    let steward = resolve_worklist_steward(&policy, adapters)?;
    let name = policy
        .name
        .clone()
        .expect("campaign name was defaulted above");
    let (gates, gate_budgets) = resolve_worklist_gate_budgets(
        &policy.gates,
        &recorded_gate_observations(state_dir, &name)?,
    );
    let agent = resolve_worklist_agent_policies(policy.agent.clone(), adapter);
    Ok((
        worklist_policy_manifest_config(&name, &policy, repository, &pool, agent, steward, gates),
        gate_budgets,
    ))
}

/// Assemble the manifest configuration one parsed worklist policy states.
///
/// Everything this host has to answer for arrives already answered: the agent
/// and steward bound to the adapter catalog, the gates carrying budgets
/// derived from their own receipts. The scaffold rehearsal has neither a
/// catalog nor a receipt history and passes what it does have, so a template
/// cannot pass a rehearsal that admission would then refuse for a shape the
/// two assembled differently.
fn worklist_policy_manifest_config(
    name: &str,
    policy: &WorklistCampaignPolicy,
    repository: &CampaignRepository,
    pool: &str,
    agent: CampaignAgent,
    steward: Option<CampaignSteward>,
    gates: Vec<CampaignGate>,
) -> Value {
    json!({
        "schemaVersion": CAMPAIGN_SCHEMA_VERSION,
        "name": name,
        "repository": repository,
        "maxTasks": policy.max_tasks,
        "maxParallel": policy.max_parallel,
        "driverRuntimeMaxSec": policy.driver_runtime_max_sec,
        "runtimeMaxSec": policy.runtime_max_sec,
        "pool": pool,
        "mergeMethod": policy.merge_method,
        "agent": agent,
        "steward": steward,
        "gates": gates,
        "tasks": [],
    })
}

/// Bind the worklist's policy silence to the selected adapter's own answer.
///
/// A worklist is adapter-neutral bytes: the three agent policy names are keys
/// of some adapter's policy map and of no other's, so a worklist that names one
/// has chosen an adapter, and a worklist that names none has said nothing at
/// all. Silence used to be filled by campaign-contract constants holding one
/// preset's vocabulary, which every other adapter then refused at render. It is
/// filled here instead, from the adapter the campaign actually selected and
/// only from what that adapter declares about itself -- the same seam that
/// already resolves the steward catalog role against this host's catalog.
///
/// An explicitly written value wins outright. Null and absent are the same
/// statement, because a worklist has no way to distinguish them and neither
/// should: both mean "the adapter answers".
fn resolve_worklist_agent_policies(agent: CampaignAgent, adapter: &AdapterConfig) -> CampaignAgent {
    let approval_policy = adapter
        .resolved_approval_policy(agent.approval_policy.as_deref())
        .map(str::to_owned);
    let sandbox_policy = adapter
        .resolved_sandbox_policy(agent.sandbox_policy.as_deref())
        .map(str::to_owned);
    let diagnosis_sandbox_policy = adapter
        .resolved_diagnosis_sandbox_policy(agent.diagnosis_sandbox_policy.as_deref())
        .map(str::to_owned);
    CampaignAgent {
        approval_policy,
        sandbox_policy,
        diagnosis_sandbox_policy,
        ..agent
    }
}

fn committed_local_worklist(
    repository: &CampaignRepository,
    worklist_pattern: &str,
) -> Result<CommittedLocalWorklist> {
    let checkout = fs::canonicalize(&repository.checkout).with_context(|| {
        format!(
            "cannot resolve campaign worklist checkout {}",
            repository.checkout.display()
        )
    })?;
    let git = |arguments: &[&str], context: &str| -> Result<std::process::Output> {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(arguments)
            .output()
            .with_context(|| format!("cannot execute git while {context}"))?;
        if !output.status.success() {
            bail!(
                "cannot {context}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output)
    };
    git(
        &["fetch", "--prune", "--no-tags", &repository.remote],
        "fetching the local campaign worklist authority",
    )?;
    let base_ref = format!(
        "{}/{}^{{commit}}",
        repository.remote, repository.base_branch
    );
    let revision = String::from_utf8(
        git(
            &["rev-parse", "--verify", &base_ref],
            "resolving the local campaign worklist authority revision",
        )?
        .stdout,
    )
    .context("campaign worklist authority revision is not valid UTF-8")?
    .trim()
    .to_ascii_lowercase();
    let literal_prefix = worklist_pattern
        .split('/')
        .take_while(|component| !component.contains(['*', '?', '[']))
        .collect::<Vec<_>>()
        .join("/");
    let mut tree_arguments = vec!["ls-tree", "-r", "-z", "--full-tree", &revision];
    if !literal_prefix.is_empty() {
        tree_arguments.extend(["--", &literal_prefix]);
    }
    let tree = git(
        &tree_arguments,
        "resolving the local campaign worklist pattern",
    )?;
    let pattern = CString::new(worklist_pattern)
        .map_err(|_| invalid("campaign worklist pattern contains a NUL byte"))?;
    let mut matches = Vec::new();
    for entry in tree.stdout.split(|byte| *byte == b'\0') {
        if entry.is_empty() {
            continue;
        }
        let tab = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| invalid("remote base tree contains a malformed worklist candidate"))?;
        let metadata = std::str::from_utf8(&entry[..tab])
            .context("remote base tree worklist metadata is not valid ASCII")?;
        let mut fields = metadata.split(' ');
        let (Some(mode), Some(object_type), Some(object_id), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(invalid(
                "remote base tree contains malformed worklist metadata",
            ));
        };
        let path = std::str::from_utf8(&entry[tab + 1..])
            .context("remote base tree worklist path is not valid UTF-8")?;
        let candidate = CString::new(path)
            .map_err(|_| invalid("remote base tree worklist path contains a NUL byte"))?;
        // SAFETY: both pointers come from live `CString`s, and `fnmatch` only
        // reads their NUL-terminated bytes for the duration of this call.
        let matched = unsafe {
            libc::fnmatch(
                pattern.as_ptr(),
                candidate.as_ptr(),
                libc::FNM_PATHNAME | libc::FNM_PERIOD,
            )
        } == 0;
        if !matched {
            continue;
        }
        if object_type == "blob" && matches!(mode, "100644" | "100755") {
            matches.push((path.to_owned(), object_id.to_owned()));
        }
    }
    let [(source_path, source_object)] = matches.as_slice() else {
        bail!(
            "campaign worklist pattern {worklist_pattern:?} matched {} regular files at fetched base revision {revision}; expected exactly one",
            matches.len()
        );
    };
    let raw = git(
        &["cat-file", "blob", source_object],
        "reading the committed local campaign worklist",
    )?
    .stdout;
    let document = serde_json::from_slice(&raw).with_context(|| {
        format!(
            "campaign worklist {source_path:?} at fetched base revision {revision} is invalid JSON"
        )
    })?;
    Ok(CommittedLocalWorklist {
        document,
        source_path: source_path.clone(),
        source_sha256: format!("sha256:{:x}", Sha256::digest(&raw)),
    })
}

fn local_campaign_graph_from_worklist(
    state_dir: &Path,
    repository: CampaignRepository,
    code_repository: &str,
    worklist_pattern: &str,
    adapters: &BTreeMap<String, AdapterConfig>,
) -> Result<CampaignGraph> {
    let committed = committed_local_worklist(&repository, worklist_pattern)?;
    let (manifest_config, gate_budgets) = manifest_config_from_worklist(
        state_dir,
        &committed,
        &repository,
        code_repository,
        adapters,
    )?;
    let validated = validate_local_worklist_document(&committed.document, &manifest_config)?;
    if validated.manifest.repository.forge != "local" {
        return Err(invalid(
            "the local worklist arm path requires campaign.repository.forge=local",
        ));
    }
    local_campaign_graph(validated, committed.source_sha256, gate_budgets)
}

fn local_campaign_graph(
    validated: ValidatedWorklist,
    worklist_sha256: String,
    gate_budgets: Vec<GateBudget>,
) -> Result<CampaignGraph> {
    if validated.tasks.len() != validated.manifest.tasks.len() {
        return Err(invalid(
            "validated worklist task content does not match its manifest references",
        ));
    }
    let ownership_preflight_warnings = ownership_preflight_warnings(&validated.tasks);
    let task_content = validated
        .tasks
        .iter()
        .zip(&validated.manifest.tasks)
        .map(|(task, reference)| CanonicalCampaignTaskV1 {
            number: reference.issue,
            title: task.title.clone(),
            body: task.body.clone(),
        })
        .collect::<Vec<_>>();
    let canonical = CanonicalCampaignGraphV1::new(validated.manifest, task_content)?;
    let task_input_hashes = canonical_task_input_hashes(&canonical)?;
    let task_completion_revisions = graph_completion_revisions(&canonical)?;
    Ok(CampaignGraph {
        canonical,
        ownership_preflight_warnings,
        worklist_sha256,
        task_input_hashes,
        task_completion_revisions,
        gate_budgets,
    })
}

fn canonical_task_input_hashes(
    graph: &CanonicalCampaignGraphV1,
) -> Result<BTreeMap<String, String>> {
    graph
        .manifest
        .tasks
        .iter()
        .zip(&graph.tasks)
        .map(|(reference, content)| {
            Ok((
                reference.id.clone(),
                task_input_hash(&graph.manifest, reference, content)?,
            ))
        })
        .collect()
}

fn approved_graph_directory(state_dir: &Path, registration_id: &str) -> PathBuf {
    let scope = format!("{:x}", Sha256::digest(registration_id.as_bytes()));
    state_dir
        .join("campaigns/approved-graphs")
        .join(&scope[..32])
}

fn approved_graph_path(state_dir: &Path, registration: &CampaignRegistration) -> PathBuf {
    graph_snapshot_path(
        state_dir,
        &registration.registration_id,
        registration.arm_serial,
    )
}

fn graph_snapshot_path(state_dir: &Path, registration_id: &str, arm_serial: u64) -> PathBuf {
    approved_graph_directory(state_dir, registration_id).join(format!("{arm_serial}.graph-v1.json"))
}

fn validated_graph_snapshot(
    snapshot: ApprovedGraphSnapshotV1,
    registration: &CampaignRegistration,
    arm_serial: u64,
    path: &Path,
) -> Result<CanonicalCampaignGraphV1> {
    if snapshot.schema_version != APPROVED_GRAPH_SNAPSHOT_SCHEMA_VERSION
        || snapshot.registration_id != registration.registration_id
        || snapshot.arm_serial != arm_serial
    {
        bail!(
            "campaign approved-graph snapshot {} disagrees with registration {} arm {arm_serial}",
            path.display(),
            registration.registration_id,
        );
    }
    let rebuilt = CanonicalCampaignGraphV1::new(
        snapshot.graph.manifest.clone(),
        snapshot.graph.tasks.clone(),
    )?;
    if rebuilt != snapshot.graph {
        bail!(
            "campaign approved-graph snapshot {} fails canonical digest verification",
            path.display()
        );
    }
    Ok(snapshot.graph)
}

fn read_approved_graph_snapshot(
    state_dir: &Path,
    registration: &CampaignRegistration,
) -> Result<Option<CanonicalCampaignGraphV1>> {
    let graph = read_graph_snapshot(state_dir, registration, registration.arm_serial)?;
    if let Some(graph) = &graph {
        if graph.executable_digest != registration.approved_graph_digest {
            bail!(
                "campaign approved-graph snapshot for registration {} arm {} disagrees with its admitted digest",
                registration.registration_id,
                registration.arm_serial
            );
        }
    }
    Ok(graph)
}

/// The graph one epoch back, kept on disk so a straddling attempt's own
/// admitted digest survives the re-admission that superseded it.
///
/// Returns `None` when there is no earlier epoch, or when its snapshot has
/// already aged out — the refusal then names the digest without the arm.
fn read_superseded_graph_snapshot(
    state_dir: &Path,
    registration: &CampaignRegistration,
) -> Result<Option<(u64, CanonicalCampaignGraphV1)>> {
    let Some(arm_serial) = registration
        .arm_serial
        .checked_sub(1)
        .filter(|serial| *serial > 0)
    else {
        return Ok(None);
    };
    Ok(read_graph_snapshot(state_dir, registration, arm_serial)?.map(|graph| (arm_serial, graph)))
}

/// Tell a straddle as a fact about two epochs.
///
/// The prepared arm is recovered from the retained snapshot when the refused
/// subject turns out to hold exactly the epoch this campaign superseded,
/// which is the shape a mid-flight attempt has. A snapshot read that fails
/// must not swallow the refusal it was only decorating, so any error here
/// simply leaves the arm unnamed.
fn campaign_digest_mismatch(
    state_dir: &Path,
    registration: &CampaignRegistration,
    prepared_graph_digest: &str,
    subject: String,
) -> CampaignDigestMismatch {
    let prepared_arm_serial = read_superseded_graph_snapshot(state_dir, registration)
        .ok()
        .flatten()
        .filter(|(_, graph)| graph.executable_digest == prepared_graph_digest)
        .map(|(arm_serial, _)| arm_serial);
    CampaignDigestMismatch {
        subject,
        admitted_graph_digest: registration.approved_graph_digest.clone(),
        admitted_arm_serial: registration.arm_serial,
        prepared_graph_digest: prepared_graph_digest.to_owned(),
        prepared_arm_serial,
    }
}

fn read_graph_snapshot(
    state_dir: &Path,
    registration: &CampaignRegistration,
    arm_serial: u64,
) -> Result<Option<CanonicalCampaignGraphV1>> {
    let path = graph_snapshot_path(state_dir, &registration.registration_id, arm_serial);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("cannot inspect campaign approved graph {}", path.display())
            })
        }
    };
    if !metadata.is_file() || metadata.len() > MAX_APPROVED_GRAPH_SNAPSHOT_BYTES {
        bail!(
            "campaign approved-graph snapshot {} is not a bounded regular file",
            path.display()
        );
    }
    let bytes = fs::read(&path)
        .with_context(|| format!("cannot read campaign approved graph {}", path.display()))?;
    let snapshot: ApprovedGraphSnapshotV1 = serde_json::from_slice(&bytes)
        .with_context(|| format!("campaign approved graph {} is invalid", path.display()))?;
    validated_graph_snapshot(snapshot, registration, arm_serial, &path).map(Some)
}

fn write_approved_graph_snapshot(
    state_dir: &Path,
    registration: &CampaignRegistration,
    graph: &CanonicalCampaignGraphV1,
) -> Result<()> {
    if graph.executable_digest != registration.approved_graph_digest {
        bail!("cannot snapshot a campaign graph that disagrees with arm authority");
    }
    let directory = approved_graph_directory(state_dir, &registration.registration_id);
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "cannot create campaign approved-graph directory {}",
            directory.display()
        )
    })?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "cannot secure campaign approved-graph directory {}",
            directory.display()
        )
    })?;
    let path = approved_graph_path(state_dir, registration);
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        registration.arm_serial,
        uuid::Uuid::now_v7()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| {
            format!(
                "cannot create campaign approved-graph snapshot {}",
                temporary.display()
            )
        })?;
    let snapshot = ApprovedGraphSnapshotV1 {
        schema_version: APPROVED_GRAPH_SNAPSHOT_SCHEMA_VERSION,
        registration_id: registration.registration_id.clone(),
        arm_serial: registration.arm_serial,
        graph: graph.clone(),
    };
    serde_json::to_writer(&mut file, &snapshot)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, &path).with_context(|| {
        format!(
            "cannot publish campaign approved-graph snapshot {}",
            path.display()
        )
    })?;
    fs::File::open(&directory)?.sync_all()?;
    Ok(())
}

/// Keep the admitted epoch's snapshot and the single epoch it superseded.
///
/// A push can re-admit the worklist while an attempt is in flight. Deleting
/// the graph that attempt was prepared under is precisely what made the
/// recorded orphan illegible: nothing left on disk could still name the
/// digest the attempt owned, so the failure was told as a missing commit.
/// Retention is one generation deep, which is all a straddle spans.
fn prune_approved_graph_snapshots(
    state_dir: &Path,
    registration: &CampaignRegistration,
) -> Result<()> {
    let directory = approved_graph_directory(state_dir, &registration.registration_id);
    let retained = [
        Some(approved_graph_path(state_dir, registration)),
        registration
            .arm_serial
            .checked_sub(1)
            .filter(|serial| *serial > 0)
            .map(|serial| graph_snapshot_path(state_dir, &registration.registration_id, serial)),
    ];
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if !retained.iter().flatten().any(|keep| *keep == path) && path.is_file() {
            fs::remove_file(&path).with_context(|| {
                format!(
                    "cannot prune obsolete campaign approved graph {}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn remove_approved_graph_snapshots(state_dir: &Path, registration_id: &str) -> Result<()> {
    let directory = approved_graph_directory(state_dir, registration_id);
    match fs::remove_dir_all(&directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "cannot remove campaign approved graphs {}",
                directory.display()
            )
        }),
    }
}

/// Arming is the last moment before a campaign spends real agent time, so a
/// policy pairing the adapter cannot honour is refused here rather than three
/// seconds into the first implementation node.
fn validate_agent_policies(agent: &CampaignAgent, adapter: &AdapterConfig) -> Result<()> {
    if let Some(policy) = &agent.approval_policy {
        if !adapter.launch.approval_policies.contains_key(policy) {
            return Err(invalid(format!(
                "campaign agent approvalPolicy {policy:?} is not authorized by adapter {:?}",
                agent.adapter
            )));
        }
    }
    if let Some(policy) = &agent.sandbox_policy {
        if !adapter.launch.sandbox_policies.contains_key(policy) {
            return Err(invalid(format!(
                "campaign agent sandboxPolicy {policy:?} is not authorized by adapter {:?}",
                agent.adapter
            )));
        }
    }
    if let Some(policy) = &agent.diagnosis_sandbox_policy {
        if !adapter.launch.sandbox_policies.contains_key(policy) {
            return Err(invalid(format!(
                "campaign agent diagnosisSandboxPolicy {policy:?} is not authorized by adapter {:?}",
                agent.adapter
            )));
        }
    }
    // The implementation node's whole obligation is a commit. When the adapter
    // has said which of its sandbox policies reach git metadata, that pairing is
    // knowable before any agent time is spent.
    //
    // The manifest's policy is read as written. A worklist campaign has already
    // had its silence bound to this adapter's own declaration by
    // `resolve_worklist_agent_policies`, so what arrives here is the policy the
    // node will actually launch under -- and a manifest that arrives with none
    // will launch with none, which is exactly the pairing to refuse.
    if !adapter
        .launch
        .permits_commit(agent.sandbox_policy.as_deref())
    {
        return Err(invalid(format!(
            "campaign agent sandboxPolicy {:?} cannot create a commit under adapter {:?}; choose one of: {}",
            agent.sandbox_policy.as_deref().unwrap_or("<adapter default>"),
            agent.adapter,
            adapter.launch.commit_capable_names()
        )));
    }
    Ok(())
}

fn worker_findings_warning(agent: &CampaignAgent, adapter: &AdapterConfig) -> Option<String> {
    (!adapter.scrape.contains_key("finalMessage")).then(|| {
        format!(
            "campaign agent adapter {:?} declares no scrape.finalMessage; worker findings will not be retained",
            agent.adapter
        )
    })
}

fn trim_path_location_suffix(mut token: &str) -> &str {
    loop {
        let Some((path, suffix)) = token.rsplit_once(':') else {
            return token;
        };
        if suffix.is_empty()
            || !suffix
                .chars()
                .all(|character| character.is_ascii_digit() || matches!(character, '-' | '~' | '–'))
        {
            return token;
        }
        token = path;
    }
}

fn normalized_path_shaped_token(token: &str) -> Option<String> {
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            '`' | '\''
                | '"'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | ','
                | ';'
                | '!'
                | '?'
                | ':'
        )
    });
    let token = token.trim_end_matches(['.', '—']);
    let token = trim_path_location_suffix(token);
    let token = token.rfind("#L").map_or(token, |index| {
        let suffix = &token[index + 2..];
        if !suffix.is_empty()
            && suffix
                .chars()
                .all(|character| character.is_ascii_digit() || matches!(character, '-' | 'L'))
        {
            &token[..index]
        } else {
            token
        }
    });
    let token = token.trim_start_matches("./");

    if token.is_empty()
        || token.contains("://")
        || token.contains("//")
        || token == "."
        || token.chars().any(char::is_control)
    {
        return None;
    }
    let path = Path::new(token);
    let has_slash = token.contains('/');
    let has_extension = path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        name.rsplit_once('.').is_some_and(|(stem, extension)| {
            !stem.is_empty()
                && !extension.is_empty()
                && extension
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
                && extension
                    .chars()
                    .any(|character| character.is_ascii_alphabetic())
        })
    });
    (has_slash || has_extension).then(|| token.to_owned())
}

fn path_shaped_tokens(text: &str) -> BTreeSet<String> {
    text.split(|character: char| {
        character.is_whitespace()
            || matches!(
                character,
                '`' | '\''
                    | '"'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '<'
                    | '>'
                    | ','
                    | ';'
                    | '|'
                    | '&'
                    | '='
            )
    })
    .filter_map(normalized_path_shaped_token)
    .collect()
}

fn path_is_inside_conflict_domain(path: &str, domain: &str) -> bool {
    !Path::new(path).is_absolute()
        && !Path::new(path).components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
        && (path == domain
            || path
                .strip_prefix(domain)
                .is_some_and(|suffix| suffix.starts_with('/')))
}

fn ownership_preflight_warnings(tasks: &[WorklistTask]) -> Vec<String> {
    let mut warnings = Vec::new();
    for task in tasks {
        if task.kind != "implementation" {
            continue;
        }
        let Some(domains) = task.conflict_domains.as_deref() else {
            // An omitted serial-task boundary is inferred after execution. It
            // gives this textual preflight no declared allowlist to compare.
            continue;
        };
        let mut outside = BTreeMap::<String, BTreeSet<String>>::new();
        for input in &task.ownership_lint_inputs {
            for path in path_shaped_tokens(&input.text) {
                if !domains
                    .iter()
                    .any(|domain| path_is_inside_conflict_domain(&path, domain))
                {
                    outside
                        .entry(path)
                        .or_default()
                        .insert(input.context.clone());
                }
            }
        }
        for (path, contexts) in outside {
            warnings.push(format!(
                "implementation task {:?} names path-shaped token {path:?} in {} outside declared conflictDomains {:?}; arming continues",
                task.id,
                contexts.into_iter().collect::<Vec<_>>().join(" and "),
                domains,
            ));
        }
    }
    warnings
}

const CACHE_USING_TOOLS: [&str; 6] = ["nix", "go", "cargo", "npm", "pip", "uv"];
const COMMON_CACHE_REDIRECTS: [&str; 2] = ["XDG_CACHE_HOME", "XDG_STATE_HOME"];
const CACHE_REDIRECTS: [&str; 9] = [
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "GOCACHE",
    "GOMODCACHE",
    "CARGO_HOME",
    "NPM_CONFIG_CACHE",
    "npm_config_cache",
    "PIP_CACHE_DIR",
    "UV_CACHE_DIR",
];

fn argv_mentions_command(argv: &[String], command: &str) -> bool {
    argv.iter().any(|argument| {
        argument
            .split(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/'))
            })
            .filter(|token| !token.is_empty())
            .any(|token| token.rsplit('/').next() == Some(command))
    })
}

fn argv_invokes_cache_using_tool(argv: &[String], tool: &str) -> bool {
    if tool != "nix" {
        return argv_mentions_command(argv, tool);
    }

    const EVALUATING_NIX_SUBCOMMANDS: [&str; 4] = ["develop", "build", "shell", "run"];
    let joined = argv.join(" ");
    joined.split([';', '&', '|']).any(|command| {
        let tokens = command
            .split(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/'))
            })
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        tokens.windows(2).any(|pair| {
            pair[0].rsplit('/').next() == Some("nix")
                && EVALUATING_NIX_SUBCOMMANDS.contains(&pair[1])
        })
    })
}

fn has_nonempty_assignment(argument: &str, name: &str) -> bool {
    let assignment = format!("{name}=");
    argument.match_indices(&assignment).any(|(index, _)| {
        let boundary = argument[..index]
            .chars()
            .next_back()
            .is_none_or(|character| !(character.is_ascii_alphanumeric() || character == '_'));
        let value = argument[index + assignment.len()..].trim_start_matches(['\'', '"']);
        boundary
            && value.chars().next().is_some_and(|character| {
                !character.is_ascii_whitespace() && !matches!(character, ';' | '&' | '|')
            })
    })
}

fn argv_has_assignment(argv: &[String], names: &[&str]) -> bool {
    names.iter().any(|name| {
        argv.iter()
            .any(|argument| has_nonempty_assignment(argument, name))
    })
}

fn tool_cache_redirects(tool: &str) -> &'static [&'static str] {
    match tool {
        "go" => &["GOCACHE", "GOMODCACHE"],
        "cargo" => &["CARGO_HOME"],
        "npm" => &["NPM_CONFIG_CACHE", "npm_config_cache"],
        "pip" => &["PIP_CACHE_DIR"],
        "uv" => &["UV_CACHE_DIR"],
        _ => &[],
    }
}

fn has_cache_redirect(argv: &[String], tool: &str) -> bool {
    argv_has_assignment(argv, &COMMON_CACHE_REDIRECTS)
        || argv_has_assignment(argv, tool_cache_redirects(tool))
}

fn tmp_reference_is_cache_assignment(argument: &str, tmp_index: usize) -> bool {
    CACHE_REDIRECTS.iter().any(|name| {
        let assignment = format!("{name}=");
        argument[..tmp_index]
            .rmatch_indices(&assignment)
            .next()
            .is_some_and(|(index, _)| {
                let boundary = argument[..index]
                    .chars()
                    .next_back()
                    .is_none_or(|character| {
                        !(character.is_ascii_alphanumeric() || character == '_')
                    });
                let value_prefix = &argument[index + assignment.len()..tmp_index];
                boundary
                    && value_prefix
                        .chars()
                        .all(|character| matches!(character, '\'' | '"'))
            })
    })
}

fn argument_tmp_references(argument: &str) -> Vec<(usize, String)> {
    argument
        .match_indices("/tmp")
        .filter_map(|(index, _)| {
            let before = argument[..index].chars().next_back();
            let after = argument[index + "/tmp".len()..].chars().next();
            let starts_path = before.is_none_or(|character| {
                !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/'))
            });
            let ends_root = after.is_none_or(|character| {
                character == '/'
                    || character.is_ascii_whitespace()
                    || matches!(character, '\'' | '"' | ';' | ':' | ',' | ')')
            });
            if !starts_path || !ends_root || tmp_reference_is_cache_assignment(argument, index) {
                return None;
            }
            let end = argument[index..]
                .find(|character: char| {
                    character.is_ascii_whitespace()
                        || matches!(character, '\'' | '"' | ';' | ':' | ',' | ')' | '&' | '|')
                })
                .map_or(argument.len(), |offset| index + offset);
            Some((index, argument[index..end].to_owned()))
        })
        .collect()
}

fn tmp_path_was_created(created: &[String], path: &str) -> bool {
    created.iter().any(|directory| {
        path == directory
            || (directory != "/tmp"
                && path
                    .strip_prefix(directory)
                    .is_some_and(|suffix| suffix.starts_with('/')))
    })
}

fn argv_has_staged_tmp_reference(argv: &[String]) -> bool {
    let joined = argv.join(" ");
    let mut created = Vec::new();
    for command in joined.split([';', '&', '|']) {
        let references = argument_tmp_references(command);
        let creates_directories = argv_mentions_command(&[command.to_owned()], "mkdir")
            && command
                .split_ascii_whitespace()
                .any(|token| matches!(token.trim_matches(['\'', '"']), "-p" | "--parents"));
        for (_, path) in references {
            if creates_directories {
                created.push(path);
            } else if !tmp_path_was_created(&created, &path) {
                return true;
            }
        }
    }
    false
}

fn argument_has_home_reference(argument: &str) -> bool {
    argument.match_indices("$HOME").any(|(index, _)| {
        argument[index + "$HOME".len()..]
            .chars()
            .next()
            .is_none_or(|character| !(character.is_ascii_alphanumeric() || character == '_'))
    }) || argument.contains("${HOME}")
}

fn argv_appears_to_write_home(argv: &[String]) -> bool {
    if !argv
        .iter()
        .any(|argument| argument_has_home_reference(argument))
    {
        return false;
    }
    const WRITE_COMMANDS: [&str; 11] = [
        "chmod", "chown", "cp", "install", "ln", "mkdir", "mv", "tee", "touch", "truncate",
        "unlink",
    ];
    const WRITE_OPTIONS: [&str; 8] = [
        "-o",
        "--cache-dir",
        "--destination",
        "--out-dir",
        "--output",
        "--prefix",
        "--root",
        "--target-dir",
    ];
    argv.iter().any(|argument| argument.contains('>'))
        || WRITE_COMMANDS
            .iter()
            .any(|command| argv_mentions_command(argv, command))
        || argv.windows(2).any(|pair| {
            WRITE_OPTIONS.contains(&pair[0].as_str()) && argument_has_home_reference(&pair[1])
        })
        || argv.iter().any(|argument| {
            argument_has_home_reference(argument)
                && WRITE_OPTIONS
                    .iter()
                    .any(|option| argument.starts_with(&format!("{option}=")))
        })
}

fn argv_hazard_warnings(manifest: &CampaignManifest, hardening: AdapterHardening) -> Vec<String> {
    if hardening.is_none() {
        return Vec::new();
    }

    let mut warnings = Vec::new();
    let mut scan = |context: String, argv: &[String]| {
        let unredirected_tools = CACHE_USING_TOOLS
            .iter()
            .copied()
            .filter(|tool| {
                argv_invokes_cache_using_tool(argv, tool) && !has_cache_redirect(argv, tool)
            })
            .collect::<Vec<_>>();
        if !unredirected_tools.is_empty() {
            warnings.push(format!(
                "{context} invokes {} without an in-argv cache/state redirect (XDG_CACHE_HOME, XDG_STATE_HOME, or a tool-specific equivalent such as GOCACHE); it may fail under the resolved adapter's hardened tier",
                unredirected_tools.join(", ")
            ));
        }
        if argv_has_staged_tmp_reference(argv) {
            warnings.push(format!(
                "{context} references a /tmp path; PrivateTmp hides paths staged outside the transient unit"
            ));
        }
        if argv_appears_to_write_home(argv) {
            warnings.push(format!(
                "{context} appears to write through $HOME; ProtectHome=read-only can reject that write"
            ));
        }
    };

    for task in &manifest.tasks {
        if task.kind == "checkpoint" {
            if let Some(argv) = task.argv.as_deref() {
                scan(format!("checkpoint task {:?} argv", task.id), argv);
            }
        }
    }
    for gate in &manifest.gates {
        if let CampaignGate::Command {
            id,
            preflight_argv,
            argv,
            ..
        } = gate
        {
            scan(
                format!("campaign gate {id:?} preflightArgv"),
                preflight_argv,
            );
            scan(format!("campaign gate {id:?} argv"), argv);
        }
    }
    warnings
}

fn validate_host(
    graph: &CampaignGraph,
    config_path: Option<&Path>,
    flow: &Path,
    driver: &Path,
) -> Result<Vec<String>> {
    let manifest = &graph.canonical.manifest;
    let config = load_client_config(config_path)?;
    let required_nodes = max_flow_nodes(manifest);
    if config.enqueue.fanout_cap < required_nodes {
        return Err(invalid(format!(
            "campaign pass requires enqueue.fanoutCap >= {required_nodes}; host has {}",
            config.enqueue.fanout_cap
        )));
    }
    for pool in ["flow", "campaign-agent", "campaign-control"] {
        if !config.pools.contains_key(pool) {
            return Err(invalid(format!(
                "campaigns require configured pool {pool:?}"
            )));
        }
    }
    validate_campaign_runner_pool(&manifest.pool, &config.pools)?;
    let mut required_adapters = vec![
        "shell",
        "spec-build-driver",
        manifest.agent.adapter.as_str(),
    ];
    // The steward is bound as a catalog role, so arming refuses a campaign
    // whose narrator names an adapter this host does not configure rather than
    // degrading every publication to the template at run time.
    if let Some(steward) = &manifest.steward {
        required_adapters.push(steward.adapter.as_str());
    }
    for adapter in required_adapters {
        if !config.adapters.contains_key(adapter) {
            return Err(invalid(format!(
                "campaigns require configured adapter {adapter:?}"
            )));
        }
    }
    let agent_adapter = &config.adapters[&manifest.agent.adapter];
    validate_agent_policies(&manifest.agent, agent_adapter)?;
    if !flow.is_file() || !driver.is_file() {
        return Err(invalid(
            "campaign flow and driver assets must be regular files",
        ));
    }
    let checkout = &manifest.repository.checkout;
    if !checkout.is_dir() {
        return Err(invalid(format!(
            "campaign repository checkout does not exist: {}",
            checkout.display()
        )));
    }
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "--git-dir"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("cannot execute git while validating campaign checkout")?;
    if !status.success() {
        return Err(invalid(format!(
            "campaign repository checkout is not a Git worktree: {}",
            checkout.display()
        )));
    }
    if manifest.repository.forge != "local" {
        return Err(invalid("campaign repository.forge must equal local"));
    }
    // Checkpoint and command-gate argvs run through the flow `sh` native, so
    // their hazards depend on the resolved shell adapter rather than the agent.
    let mut warnings = argv_hazard_warnings(manifest, config.adapters["shell"].hardening);
    if let Some(warning) = worker_findings_warning(&manifest.agent, agent_adapter) {
        warnings.push(warning);
    }
    Ok(warnings)
}

fn validate_campaign_runner_pool(pool: &str, pools: &BTreeMap<String, PoolConfig>) -> Result<()> {
    if pool.starts_with(CAMPAIGN_POOL_PREFIX) {
        if is_campaign_pool_name(pool) {
            return Ok(());
        }
        return Err(invalid(
            "campaign namespace pool must use campaign/OWNER/REPO form",
        ));
    }
    let runner = pools
        .get(pool)
        .ok_or_else(|| invalid(format!("campaigns require configured pool {pool:?}")))?;
    if runner.resource() != ResourceKind::Mutex || runner.capacity != 1 {
        return Err(invalid(format!(
            "campaign runner pool {pool:?} must be a capacity-1 mutex"
        )));
    }
    Ok(())
}

fn priority(value: &str) -> Priority {
    match value {
        "interrupt" => Priority::Interrupt,
        "high" => Priority::High,
        "medium" => Priority::Medium,
        _ => Priority::Low,
    }
}

fn max_flow_nodes(manifest: &CampaignManifest) -> u32 {
    let command_gates = manifest
        .gates
        .iter()
        .filter(|gate| gate.is_command())
        .count();
    // Two nodes per command gate: the gating base-safe probe and the
    // non-gating witness that runs the gate's real merge-criterion argv on the
    // same pristine base. The witness decides nothing, but it is admitted and
    // therefore budgeted.
    let preflight = if command_gates == 0 {
        0
    } else {
        2 * command_gates + 2
    };
    // Sweep, reconcile, one possible continuation, and each worst-case
    // implementation lane: prep, steering re-check, agent, ownership, gates,
    // publish, rebase,
    // optional re-gates, merge, then the failure path's machinery retry, diff,
    // diagnosis, and steering, and finally cleanup. A lane that fails at merge
    // is the expensive one, not a lane that merges: maxNodes counts cumulative
    // rows, so finished nodes never return budget. Budgeting the success path
    // alone starves failure steering exactly when it is needed. A machinery
    // fault whose retry budget is already spent records the retry node and is
    // then steered, so both failure paths can land in one lane. Checkpoint
    // lanes are smaller.
    (3 + preflight + manifest.max_parallel * (12 + 2 * manifest.gates.len())) as u32
}

/// The `--projection-wait-ms` an arm may record (#432).
///
/// Refused at arm rather than at the first pass, because the value is durable:
/// a zero here would be written into the registration and then rejected by
/// every `tally flow run` this campaign ever dispatches, including the ones the
/// poll timer dispatches unattended. Absent stays absent, which is what leaves
/// the flow host's own 10 s default alone.
fn validated_projection_wait_ms(value: Option<u64>) -> Result<Option<u64>> {
    if value == Some(0) {
        return Err(invalid("--projection-wait-ms must be greater than zero"));
    }
    Ok(value)
}

/// Argv the pass writes into the events directory to admit its own successor.
///
/// It is the poll the timer already runs: one registry scan that reloads the
/// committed worklist graph, recomputes the observation revision, and dispatches through
/// `dispatch_campaign`, so the next pass inherits the `campaign:<repo>:<number>:<revision>`
/// dedup identity. A duplicate event, or a race with `tally-campaign-poll.timer`,
/// therefore collapses in the enqueue kernel instead of starting a second pass.
/// The host bindings every dispatch needs: where the daemon listens, which
/// configuration it was started from, and which registry the pass belongs to.
#[derive(Clone, Copy)]
struct CampaignHost<'a> {
    socket: &'a Path,
    config_path: Option<&'a Path>,
    state_dir: &'a Path,
    rpc_timeout: Duration,
}

impl CampaignHost<'_> {
    /// The one global CLI prefix inherited by every campaign child.
    ///
    /// Global flags have to precede `flow`/`campaign` for clap to parse the
    /// child exactly like the host that admitted it. In particular, a NixOS
    /// host keeps its only configuration at `/etc/tally/config.json`; allowing
    /// the initial flow to fall back to the service account's XDG home made it
    /// disagree with both the daemon and the continuation poll (#442).
    fn tally_argv_prefix(&self, executable: &Path) -> Vec<String> {
        let mut argv = vec![executable.display().to_string()];
        if let Some(config) = self.config_path {
            argv.push("--config".to_owned());
            argv.push(config.display().to_string());
        }
        argv.push("--socket".to_owned());
        argv.push(self.socket.display().to_string());
        argv
    }

    /// Argv of the `tally flow run` this campaign dispatches for one pass.
    ///
    /// This is the only place a durable registration turns into something the
    /// pass actually executes, and an argv nothing constructs in a test is an
    /// argv nothing notices the loss of (#432). `projection_wait_ms` travels on
    /// the argv rather than in the environment because the pass runs as a
    /// daemon-launched transient unit whose environment is an explicit
    /// `--setenv` list, so nothing an operator exports at arm time is visible to
    /// it. `None` adds no projection-wait elements: this vector is hashed into
    /// the enqueue payload, so an unconditional flag would move every existing
    /// campaign's payload identity.
    fn dispatch_flow_argv(
        &self,
        executable: &Path,
        flow: &Path,
        max_nodes: u32,
        projection_wait_ms: Option<u64>,
    ) -> Vec<String> {
        let mut argv = self.tally_argv_prefix(executable);
        argv.extend([
            "flow".to_owned(),
            "run".to_owned(),
            flow.display().to_string(),
            "--args-from-brief".to_owned(),
            "--max-nodes".to_owned(),
            max_nodes.to_string(),
        ]);
        if let Some(millis) = projection_wait_ms {
            argv.push("--result-projection-wait-ms".to_owned());
            argv.push(millis.to_string());
        }
        argv
    }

    fn continuation_argv(&self, executable: &Path) -> Vec<String> {
        let mut argv = self.tally_argv_prefix(executable);
        argv.extend([
            "campaign".to_owned(),
            "poll".to_owned(),
            "--once".to_owned(),
            "--state-dir".to_owned(),
            self.state_dir.display().to_string(),
        ]);
        argv
    }

    fn events_dir(&self) -> PathBuf {
        self.state_dir.join("events")
    }
}

fn campaign_dispatch_dedup_key(
    registration: &CampaignRegistration,
    revision: &str,
    liveness_arm: Option<&str>,
) -> String {
    match liveness_arm {
        Some(flow_run_id) => format!(
            "campaign:{}:{}:{}:liveness:{:x}",
            registration.code_repository,
            LOCAL_CAMPAIGN_ISSUE_NUMBER,
            revision,
            Sha256::digest(flow_run_id.as_bytes()),
        ),
        None => format!(
            "campaign:{}:{}:{}",
            registration.code_repository, LOCAL_CAMPAIGN_ISSUE_NUMBER, revision
        ),
    }
}

/// Enqueue one reconcile pass for the frontier this lease owns.
///
/// The lease is a parameter rather than a check because it is the whole
/// no-double-dispatch story: a caller that cannot produce a held
/// [`CampaignLeaseGuard`] cannot reach this code path, and the enqueue that
/// succeeds renews the lease it dispatched under. Activation and work are the
/// same fact, held together by the type.
async fn dispatch_campaign(
    host: CampaignHost<'_>,
    graph: &CampaignGraph,
    repository_progress: &Value,
    registration: &mut CampaignRegistration,
    wait: bool,
    liveness_arm: Option<&str>,
    lease: &mut CampaignLeaseGuard,
) -> Result<Value> {
    let CampaignHost {
        socket,
        config_path,
        rpc_timeout,
        ..
    } = host;
    let manifest = &graph.canonical.manifest;
    if graph.canonical.executable_digest != registration.approved_graph_digest {
        // A pass that reaches here was prepared under an epoch this identity
        // has since left behind — the straddle. Poll re-admits changed
        // worklists on its own now, so there is no verb to recommend; the
        // only useful thing to say is which two digests disagree.
        return Err(anyhow::Error::new(campaign_digest_mismatch(
            host.state_dir,
            registration,
            &graph.canonical.executable_digest,
            format!(
                "campaign pass {} {}",
                registration.code_repository, registration.worklist_pattern
            ),
        )));
    }
    let _ = validate_host(graph, config_path, &registration.flow, &registration.driver)?;
    // A steer racing this boundary has exactly two outcomes. If its append
    // wins the lock, its embargo must expire and this dispatch includes it. If
    // enqueue wins, the append follows the committed cursor and necessarily
    // changes the next observation. Holding the lock through enqueue is the
    // transaction boundary; a failed enqueue never advances the cursor.
    let steering_dispatch = loop {
        match open_local_steering_dispatch_at(host.state_dir, registration, Utc::now())? {
            LocalSteeringDispatchState::Ready(dispatch) => break dispatch,
            LocalSteeringDispatchState::Embargoed(until) => {
                let delay = (until - Utc::now().fixed_offset())
                    .to_std()
                    .unwrap_or_default();
                tokio::time::sleep(delay).await;
            }
        }
    };
    let steering = &steering_dispatch.snapshot.steering;
    let revision = campaign_observation(
        graph,
        steering,
        repository_progress,
        registration.arm_serial,
    )?;
    let executable = std::env::current_exe().context("cannot resolve tally executable")?;
    let issue_url = campaign_issue_url(
        &registration.code_repository,
        &registration.worklist_pattern,
    );
    let brief = json!({
        "campaignIdentity": &registration.registration_id,
        "repository": &registration.code_repository,
        "issue": {
            "number": LOCAL_CAMPAIGN_ISSUE_NUMBER.to_string(),
            "url": &issue_url,
        },
        "runId": &revision,
        "worklist": {
            "kind": "github-issue",
            "graphDigest": &registration.approved_graph_digest,
        },
        // Keep #433's normalized manifest receipt at the public arm boundary.
        // Current flows carry the complete graph below; compatibility callers
        // may carry only this manifest plus its graph digest, in which case
        // the driver must reconstruct and verify the omitted task envelope.
        "armedManifest": manifest,
        // The complete graph Rust normalized and hashed. The packaged driver
        // consumes this envelope; it never reparses another manifest into a
        // second executable contract.
        "campaignGraph": &graph.canonical,
        // Per-task authored input identity excludes sibling task and capacity
        // edits. The flow combines it with the addressed steering high-water
        // to derive the exact attempt epoch carried by new receipts.
        "taskInputHashes": &graph.task_input_hashes,
        // Per-task completion identity: the writer's tuple this campaign
        // admitted. The driver stamps it into the merge trailer verbatim, so
        // release proves the same task through the exact oracle rather than
        // through the legacy bridge.
        "taskCompletionRevisions": &graph.task_completion_revisions,
        "steering": steering.master,
        "taskSteering": steering.tasks,
        // The pre-agent re-check reads the same append-only source. The local
        // actor comes from the closed arm authority; it is not inferred from
        // a record that happens to be present in the log.
        "localActor": &registration.local_actor,
        "steeringSource": &steering_dispatch.snapshot.source,
        // Kept until the paired driver contract moves entirely to local names;
        // local steering authorization does not consult it.
        "allowedActors": &registration.allowed_actors,
        "capabilities": {"subIssueWalk": false},
        "workspaceRoot": &registration.workspace_root,
        // Checkpoint snapshots join the executor's existing archive so the
        // ordinary captureArchiveHorizon sweep owns their lifecycle.
        "captureRoot": host.state_dir.join("capture/archive"),
        "tally": &executable,
        "driver": &registration.driver,
        "driverRuntimeMaxSec": manifest.driver_runtime_max_sec,
        "continuation": {
            "argv": host.continuation_argv(&executable),
            // The control pool, not the campaign mutex: the scan must be free
            // to run while this pass finishes its cleanup. Its dispatch still
            // queues behind the capacity-1 runner mutex, so passes serialize.
            "pool": ["campaign-control"],
            "priority": "low",
            "runtimeMaxSec": manifest.driver_runtime_max_sec,
            "eventsDir": host.events_dir(),
        },
    });
    let payload = EnqueuePayload {
        invocation: None,
        argv: Some(host.dispatch_flow_argv(
            &executable,
            &registration.flow,
            max_flow_nodes(manifest),
            registration.projection_wait_ms,
        )),
        pools: Some(vec!["flow".to_owned(), manifest.pool.clone()]),
        executor: None,
        priority: Some(priority(&manifest.agent.priority)),
        adapter: Some("shell".to_owned()),
        cwd: None,
        workspace: None,
        adapter_options: None,
        gate_manifest: None,
        brief: Some(brief),
        brief_path: None,
        resume_from: None,
        source: Some(EnqueueSource::Manual),
        // An unchanged observation normally names the already-completed pass.
        // A liveness recovery therefore needs a distinct, deterministic dedup
        // arm or full-mode enqueue will correctly reuse that terminal witness
        // and no reconcile work will actually run. The prior flow UUID makes
        // one arm stable across poll races and advances after every real pass.
        dedup_key: Some(campaign_dispatch_dedup_key(
            registration,
            &revision,
            liveness_arm,
        )),
        submission: Some(SubmissionOptions {
            mode: SubmissionMode::Full,
        }),
        orchestration: None,
        parent: None,
        evidence: vec!["exit:0".to_owned()],
        drv: None,
        evidence_class: Some(json!({
            "kind": "forge-native-campaign",
            "issue": &issue_url,
            "registrationId": &registration.registration_id,
            "revision": &revision,
            "approvedBy": &registration.local_actor,
            "allowedActors": &registration.allowed_actors,
            "graphDigest": &registration.approved_graph_digest,
        })),
        manifest_hash: Some(graph.canonical.executable_digest.clone()),
        consumption_estimate: None,
        runtime_max_sec: manifest.runtime_max_sec,
        no_enqueue: false,
        credentials: Default::default(),
        origin: None,
        caller_job_id: inherited_caller_job_id(),
        caller_job_token: inherited_caller_job_token(),
        task_uuid: None,
        related_trigger: None,
        wait,
    };
    let client = connect_rpc(socket, config_path).await?;
    let admitted = client
        .call("queue.enqueue", Some(serde_json::to_value(payload)?))
        .await?;
    steering_dispatch.commit(&revision, Utc::now())?;
    report_degraded_membership(&admitted)?;
    // Work is flowing again under this lease, so the lease says so.
    lease.renew(Utc::now())?;
    registration.last_observation = Some(revision);
    if !wait || admitted.get("verdict").and_then(Value::as_str).is_some() {
        return Ok(admitted);
    }
    let task_uuid = admitted
        .get("task_uuid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("queue.enqueue returned no task_uuid for campaign --wait"))?;
    Ok(await_job_with_rearm(client, socket, task_uuid, rpc_timeout).await?)
}

fn read_campaign_steering_message(args: &CampaignSteerArgs) -> Result<String> {
    if let Some(message) = &args.message {
        return Ok(message.clone());
    }
    let path = args
        .message_file
        .as_deref()
        .expect("clap requires --message or --message-file");
    let mut bytes = Vec::new();
    let byte_limit = u64::try_from(MAX_CAMPAIGN_STEERING_BODY_CHARS)
        .expect("steering body bound fits u64")
        .saturating_mul(4)
        .saturating_add(1);
    if path == Path::new("-") {
        std::io::stdin()
            .take(byte_limit)
            .read_to_end(&mut bytes)
            .context("cannot read campaign steering from stdin")?;
    } else {
        fs::File::open(path)
            .with_context(|| format!("cannot open campaign steering text {}", path.display()))?
            .take(byte_limit)
            .read_to_end(&mut bytes)
            .with_context(|| format!("cannot read campaign steering text {}", path.display()))?;
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) >= byte_limit {
        return Err(invalid(format!(
            "campaign steering text exceeds {MAX_CAMPAIGN_STEERING_BODY_CHARS} characters"
        )));
    }
    String::from_utf8(bytes).context("campaign steering text is not valid UTF-8")
}

fn run_campaign_steer(args: CampaignSteerArgs) -> Result<()> {
    let (code_repository, worklist_pattern) =
        campaign_identity(&args.code_repository, &args.worklist_pattern)?;
    let state_dir = resolve_state_dir(args.state_dir.clone())?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let registration = registry
        .read_campaign(&code_repository, &worklist_pattern)?
        .ok_or_else(|| {
            invalid(format!(
                "campaign {code_repository}/{worklist_pattern} is not armed; arm it before steering"
            ))
        })?;
    require_local_actor(&registration)?;
    let task_id = args.task.clone();
    if let Some(task_id) = task_id.as_deref() {
        if !safe_task_id(task_id) {
            return Err(invalid("campaign steering --task is not a safe task ID"));
        }
        let graph = read_approved_graph_snapshot(&state_dir, &registration)?.ok_or_else(|| {
            invalid(
                "campaign has no approved graph snapshot; re-arm it before task-scoped steering",
            )
        })?;
        if !graph.manifest.tasks.iter().any(|task| task.id == task_id) {
            return Err(invalid(format!(
                "campaign has no admitted task {task_id:?}; inspect the worklist and re-arm before steering"
            )));
        }
    }
    let body = read_campaign_steering_message(&args)?;
    let record = append_local_steering_at(&state_dir, &registration, task_id, body, Utc::now())?;
    let paths = local_steering_paths(&state_dir, &registration.registration_id);
    outln!(
        "{}",
        serde_json::to_string(&json!({
            "status": "recorded",
            "codeRepository": code_repository,
            "worklistPattern": worklist_pattern,
            "taskId": record.task_id,
            "sequence": record.sequence,
            "comment": record.comment,
            "doNotDispatchBefore": record.do_not_dispatch_before,
            "source": {
                "kind": "local-jsonl",
                "path": paths.log,
            },
            // The stdout receipt is what an SSH caller reads back. No forge
            // acknowledgement or coordinator-side shell parsing is involved.
            "offHostReceipt": "stdout-json-v1",
        }))?
    );
    Ok(())
}

async fn run_campaign_arm(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: CampaignArmArgs,
) -> Result<()> {
    let (code_repository, worklist_pattern) =
        campaign_identity(&args.code_repository, &args.worklist_pattern)?;
    let state_dir = resolve_state_dir(args.state_dir.clone())?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let prior = registry.read_campaign(&code_repository, &worklist_pattern)?;
    if let Some(registration) = &prior {
        require_local_actor(registration)?;
    }
    let local_actor = local_actor();
    let repository = campaign_repository_from_arm(&args)?;
    let adapters = load_client_config(config_path)?.adapters;
    let flow = resolve_campaign_asset(
        args.flow,
        Path::new("../share/tally/flows/spec-build.js"),
        "flow",
    )?;
    let driver = resolve_campaign_asset(
        args.driver,
        Path::new("../libexec/tally/spec-build-driver"),
        "driver",
    )?;
    let graph = local_campaign_graph_from_worklist(
        &state_dir,
        repository,
        &code_repository,
        &worklist_pattern,
        &adapters,
    )?;
    let allowed_actors = normalize_allowed_actors(&args.allowed_actors, LOCAL_ALLOWED_ACTOR)?;
    // Arm-time conflictDomains warnings are advisory: they share the receipt
    // surface with host warnings but never participate in graph admission.
    let mut arm_warnings = graph.ownership_preflight_warnings.clone();
    let workspace_root = args.workspace_root.unwrap_or_else(|| {
        state_dir
            .join("campaigns")
            .join(&graph.canonical.manifest.name)
    });
    if !workspace_root.is_absolute() {
        return Err(invalid("campaign workspace root must be absolute"));
    }
    let projection_wait_ms = validated_projection_wait_ms(args.projection_wait_ms)?;
    let arm_serial = prior.as_ref().map_or(Ok(1), |value| {
        value
            .arm_serial
            .checked_add(1)
            .ok_or_else(|| invalid("campaign arm retry counter is exhausted"))
    })?;
    let mut registration = CampaignRegistration::new(
        CampaignRegistrationV4 {
            schema_version: REGISTRY_SCHEMA_VERSION,
            registration_id: prior.as_ref().map_or_else(
                || uuid::Uuid::now_v7().to_string(),
                |value| value.registration_id.clone(),
            ),
            worklist_pattern: worklist_pattern.clone(),
            code_repository: code_repository.clone(),
            checkout: graph.canonical.manifest.repository.checkout.clone(),
            base_branch: graph.canonical.manifest.repository.base_branch.clone(),
            remote: graph.canonical.manifest.repository.remote.clone(),
            armed_at: Utc::now().to_rfc3339(),
            arm_serial,
            approved_graph_digest: graph.canonical.executable_digest.clone(),
            local_actor,
            allowed_actors,
            last_observation: prior
                .as_ref()
                .and_then(|value| value.last_observation.clone()),
            flow,
            driver,
            workspace_root,
        },
        projection_wait_ms,
    );
    arm_warnings.extend(validate_host(
        &graph,
        config_path,
        &registration.flow,
        &registration.driver,
    )?);
    // Publish the arm authority before a pass can append stamped receipts.
    write_local_attempt_receipt_authority(&state_dir, &graph, registration.arm_serial)?;
    write_approved_graph_snapshot(&state_dir, &registration, &graph.canonical)?;
    registry.write(&mut registration)?;
    prune_approved_graph_snapshots(&state_dir, &registration)?;
    if args.no_enqueue {
        let issue_url = campaign_issue_url(&code_repository, &worklist_pattern);
        let receipt = json!({
            "status": "armed",
            "issue": issue_url,
            "codeRepository": code_repository,
            "worklistPattern": worklist_pattern,
            "tasks": graph.canonical.tasks.len(),
            "graphDigest": graph.canonical.executable_digest,
            "allowedActors": registration.allowed_actors,
            "enqueued": false,
        });
        outln!(
            "{}",
            serde_json::to_string(&arm_receipt(&receipt, &arm_warnings, &graph.gate_budgets))?
        );
        return Ok(());
    }
    let repository_progress = repository_progress_value(&graph)?;
    // Activation: the doorbell acquires the lease the passes then renew. A
    // campaign already leased by a live pass, or already lapsed on this very
    // graph, says so here instead of dispatching a second frontier.
    let store = campaign_lease_store(&state_dir, &registration);
    let mut lease = store.acquire(&campaign_activation(&graph, &registration), Utc::now())?;
    let result = dispatch_campaign(
        CampaignHost {
            socket,
            config_path,
            state_dir: &state_dir,
            rpc_timeout,
        },
        &graph,
        &repository_progress,
        &mut registration,
        args.wait,
        None,
        &mut lease,
    )
    .await?;
    registry.write(&mut registration)?;
    outln!(
        "{}",
        serde_json::to_string(&arm_receipt(&result, &arm_warnings, &graph.gate_budgets))?
    );
    if args.wait {
        let code = waited_exit_code(&result);
        if code != 0 {
            return Err(anyhow::Error::new(ExitFailure {
                code,
                message: "campaign reconcile pass returned a non-zero verdict".to_owned(),
            }));
        }
    }
    Ok(())
}

/// Admit a worklist change observed at the identity's authority remote as a
/// fresh reconcile epoch, with no operator verb between the push and the
/// dispatch.
///
/// `forge:"local"` promises REMOTE-AUTHORITY (sitting-c1, R4): the identity's
/// authority surface is the local checkout's own configured git remote, which
/// is exactly what the graph handed here was read from. Pushing the worklist
/// *is* the arming act, so this is `run_campaign_arm`'s epoch flip minus the
/// human — same order, same durable artifacts, same arm-serial discipline.
///
/// The order is the safety property. Host validation runs against the pushed
/// graph before any authority moves, so a worklist naming an adapter this
/// host cannot serve is refused while the campaign is still armed on the
/// epoch that worked; a bad push cannot cost a good epoch. Only then does the
/// receipt authority, the approved-graph snapshot, and the registration
/// advance together, and the superseded snapshot stays on disk so an attempt
/// that straddles this moment can still be told which digest it owns.
fn readmit_campaign_epoch(
    state_dir: &Path,
    registry: &CampaignRegistry,
    registration: &mut CampaignRegistration,
    graph: &CampaignGraph,
    config_path: Option<&Path>,
) -> Result<String> {
    let _ = validate_host(graph, config_path, &registration.flow, &registration.driver)?;
    let superseded_graph_digest = registration.approved_graph_digest.clone();
    let arm_serial = registration
        .arm_serial
        .checked_add(1)
        .ok_or_else(|| invalid("campaign arm retry counter is exhausted"))?;
    registration.arm_serial = arm_serial;
    registration
        .approved_graph_digest
        .clone_from(&graph.canonical.executable_digest);
    registration.armed_at = Utc::now().to_rfc3339();
    // Publish the arm authority before a pass can append stamped receipts.
    write_local_attempt_receipt_authority(state_dir, graph, arm_serial)?;
    write_approved_graph_snapshot(state_dir, registration, &graph.canonical)?;
    registry.write(registration)?;
    prune_approved_graph_snapshots(state_dir, registration)?;
    Ok(superseded_graph_digest)
}

/// This identity's durable lease.
///
/// It is named for the identity — repository and worklist — and never for the
/// registration serving it. The lapse fact outlives every pass, and a release
/// reads it with no registration in hand at all.
fn campaign_lease_store(
    state_dir: &Path,
    registration: &CampaignRegistration,
) -> CampaignLeaseStore {
    CampaignLeaseStore::new(
        state_dir,
        &registration.code_repository,
        &registration.worklist_pattern,
    )
}

fn campaign_activation(
    graph: &CampaignGraph,
    registration: &CampaignRegistration,
) -> CampaignActivation {
    CampaignActivation {
        campaign: graph.canonical.manifest.name.clone(),
        repository: registration.code_repository.clone(),
        worklist: registration.worklist_pattern.clone(),
        arm_serial: registration.arm_serial,
        graph_digest: graph.canonical.executable_digest.clone(),
    }
}

/// The witnessed facts this identity's lease decision rests on.
///
/// Everything here comes from the authority remote the pass already queried
/// for its observation revision: the base head it publishes and the
/// campaign-scoped refs its merges, checkpoints, and publications wrote. No
/// second source, and nothing the pass predicted about itself.
fn campaign_lease_facts(
    graph: &CampaignGraph,
    repository_progress: &Value,
    live_nodes: usize,
) -> Result<CampaignLeaseFacts> {
    let published_head = repository_progress["base"]["target"]
        .as_str()
        .ok_or_else(|| invalid("campaign repository progress carries no base head"))?;
    let campaign_refs = repository_progress["campaignRefs"]
        .as_object()
        .ok_or_else(|| invalid("campaign repository progress carries no campaign refs"))?
        .iter()
        .map(|(name, target)| {
            let target = target.as_str().ok_or_else(|| {
                invalid(format!("campaign repository ref {name} has no object ID"))
            })?;
            Ok((name.clone(), target.to_owned()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(CampaignLeaseFacts {
        state_prefix: campaign_state_ref_prefix(
            &graph.canonical.manifest.name,
            LOCAL_CAMPAIGN_ISSUE_NUMBER,
        ),
        tasks: graph
            .canonical
            .manifest
            .tasks
            .iter()
            .map(|task| CampaignLeaseTask::new(&task.id, task.kind == "checkpoint"))
            .collect(),
        campaign_refs,
        published_head: published_head.to_owned(),
        live_nodes,
    })
}

/// What one reconcile pass may do with an identity.
#[derive(Debug)]
enum CampaignPassAdmission {
    /// Another pass holds the lease. This one admitted nothing and will
    /// dispatch nothing.
    Deferred { detail: String },
    /// The identity is finished under the graph its remote carries, and the
    /// fact was written by whichever pass got there first.
    Complete(CampaignLapseV1),
    /// Activation belongs to this pass, with the epoch it admitted on the way
    /// in.
    Open {
        lease: Box<CampaignLeaseGuard>,
        superseded_graph_digest: Option<String>,
    },
}

/// Take activation for one pass, then admit whatever the authority remote now
/// carries.
///
/// The order is the safety property, and it is the reverse of the one the
/// record charges for the re-arm orphan. The lease is taken *before* any
/// epoch moves, so two passes observing the same push cannot both bump the
/// arm serial and dispatch the frontier twice: the second one is told the
/// identity is leased and stands down having changed nothing. A lapsed
/// identity never opens at all for the graph it finished, which is what makes
/// a poll against a complete campaign free rather than merely harmless.
fn open_campaign_pass(
    state_dir: &Path,
    registry: &CampaignRegistry,
    registration: &mut CampaignRegistration,
    graph: &CampaignGraph,
    config_path: Option<&Path>,
    now: DateTime<Utc>,
) -> Result<CampaignPassAdmission> {
    let store = campaign_lease_store(state_dir, registration);
    let mut lease = match store.acquire(&campaign_activation(graph, registration), now) {
        Ok(lease) => lease,
        Err(error @ CampaignLeaseError::Held { .. }) => {
            return Ok(CampaignPassAdmission::Deferred {
                detail: error.to_string(),
            })
        }
        Err(CampaignLeaseError::Lapsed { campaign, .. }) => {
            let lapse = store
                .read()?
                .and_then(|record| record.lapse)
                .ok_or_else(|| {
                    invalid(format!(
                        "campaign {campaign} lapsed without a completion fact"
                    ))
                })?;
            return Ok(CampaignPassAdmission::Complete(lapse));
        }
        Err(error) => return Err(error.into()),
    };
    // Whatever this pass read before it held the lease is a snapshot from
    // outside it. A pass that was queued at the lock while another admitted a
    // pushed worklist would otherwise admit that same push a second time and
    // dispatch the frontier twice, so the identity is re-read under the lease
    // and only then compared against the remote.
    if let Some(admitted) = registry.read_campaign(
        &registration.code_repository,
        &registration.worklist_pattern,
    )? {
        *registration = admitted;
    }
    let superseded_graph_digest =
        if graph.canonical.executable_digest == registration.approved_graph_digest {
            None
        } else {
            Some(readmit_campaign_epoch(
                state_dir,
                registry,
                registration,
                graph,
                config_path,
            )?)
        };
    // The lease names the epoch this pass serves, admitted or inherited.
    lease.admit(registration.arm_serial, now)?;
    Ok(CampaignPassAdmission::Open {
        lease: Box::new(lease),
        superseded_graph_digest,
    })
}

async fn run_campaign_poll(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: CampaignPollArgs,
) -> Result<()> {
    if !args.once {
        return Err(invalid("campaign poll currently requires --once"));
    }
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let entries = registry.registrations()?;
    let adapters = load_client_config(config_path)?.adapters;
    let mut had_failures = false;
    for (path, mut registration) in entries {
        let event_issue = campaign_issue_url(
            &registration.code_repository,
            &registration.worklist_pattern,
        );
        let registration_path = path.display().to_string();
        // A re-admission is a durable fact of its own, recorded before the
        // pass it enables reports anything. An operator reading the journal
        // sees the epoch flip even when the dispatch behind it then fails.
        let mut events = Vec::new();
        let attempt = async {
            require_local_actor(&registration)?;
            let repository = campaign_repository_from_registration(&registration);
            let graph = local_campaign_graph_from_worklist(
                &state_dir,
                repository,
                &registration.code_repository,
                &registration.worklist_pattern,
                &adapters,
            )?;
            let mut lease = match open_campaign_pass(
                &state_dir,
                &registry,
                &mut registration,
                &graph,
                config_path,
                Utc::now(),
            )? {
                CampaignPassAdmission::Deferred { detail } => {
                    return Ok(CampaignPollAttempt::Deferred { detail })
                }
                CampaignPassAdmission::Complete(lapse) => {
                    return Ok(CampaignPollAttempt::Complete(lapse))
                }
                CampaignPassAdmission::Open {
                    lease,
                    superseded_graph_digest,
                } => {
                    if let Some(superseded) = superseded_graph_digest {
                        events.push(CampaignPollEvent::readmitted(
                            &registration.registration_id,
                            &event_issue,
                            &registration_path,
                            superseded,
                            &registration.approved_graph_digest,
                            registration.arm_serial,
                        ));
                    }
                    *lease
                }
            };
            let repository_progress = repository_progress_value(&graph)?;
            let steering = read_local_steering_snapshot(&state_dir, &registration)?;
            let observation = campaign_observation(
                &graph,
                &steering.steering,
                &repository_progress,
                registration.arm_serial,
            )?;
            let host = CampaignHost {
                socket,
                config_path,
                state_dir: &state_dir,
                rpc_timeout,
            };
            let liveness_arm = if registration.last_observation.as_deref() == Some(&observation) {
                match campaign_poll_liveness_arm(host, &graph, &registration, &observation).await? {
                    CampaignPassLiveness::Dispatchable(flow_run_id) => Some(flow_run_id),
                    CampaignPassLiveness::Live { nodes } => {
                        lease.renew(Utc::now())?;
                        return Ok(CampaignPollAttempt::Unchanged {
                            detail: Some(format!("{nodes} node(s) live under this lease")),
                        });
                    }
                    // A pass at rest with nothing to dispatch is the only
                    // moment completion can be decided, and the lease is
                    // where the decision is written.
                    CampaignPassLiveness::AtRest => {
                        let facts = campaign_lease_facts(&graph, &repository_progress, 0)?;
                        return match lease_disposition(&facts) {
                            CampaignLeaseDisposition::Lapse {
                                sha,
                                proven_by,
                                tasks,
                                ..
                            } => Ok(CampaignPollAttempt::Complete(lease.lapse(
                                &sha,
                                proven_by,
                                tasks,
                                Utc::now(),
                            )?)),
                            CampaignLeaseDisposition::Renew { reason } => {
                                lease.renew(Utc::now())?;
                                Ok(CampaignPollAttempt::Unchanged {
                                    detail: Some(reason),
                                })
                            }
                        };
                    }
                }
            } else {
                None
            };
            let result = dispatch_campaign(
                host,
                &graph,
                &repository_progress,
                &mut registration,
                args.wait,
                liveness_arm.as_deref(),
                &mut lease,
            )
            .await?;
            registry.write(&mut registration)?;
            if args.wait {
                let code = waited_exit_code(&result);
                if code != 0 {
                    return Err(anyhow::Error::new(ExitFailure {
                        code,
                        message: format!(
                            "campaign reconcile pass for {}/{} returned a non-zero verdict",
                            registration.code_repository, registration.worklist_pattern
                        ),
                    }));
                }
            }
            Ok::<_, anyhow::Error>(CampaignPollAttempt::Dispatched)
        }
        .await;
        events.push(match attempt {
            Ok(CampaignPollAttempt::Dispatched) => CampaignPollEvent::new(
                &registration.registration_id,
                &event_issue,
                &registration_path,
                CampaignPollStatus::Dispatched,
            ),
            Ok(CampaignPollAttempt::Unchanged { detail }) => {
                let mut event = CampaignPollEvent::new(
                    &registration.registration_id,
                    &event_issue,
                    &registration_path,
                    CampaignPollStatus::Unchanged,
                );
                event.detail = detail;
                event
            }
            Ok(CampaignPollAttempt::Deferred { detail }) => CampaignPollEvent::deferred(
                &registration.registration_id,
                &event_issue,
                &registration_path,
                detail,
            ),
            // The identity is finished and the fact is written. Nothing is
            // pruned: the registration stays armed, and the next push to its
            // worklist reactivates it with no operator verb in between.
            Ok(CampaignPollAttempt::Complete(lapse)) => CampaignPollEvent::complete(
                &registration.registration_id,
                &event_issue,
                &registration_path,
                &lapse.sha,
                lapse.arm_serial,
            ),
            Err(error) => {
                had_failures = true;
                CampaignPollEvent::failed(
                    &registration.registration_id,
                    &event_issue,
                    &registration_path,
                    format!("{error:#}"),
                )
            }
        });
        for event in events {
            outln!("{}", serde_json::to_string(&event)?);
        }
    }
    if had_failures {
        bail!("one or more armed campaigns could not be polled")
    } else {
        Ok(())
    }
}

async fn run_campaign_status(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: CampaignStatusArgs,
) -> Result<()> {
    let (code_repository, worklist_pattern) =
        campaign_identity(&args.code_repository, &args.worklist_pattern)?;
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let registration = registry.read_campaign(&code_repository, &worklist_pattern)?;
    if let Some(registration) = &registration {
        require_local_actor(registration)?;
    }
    let approved_graph = registration
        .as_ref()
        .map(|registration| read_approved_graph_snapshot(&state_dir, registration))
        .transpose()?
        .flatten();
    let issue_url = campaign_issue_url(&code_repository, &worklist_pattern);
    let params = json!({
        "issueUrl": issue_url,
        "registrationId": registration
            .as_ref()
            .map(|registration| registration.registration_id.as_str()),
        "latestObservation": registration
            .as_ref()
            .and_then(|registration| registration.last_observation.as_deref()),
    });
    let client = connect_rpc(socket, config_path).await?;
    let latest = client
        .call_with_deadline("__campaign.status", Some(params), rpc_timeout)
        .await?;
    let mut status = reconciled_campaign_status(
        &client,
        rpc_timeout,
        latest,
        registration.as_ref(),
        approved_graph.as_ref(),
        &code_repository,
        &worklist_pattern,
    )
    .await?;
    attach_campaign_status_outcomes(
        &state_dir,
        approved_graph.as_ref(),
        registration.as_ref(),
        &mut status,
    )?;
    attach_campaign_lease(&state_dir, registration.as_ref(), &mut status)?;
    if args.json {
        outln!("{}", serde_json::to_string(&status)?);
        return Ok(());
    }
    print_campaign_status_human(&status)
}

fn campaign_status_tasks<'a>(value: &'a Value, context: &str) -> Result<&'a [Value]> {
    match value.get("tasks") {
        None => Ok(&[]),
        Some(tasks) => tasks
            .as_array()
            .map(Vec::as_slice)
            .ok_or_else(|| invalid(format!("daemon returned an invalid {context} task table"))),
    }
}

async fn most_recent_reconciled_campaign_run(
    client: &RpcClient,
    rpc_timeout: Duration,
    status: &Value,
) -> Result<Option<Value>> {
    let latest = status["flowRunId"]
        .as_str()
        .ok_or_else(|| invalid("daemon returned an unidentifiable campaign pass"))?;
    let flow_runs = status["flowRuns"]
        .as_array()
        .ok_or_else(|| invalid("daemon returned an invalid campaign pass lineage"))?;
    for candidate in flow_runs.iter().rev() {
        let candidate = candidate
            .as_str()
            .ok_or_else(|| invalid("daemon returned a non-string campaign pass ID"))?;
        if candidate == latest {
            continue;
        }
        let run = client
            .call_with_deadline("query.run", Some(json!({"id": candidate})), rpc_timeout)
            .await?;
        if !campaign_status_tasks(&run, "prior campaign run")?.is_empty() {
            return Ok(Some(run));
        }
    }
    Ok(None)
}

fn durable_registration_task_state(
    registration: &CampaignRegistration,
    graph: &CanonicalCampaignGraphV1,
) -> Result<(Value, Value)> {
    let titles = graph
        .tasks
        .iter()
        .map(|task| (task.number, task.title.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut pending = 0usize;
    let mut blocked = 0usize;
    let tasks = graph
        .manifest
        .tasks
        .iter()
        .map(|task| {
            let title = titles.get(&task.issue).ok_or_else(|| {
                invalid(format!(
                    "campaign approved graph has no content for task {}",
                    task.id
                ))
            })?;
            let status = if task.dependencies.is_empty() {
                pending += 1;
                "pending"
            } else {
                blocked += 1;
                "blocked"
            };
            Ok(json!({
                "taskRef": format!("{}/{}", registration.registration_id, task.id),
                "title": title,
                "status": status,
                "blockedBy": task.dependencies,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((
        Value::Array(tasks),
        json!({"done": 0, "running": 0, "blocked": blocked, "pending": pending}),
    ))
}

fn campaign_name_for_status(
    graph: Option<&CanonicalCampaignGraphV1>,
    reconciled: &Value,
    worklist_pattern: &str,
) -> String {
    graph
        .map(|graph| graph.manifest.name.clone())
        .or_else(|| {
            reconciled["campaign"]
                .as_str()
                .filter(|name| !name.trim().is_empty() && *name != "campaign")
                .map(ToOwned::to_owned)
        })
        .or_else(|| worklist_default_campaign_name(worklist_pattern).ok())
        .unwrap_or_else(|| worklist_pattern.to_owned())
}

fn replace_campaign_projection(target: &mut Value, source: &Value) -> Result<()> {
    let target = target
        .as_object_mut()
        .ok_or_else(|| invalid("daemon returned a non-object campaign status"))?;
    for field in [
        "flowRunId",
        "state",
        "flowName",
        "counts",
        "items",
        "tasks",
        "anomalies",
        "currentNodes",
        "failures",
    ] {
        if let Some(value) = source.get(field) {
            target.insert(field.to_owned(), value.clone());
        } else if matches!(field, "items" | "anomalies" | "currentNodes" | "failures") {
            target.insert(field.to_owned(), json!([]));
        }
    }
    Ok(())
}

/// Keep a queued successor visible without letting its empty pre-reconcile
/// projection erase the most recent task truth.
async fn reconciled_campaign_status(
    client: &RpcClient,
    rpc_timeout: Duration,
    mut status: Value,
    registration: Option<&CampaignRegistration>,
    graph: Option<&CanonicalCampaignGraphV1>,
    code_repository: &str,
    worklist_pattern: &str,
) -> Result<Value> {
    let unreconciled = status["flowRunId"].as_str().is_some()
        && campaign_status_tasks(&status, "campaign")?.is_empty();
    let mut name_source = status.clone();
    if unreconciled {
        let queued_flow_run = status["flowRunId"]
            .as_str()
            .expect("the unreconciled predicate requires a flow run")
            .to_owned();
        if let Some(reconciled) =
            most_recent_reconciled_campaign_run(client, rpc_timeout, &status).await?
        {
            name_source = reconciled.clone();
            replace_campaign_projection(&mut status, &reconciled)?;
            let object = status
                .as_object_mut()
                .expect("replace_campaign_projection validated the status object");
            object.insert("taskTableSource".to_owned(), json!("reconciled-pass"));
        } else {
            let (registration, graph) = registration.zip(graph).ok_or_else(|| {
                invalid(format!(
                    "queued campaign pass {queued_flow_run} has no reconciled predecessor or durable approved graph"
                ))
            })?;
            let (tasks, counts) = durable_registration_task_state(registration, graph)?;
            let object = status
                .as_object_mut()
                .ok_or_else(|| invalid("daemon returned a non-object campaign status"))?;
            object.remove("flowRunId");
            object.insert("state".to_owned(), json!("armed"));
            object.insert("counts".to_owned(), counts);
            object.insert("items".to_owned(), json!([]));
            object.insert("tasks".to_owned(), tasks);
            object.insert("anomalies".to_owned(), json!([]));
            object.insert("currentNodes".to_owned(), json!([]));
            object.insert("failures".to_owned(), json!([]));
            object.insert("taskTableSource".to_owned(), json!("registration"));
        }
        status
            .as_object_mut()
            .expect("campaign status was validated above")
            .insert("queuedFlowRunId".to_owned(), json!(queued_flow_run));
    }

    let name = campaign_name_for_status(graph, &name_source, worklist_pattern);
    let object = status
        .as_object_mut()
        .ok_or_else(|| invalid("daemon returned a non-object campaign status"))?;
    object.insert("campaign".to_owned(), json!(name));
    object.insert("repository".to_owned(), json!(code_repository));
    Ok(status)
}

fn print_campaign_status_human(status: &Value) -> Result<()> {
    let issue = status["issueUrl"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("daemon returned an invalid campaign status response"))?;
    let state = status["state"].as_str().unwrap_or("unknown");
    let name = status["campaign"]
        .as_str()
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("campaign status has no authoritative campaign name"))?;
    outln!("Campaign {}  {}", compact_text(name), compact_text(state));
    outln!("  {}", compact_text(issue));
    if status["registered"].as_bool() == Some(true) {
        outln!(
            "Registration: {} (armed)",
            compact_text(status["registrationId"].as_str().unwrap_or("-"))
        );
    } else {
        outln!("Registration: inactive; resolved from durable campaign lineage");
    }
    outln!(
        "Observation: {}",
        compact_text(status["latestObservation"].as_str().unwrap_or("none"))
    );
    print_campaign_lease(&status["lease"])?;
    let run_count = status["flowRuns"].as_array().map_or(0, Vec::len);
    if let Some(queued_flow_run) = status["queuedFlowRunId"].as_str() {
        outln!(
            "Latest flow run: {} (queued, awaiting reconciliation; {} pass{})",
            compact_text(queued_flow_run),
            run_count,
            if run_count == 1 { "" } else { "es" }
        );
        if let Some(flow_run) = status["flowRunId"].as_str() {
            outln!(
                "Rendered truth: {} (most recent reconciled pass)",
                compact_text(flow_run)
            );
        } else {
            outln!("Rendered truth: durable registration (no pass has reconciled yet)");
        }
        return print_run_body(status, None, "Campaign usage");
    }
    let Some(flow_run) = status["flowRunId"].as_str() else {
        outln!("Latest flow run: none (no reconcile pass admitted)");
        outln!("Campaign usage: no flow run admitted");
        return Ok(());
    };
    outln!(
        "Latest flow run: {} ({} pass{})",
        compact_text(flow_run),
        run_count,
        if run_count == 1 { "" } else { "es" }
    );
    print_run_body(status, None, "Campaign usage")
}

/// The lease line: whether this identity is live, or finished and on what.
fn print_campaign_lease(lease: &Value) -> Result<()> {
    let Some(state) = lease["state"].as_str() else {
        outln!("Lease: none on this host");
        return Ok(());
    };
    match lease.get("lapse") {
        Some(lapse) => outln!(
            "Lease: {} at {} on published head {} (proven by {})",
            compact_text(state),
            compact_text(lapse["lapsedAt"].as_str().unwrap_or("-")),
            compact_text(lapse["sha"].as_str().unwrap_or("-")),
            compact_text(lapse["provenBy"]["taskId"].as_str().unwrap_or("-")),
        ),
        None => outln!(
            "Lease: {} since {}, renewed {} (arm {})",
            compact_text(state),
            compact_text(lease["acquiredAt"].as_str().unwrap_or("-")),
            compact_text(lease["renewedAt"].as_str().unwrap_or("-")),
            lease["armSerial"].as_u64().unwrap_or_default(),
        ),
    }
    Ok(())
}

fn run_campaign_list(args: CampaignListArgs) -> Result<()> {
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let values = campaign_list_values(&registry)?;
    outln!("{}", serde_json::to_string(&values)?);
    Ok(())
}

fn campaign_list_values(registry: &CampaignRegistry) -> Result<Vec<Value>> {
    registry
        .registrations()?
        .into_iter()
        .map(|(_, registration)| {
            require_local_actor(&registration)?;
            Ok(registration.list_value()?)
        })
        .collect()
}

/// Quiescence, read from durable facts rather than observed.
///
/// The predicate used to be "no registration is listed", which made an
/// operator's `disarm` the only way a campaign could ever be quiet and left
/// liveness to be inferred from `list-units` beside it. A campaign is quiet
/// now when its lease has lapsed: the last task went terminal under a
/// gate-proven, published head and a pass wrote that down. The identity stays
/// armed through it, so a push to its worklist still reactivates it.
fn run_campaign_quiescent(args: CampaignQuiescentArgs) -> Result<()> {
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let mut live = Vec::new();
    for (_, registration) in registry.registrations()? {
        require_local_actor(&registration)?;
        let lease = campaign_lease_store(&state_dir, &registration).read()?;
        // A registration with no lease at all has never activated on this
        // host; it cannot be called finished on the strength of an absence.
        if lease.is_some_and(|record| record.is_lapsed()) {
            continue;
        }
        live.push(registration.list_value()?);
    }
    if live.is_empty() {
        return Ok(());
    }

    errln!("{}", serde_json::to_string(&live)?);
    Err(exit_failure(1, String::new()))
}

fn run_campaign_disarm(args: CampaignDisarmArgs) -> Result<()> {
    let (code_repository, worklist_pattern) =
        campaign_identity(&args.code_repository, &args.worklist_pattern)?;
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let registration = registry.read_campaign(&code_repository, &worklist_pattern)?;
    let removed = if let Some(registration) = registration {
        require_local_actor(&registration)?;
        registry.remove(&registration)?;
        remove_approved_graph_snapshots(&state_dir, &registration.registration_id)?;
        true
    } else {
        false
    };
    outln!(
        "{}",
        serde_json::to_string(&json!({
            "codeRepository": code_repository,
            "worklistPattern": worklist_pattern,
            "disarmed": removed,
        }))?
    );
    Ok(())
}

fn required_worklist_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && !value.contains('\0'))
        .ok_or_else(|| invalid(format!("{context}.{field} must be a non-empty string")))
}

fn worklist_string_list(value: Option<&Value>, context: &str) -> Result<Vec<String>> {
    value.map_or_else(
        || Ok(Vec::new()),
        |value| {
            value
                .as_array()
                .ok_or_else(|| invalid(format!("{context} must be an array")))?
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    item.as_str()
                        .filter(|text| !text.is_empty() && !text.contains('\0'))
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            invalid(format!("{context}[{index}] must be a non-empty string"))
                        })
                })
                .collect()
        },
    )
}

fn render_worklist_task_body(
    object: &serde_json::Map<String, Value>,
    context: &str,
    checkpoint: Option<CheckpointBrief<'_>>,
) -> Result<String> {
    if let Some(body) = object.get("body") {
        return body
            .as_str()
            .filter(|body| !body.trim().is_empty() && !body.contains('\0'))
            .map(ToOwned::to_owned)
            .ok_or_else(|| invalid(format!("{context}.body must be a non-empty string")));
    }
    if let Some(checkpoint) = checkpoint {
        let argv = serde_json::to_string(checkpoint.argv)?;
        let dependencies = serde_json::to_string(checkpoint.dependencies)?;
        return Ok(format!(
            "## Checkpoint\n\nCampaign: `{}`\n\n## Gate argv\n\n    {argv}\n\n## Runtime limit\n\n{} seconds\n\n## Dependencies\n\n    {dependencies}\n",
            checkpoint.campaign_name, checkpoint.runtime_max_sec
        ));
    }
    let goal = required_worklist_string(object, "goal", context)?;
    let delivered = worklist_string_list(
        object.get("deliveredBehaviors"),
        &format!("{context}.deliveredBehaviors"),
    )?;
    let acceptance = object
        .get("acceptanceCriteria")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("{context}.acceptanceCriteria must be an array")))?;
    if delivered.is_empty() || acceptance.is_empty() {
        return Err(invalid(format!(
            "{context} without body requires non-empty deliveredBehaviors and acceptanceCriteria"
        )));
    }
    let mut body = format!("## Goal\n\n{goal}\n\n## Delivered behaviors\n");
    for item in delivered {
        body.push_str(&format!("\n- {item}"));
    }
    if let Some(read_first) = object.get("readFirst").and_then(Value::as_object) {
        body.push_str("\n\n## Read first\n");
        for field in ["specSections", "styleReferences"] {
            for item in worklist_string_list(
                read_first.get(field),
                &format!("{context}.readFirst.{field}"),
            )? {
                body.push_str(&format!("\n- {item}"));
            }
        }
    }
    body.push_str("\n\n## Acceptance criteria\n");
    for (index, candidate) in acceptance.iter().enumerate() {
        let item = candidate.as_object().ok_or_else(|| {
            invalid(format!(
                "{context}.acceptanceCriteria[{index}] must be an object"
            ))
        })?;
        let identifier = required_worklist_string(
            item,
            "id",
            &format!("{context}.acceptanceCriteria[{index}]"),
        )?;
        let description = required_worklist_string(
            item,
            "description",
            &format!("{context}.acceptanceCriteria[{index}]"),
        )?;
        body.push_str(&format!("\n- [ ] `{identifier}` — {description}"));
        if let Some(arguments) = item.get("argv") {
            let rendered = worklist_string_list(
                Some(arguments),
                &format!("{context}.acceptanceCriteria[{index}].argv"),
            )?;
            if !rendered.is_empty() {
                body.push_str(&format!(" (`{}`)", rendered.join(" ")));
            }
        }
    }
    body.push('\n');
    Ok(body)
}

fn worklist_tasks(document: &Value, campaign_name: &str) -> Result<Vec<WorklistTask>> {
    let object = document
        .as_object()
        .ok_or_else(|| invalid("campaign worklist must be an object"))?;
    if object.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err(invalid("campaign worklist schemaVersion must equal 1"));
    }
    let candidates = object
        .get("tasks")
        .and_then(Value::as_array)
        .filter(|tasks| !tasks.is_empty() && tasks.len() <= MAX_CAMPAIGN_TASKS)
        .ok_or_else(|| {
            invalid(format!(
                "campaign worklist must contain 1..={MAX_CAMPAIGN_TASKS} tasks"
            ))
        })?;
    let mut prior = BTreeSet::new();
    let mut issues = BTreeSet::new();
    let mut tasks = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        let context = format!("tasks[{index}]");
        let item = candidate
            .as_object()
            .ok_or_else(|| invalid(format!("{context} must be an object")))?;
        let kind = required_worklist_string(item, "kind", &context)?.to_owned();
        if !matches!(kind.as_str(), "implementation" | "checkpoint") {
            return Err(invalid(format!(
                "{context}.kind must be implementation or checkpoint"
            )));
        }
        let allowed = match kind.as_str() {
            "implementation" => BTreeSet::from([
                "id",
                "kind",
                "title",
                "body",
                "goal",
                "deliveredBehaviors",
                "readFirst",
                "acceptanceCriteria",
                "issue",
                "dependencies",
                "conflictDomains",
            ]),
            "checkpoint" => BTreeSet::from([
                "id",
                "kind",
                "title",
                "body",
                "issue",
                "dependencies",
                "argv",
                "runtimeMaxSec",
            ]),
            _ => unreachable!(),
        };
        if let Some(field) = item.keys().find(|field| !allowed.contains(field.as_str())) {
            return Err(invalid(format!(
                "{context} contains unsupported field {field:?} for kind {kind}"
            )));
        }
        let id = required_worklist_string(item, "id", &context)?.to_owned();
        if !safe_task_id(&id) || !prior.insert(id.clone()) {
            return Err(invalid(format!("{context}.id is invalid or duplicated")));
        }
        let title = required_worklist_string(item, "title", &context)?.to_owned();
        if title.len() > 300 || title.contains(['\r', '\n']) {
            return Err(invalid(format!(
                "{context}.title must fit on one line and be at most 300 bytes"
            )));
        }
        let issue =
            item.get("issue")
                .map(|value| {
                    value.as_u64().filter(|number| *number > 0).ok_or_else(|| {
                        invalid(format!("{context}.issue must be a positive integer"))
                    })
                })
                .transpose()?;
        if issue.is_some_and(|number| !issues.insert(number)) {
            return Err(invalid("campaign worklist repeats a task number"));
        }
        let dependencies =
            worklist_string_list(item.get("dependencies"), &format!("{context}.dependencies"))?;
        let mut seen_dependencies = BTreeSet::new();
        for dependency in &dependencies {
            if !prior.contains(dependency)
                || dependency == &id
                || !seen_dependencies.insert(dependency.clone())
            {
                return Err(invalid(format!(
                    "{context}.dependencies must be unique earlier task ids"
                )));
            }
        }
        let conflict_domains = if kind == "implementation" {
            item.get("conflictDomains")
                .map(|value| {
                    worklist_string_list(Some(value), &format!("{context}.conflictDomains"))
                })
                .transpose()?
        } else {
            None
        };
        let argv = if kind == "checkpoint" {
            let values = worklist_string_list(item.get("argv"), &format!("{context}.argv"))?;
            validate_argv(&values, &format!("{context}.argv"))?;
            Some(values)
        } else {
            None
        };
        let runtime_max_sec = if kind == "checkpoint" {
            Some(
                item.get("runtimeMaxSec")
                    .and_then(Value::as_u64)
                    .filter(|value| *value > 0)
                    .ok_or_else(|| {
                        invalid(format!(
                            "{context}.runtimeMaxSec must be a positive integer"
                        ))
                    })?,
            )
        } else {
            None
        };
        let checkpoint = (kind == "checkpoint").then(|| CheckpointBrief {
            campaign_name,
            argv: argv
                .as_deref()
                .expect("checkpoint argv was validated above"),
            runtime_max_sec: runtime_max_sec.expect("checkpoint runtime was validated above"),
            dependencies: &dependencies,
        });
        let mut ownership_lint_inputs = Vec::new();
        if kind == "implementation" {
            if let Some(goal) = item.get("goal").and_then(Value::as_str) {
                ownership_lint_inputs.push(OwnershipLintInput {
                    context: "goal".to_owned(),
                    text: goal.to_owned(),
                });
            }
            if let Some(criteria) = item.get("acceptanceCriteria").and_then(Value::as_array) {
                for (criterion_index, criterion) in criteria.iter().enumerate() {
                    let Some(arguments) = criterion.get("argv").and_then(Value::as_array) else {
                        continue;
                    };
                    for argument in arguments.iter().filter_map(Value::as_str) {
                        ownership_lint_inputs.push(OwnershipLintInput {
                            context: format!("acceptanceCriteria[{criterion_index}].argv"),
                            text: argument.to_owned(),
                        });
                    }
                }
            }
        }
        let body = render_worklist_task_body(item, &context, checkpoint)?;
        if body.chars().count() > 64_000 {
            return Err(invalid(format!(
                "{context} task brief must contain at most 64000 characters"
            )));
        }
        tasks.push(WorklistTask {
            id,
            kind,
            title,
            body,
            ownership_lint_inputs,
            issue,
            dependencies,
            conflict_domains,
            argv,
            runtime_max_sec,
        });
    }
    Ok(tasks)
}

fn worklist_manifest_config(document: &Value, separate: Option<&Value>) -> Result<Value> {
    let config = match separate {
        Some(value) => value.clone(),
        None => document
            .get("campaign")
            .cloned()
            .ok_or_else(|| invalid("worklist requires a campaign object or --campaign-config"))?,
    };
    let mut object = config
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("campaign configuration must be an object"))?;
    object.insert("schemaVersion".to_owned(), json!(CAMPAIGN_SCHEMA_VERSION));
    object.insert("tasks".to_owned(), Value::Array(Vec::new()));
    Ok(Value::Object(object))
}

fn task_references(tasks: &[WorklistTask]) -> Result<Value> {
    Ok(Value::Array(
        tasks
            .iter()
            .map(|task| {
                let mut reference = json!({
                    "id": task.id,
                    "kind": task.kind,
                    "issue": task.issue.ok_or_else(|| invalid(format!("task {} has no task number", task.id)))?,
                    "dependencies": task.dependencies,
                });
                let object = reference.as_object_mut().expect("reference is an object");
                if task.kind == "implementation" {
                    if let Some(domains) = &task.conflict_domains {
                        object.insert("conflictDomains".to_owned(), json!(domains));
                    }
                } else {
                    object.insert("argv".to_owned(), json!(task.argv));
                    object.insert("runtimeMaxSec".to_owned(), json!(task.runtime_max_sec));
                }
                Ok(reference)
            })
            .collect::<Result<Vec<_>>>()?,
    ))
}

fn manifest_value(config: &Value, tasks: &[WorklistTask]) -> Result<Value> {
    let mut value = config.clone();
    value
        .as_object_mut()
        .expect("worklist_manifest_config returns an object")
        .insert("tasks".to_owned(), task_references(tasks)?);
    Ok(value)
}

fn validate_worklist_shape(config: &Value, tasks: &[WorklistTask]) -> Result<CampaignManifest> {
    let mut projected = tasks.to_vec();
    let mut used = projected
        .iter()
        .filter_map(|task| task.issue)
        .collect::<BTreeSet<_>>();
    let mut placeholder = 1u64;
    for task in &mut projected {
        if task.issue.is_none() {
            while used.contains(&placeholder) {
                placeholder = placeholder
                    .checked_add(1)
                    .ok_or_else(|| invalid("campaign task numbering is exhausted"))?;
            }
            task.issue = Some(placeholder);
            used.insert(placeholder);
        }
    }
    let value = manifest_value(config, &projected)?;
    admit_manifest_value(value).map_err(|error| {
        invalid(format!(
            "campaign configuration cannot form a valid manifest: {error}"
        ))
    })
}

/// Parse one JSON worklist into the exact manifest and immutable task content
/// admitted at the campaign boundary.
fn validate_worklist_document(
    document: &Value,
    separate_config: Option<&Value>,
) -> Result<ValidatedWorklist> {
    let raw_config = worklist_manifest_config(document, separate_config)?;
    let campaign_name = raw_config
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| safe_component(value))
        .ok_or_else(|| invalid("campaign configuration name is missing or invalid"))?;
    let tasks = worklist_tasks(document, campaign_name)?;
    let manifest = validate_worklist_shape(&raw_config, &tasks)?;
    Ok(ValidatedWorklist { manifest, tasks })
}

fn require_local_fields(
    object: &serde_json::Map<String, Value>,
    required: &[&str],
    optional: &[&str],
    context: &str,
) -> Result<()> {
    let allowed = required
        .iter()
        .chain(optional)
        .copied()
        .collect::<BTreeSet<_>>();
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(field.as_str()))
    {
        return Err(invalid(format!(
            "{context} contains unsupported field {field:?}"
        )));
    }
    if let Some(field) = required.iter().find(|field| !object.contains_key(**field)) {
        return Err(invalid(format!("{context} is missing field {field:?}")));
    }
    Ok(())
}

fn local_bounded_string<'a>(value: &'a Value, context: &str, maximum: usize) -> Result<&'a str> {
    let text = value
        .as_str()
        .filter(|text| {
            !text.is_empty()
                && text.chars().count() <= maximum
                && !text.chars().any(|character| character < '\u{20}')
        })
        .ok_or_else(|| {
            invalid(format!(
                "{context} must be a non-empty string of at most {maximum} characters without control characters"
            ))
        })?;
    Ok(text)
}

fn local_string_list(value: &Value, context: &str, nonempty: bool) -> Result<Vec<String>> {
    let values = value
        .as_array()
        .filter(|values| !nonempty || !values.is_empty())
        .ok_or_else(|| {
            invalid(format!(
                "{context} must be {} array",
                if nonempty { "a non-empty" } else { "an" }
            ))
        })?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            local_bounded_string(value, &format!("{context}[{index}]"), usize::MAX)
                .map(ToOwned::to_owned)
        })
        .collect()
}

/// The committed local file owns campaign policy and immutable task briefs.
fn validate_local_worklist_document(
    document: &Value,
    manifest_config: &Value,
) -> Result<ValidatedWorklist> {
    let object = document
        .as_object()
        .ok_or_else(|| invalid("local campaign worklist must be an object"))?;
    require_local_fields(
        object,
        &["schemaVersion", "tasks"],
        &["campaign"],
        "local worklist",
    )?;
    let tasks = object
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("local worklist.tasks must be an array"))?;
    for (index, candidate) in tasks.iter().enumerate() {
        let context = format!("tasks[{index}]");
        let item = candidate
            .as_object()
            .ok_or_else(|| invalid(format!("{context} must be an object")))?;
        let kind = item
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(format!("{context}.kind must be a string")))?;
        match kind {
            "checkpoint" => require_local_fields(
                item,
                &[
                    "id",
                    "kind",
                    "title",
                    "argv",
                    "runtimeMaxSec",
                    "dependencies",
                ],
                &[],
                &context,
            )?,
            "implementation" => {
                require_local_fields(
                    item,
                    &[
                        "id",
                        "kind",
                        "title",
                        "goal",
                        "deliveredBehaviors",
                        "readFirst",
                        "acceptanceCriteria",
                        "dependencies",
                    ],
                    &["conflictDomains"],
                    &context,
                )?;
                local_bounded_string(
                    item.get("goal").expect("goal is required above"),
                    &format!("{context}.goal"),
                    12_000,
                )?;
                local_string_list(
                    item.get("deliveredBehaviors")
                        .expect("deliveredBehaviors is required above"),
                    &format!("{context}.deliveredBehaviors"),
                    true,
                )?;
                let read_first = item
                    .get("readFirst")
                    .and_then(Value::as_object)
                    .ok_or_else(|| invalid(format!("{context}.readFirst must be an object")))?;
                require_local_fields(
                    read_first,
                    &["specSections", "styleReferences"],
                    &[],
                    &format!("{context}.readFirst"),
                )?;
                local_string_list(
                    read_first
                        .get("specSections")
                        .expect("specSections is required above"),
                    &format!("{context}.readFirst.specSections"),
                    true,
                )?;
                local_string_list(
                    read_first
                        .get("styleReferences")
                        .expect("styleReferences is required above"),
                    &format!("{context}.readFirst.styleReferences"),
                    false,
                )?;
                let acceptance = item
                    .get("acceptanceCriteria")
                    .and_then(Value::as_array)
                    .filter(|acceptance| !acceptance.is_empty())
                    .ok_or_else(|| {
                        invalid(format!(
                            "{context}.acceptanceCriteria must be a non-empty array"
                        ))
                    })?;
                let mut acceptance_ids = BTreeSet::new();
                for (criterion_index, candidate) in acceptance.iter().enumerate() {
                    let criterion_context =
                        format!("{context}.acceptanceCriteria[{criterion_index}]");
                    let criterion = candidate
                        .as_object()
                        .ok_or_else(|| invalid(format!("{criterion_context} must be an object")))?;
                    require_local_fields(
                        criterion,
                        &["id", "description", "argv"],
                        &[],
                        &criterion_context,
                    )?;
                    let identifier = local_bounded_string(
                        criterion.get("id").expect("id is required above"),
                        &format!("{criterion_context}.id"),
                        80,
                    )?;
                    if !safe_component(identifier) || !acceptance_ids.insert(identifier) {
                        return Err(invalid(format!(
                            "{context}.acceptanceCriteria ids must be safe and unique"
                        )));
                    }
                    local_bounded_string(
                        criterion
                            .get("description")
                            .expect("description is required above"),
                        &format!("{criterion_context}.description"),
                        4_000,
                    )?;
                    let argv = local_string_list(
                        criterion.get("argv").expect("argv is required above"),
                        &format!("{criterion_context}.argv"),
                        true,
                    )?;
                    validate_argv(&argv, &format!("{criterion_context}.argv"))?;
                }
            }
            _ => {
                return Err(invalid(format!(
                    "{context}.kind must equal implementation or checkpoint"
                )))
            }
        }
        local_bounded_string(
            item.get("id").expect("task id is required above"),
            &format!("{context}.id"),
            80,
        )?;
        local_bounded_string(
            item.get("title").expect("task title is required above"),
            &format!("{context}.title"),
            300,
        )?;
    }
    validate_worklist_document(document, Some(manifest_config))
}

/// Where a scaffolded worklist lands when the identity alone names it.
///
/// The fleet genre already proves this directory, and a campaign is armed by a
/// committed pattern rather than by a path handed at run time, so one
/// conventional home keeps the arm line short and the pattern stable.
const SCAFFOLD_WORKLIST_DIRECTORY: &str = "silent-factory-worklists";

/// The marker every value the author must replace is spelled with.
///
/// One greppable marker, named by the printed handoff, is the whole editing
/// contract: no second list of fields to keep in sync with the template.
const SCAFFOLD_EDIT_MARKER: &str = "EDIT-ME";

/// The repository coordinate the handoff prints when the checkout names none.
const SCAFFOLD_CODE_REPOSITORY_PLACEHOLDER: &str = "OWNER/REPO";

/// One scaffolded worklist: rendered, rehearsed, and not yet on disk.
#[derive(Debug, Clone)]
struct ScaffoldedWorklist {
    /// Absolute path the bytes go to.
    target: PathBuf,
    /// The checkout-relative pattern the arm line names.
    pattern: String,
    /// `OWNER/REPO`, read from the checkout's own remote when it spells one.
    code_repository: String,
    campaign_name: String,
    task_id: String,
    /// The exact bytes, already round-tripped through admission's validation.
    bytes: String,
}

impl ScaffoldedWorklist {
    fn arm_argv(&self) -> Vec<String> {
        vec![
            "tally".to_owned(),
            "campaign".to_owned(),
            "arm".to_owned(),
            self.code_repository.clone(),
            self.pattern.clone(),
        ]
    }
}

/// Derive the example task's id from the campaign identity.
///
/// A campaign name is a safe path component; a task id is the stricter
/// lowercase-and-hyphen form. The derivation is the obvious one, and an
/// identity that cannot reach the stricter form is refused here — at the verb
/// that could still be re-run cheaply — rather than at arm.
fn scaffold_task_id(identity: &str) -> Result<String> {
    let mut derived = String::with_capacity(identity.len());
    for character in identity
        .chars()
        .map(|character| character.to_lowercase().next().unwrap_or(character))
    {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            derived.push(character);
        } else if !derived.ends_with('-') {
            derived.push('-');
        }
    }
    let derived = derived.trim_matches('-').to_owned();
    if !safe_task_id(&derived) {
        return Err(invalid(format!(
            "campaign identity {identity:?} derives no task id; use lowercase letters, digits, and hyphens"
        )));
    }
    Ok(derived)
}

/// Render the minimal worklist one identity scaffolds to.
///
/// Minimal means what admission requires and nothing the fleet genre added on
/// top of it: one gate, one implementation task, no spec plane and no citation
/// apparatus. `readFirst.specSections` is non-empty because the local
/// validator requires a non-empty list of strings there, not because a
/// `specs/` tree has to exist — nothing resolves those strings against the
/// filesystem, and an ordinary repository names an ordinary file.
///
/// The two example commands are deliberately real argv that fail: an unedited
/// scaffold must not be able to run green, and the failure names the field to
/// edit. Their text carries no path-shaped token, so an unedited template also
/// arms without advisory conflict-domain warnings.
fn scaffold_worklist_value(identity: &str, task_id: &str) -> Value {
    json!({
        "schemaVersion": 1,
        "campaign": {
            "name": identity,
            "agent": {
                "adapter": "claude-code"
            },
            "gates": [{
                "kind": "command",
                "id": "tests",
                "preflightArgv": ["sh", "-euc", "command -v bash >/dev/null"],
                "argv": [
                    "bash",
                    "-lc",
                    format!("echo '{SCAFFOLD_EDIT_MARKER}: replace this gate with the command every task of this campaign must pass before it merges' >&2; exit 1"),
                ]
            }]
        },
        "tasks": [{
            "id": task_id,
            "kind": "implementation",
            "title": format!("{SCAFFOLD_EDIT_MARKER}: one line naming the change this task delivers"),
            "goal": format!("{SCAFFOLD_EDIT_MARKER}: state what the tree does today, the change this task makes, and the boundary it must not cross. The lane reads this and nothing else about your intent, so state the problem before the remedy and name what stays untouched"),
            "deliveredBehaviors": [
                format!("{SCAFFOLD_EDIT_MARKER}: one behaviour the tree has after this task that it does not have now")
            ],
            "readFirst": {
                "specSections": [
                    format!("{SCAFFOLD_EDIT_MARKER}: one file or note the lane must read before it changes code; an ordinary repository needs no spec plane here")
                ],
                "styleReferences": []
            },
            "acceptanceCriteria": [{
                "id": "tests-pass",
                "description": format!("{SCAFFOLD_EDIT_MARKER}: one sentence saying what the command below proves"),
                "argv": [
                    "bash",
                    "-lc",
                    format!("echo '{SCAFFOLD_EDIT_MARKER}: replace this with the command that proves this task' >&2; exit 1"),
                ]
            }],
            "dependencies": [],
            "conflictDomains": ["src"]
        }]
    })
}

/// Round-trip rendered worklist bytes through the validation admission uses.
///
/// Admission reads the committed blob as JSON, parses the campaign policy,
/// assembles a manifest configuration, and validates the local document
/// against it; this does the same to bytes that are not committed yet, so
/// template drift that arm would refuse fails at the verb and in its tests.
///
/// One binding it deliberately does not perform is the host adapter catalog's:
/// the template names an adapter in that catalog's own vocabulary and the
/// author may edit it before arming, so requiring the name to resolve here
/// would refuse scaffolding on every host but the one that wrote the default.
/// A steward is a catalog binding that cannot be deferred that way, so the
/// rehearsal refuses one outright rather than validating a configuration it
/// silently dropped. Base branch and remote are arm's own defaults; neither
/// participates in worklist validity.
fn rehearse_scaffolded_worklist(
    bytes: &str,
    source_path: &str,
    checkout: &Path,
    code_repository: &str,
) -> Result<ValidatedWorklist> {
    let document: Value = serde_json::from_str(bytes)
        .map_err(|error| invalid(format!("scaffolded worklist is not valid JSON: {error}")))?;
    let policy = parse_worklist_campaign_policy(&document, source_path)?;
    if policy.steward.is_some() {
        return Err(invalid(
            "a scaffolded worklist declares no steward; resolving one needs the host adapter catalog this rehearsal does not read",
        ));
    }
    let name = policy
        .name
        .clone()
        .expect("campaign name was defaulted above");
    let repository = CampaignRepository {
        checkout: checkout.to_path_buf(),
        base_branch: "main".to_owned(),
        remote: "origin".to_owned(),
        forge: "local".to_owned(),
    };
    let pool = format!("{CAMPAIGN_POOL_PREFIX}{code_repository}");
    debug_assert!(is_campaign_pool_name(&pool));
    // No receipts exist for a campaign that has never run, which is exactly
    // the never-fired floor `resolve_gate_budget` already answers with.
    let (gates, _) = resolve_worklist_gate_budgets(&policy.gates, &BTreeMap::new());
    let config = worklist_policy_manifest_config(
        &name,
        &policy,
        &repository,
        &pool,
        policy.agent.clone(),
        None,
        gates,
    );
    validate_local_worklist_document(&document, &config)
}

/// Resolve the Git worktree the scaffolded worklist will be armed from.
fn scaffold_checkout(directory: &Path) -> Result<PathBuf> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(directory)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("cannot execute git while resolving the scaffold checkout")?;
    if !output.status.success() {
        return Err(invalid(format!(
            "{} is not inside a Git worktree; a campaign is armed from a committed worklist, so scaffold inside the checkout that will carry it",
            directory.display()
        )));
    }
    let root = String::from_utf8(output.stdout)
        .context("Git worktree root is not valid UTF-8")?
        .trim_end_matches('\n')
        .to_owned();
    fs::canonicalize(&root).with_context(|| format!("cannot resolve Git worktree root {root}"))
}

/// Read the `OWNER/REPO` identity from the checkout's own remote.
///
/// A local campaign's repository coordinate is an identity, not a fetch
/// target: nothing contacts a forge with it. Reading it from the remote is
/// what makes the printed arm line copy-pasteable on the ordinary case, and a
/// checkout with no remote — or one whose remote is a bare filesystem path —
/// gets the placeholder the handoff then names.
fn scaffold_code_repository(checkout: &Path, remote: &str) -> Option<String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["remote", "get-url", remote])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8(output.stdout).ok()?;
    let url = url.trim();
    if !url.contains("://") && !url.contains('@') {
        return None;
    }
    let url = url.trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let mut parts = url.rsplit(['/', ':']);
    let repository = parts.next()?;
    let owner = parts.next()?;
    parse_repository(&format!("{owner}/{repository}")).ok()
}

/// Render and rehearse one scaffolded worklist without writing anything.
///
/// Splitting the decision from the write keeps every filesystem effect after
/// every refusal, and lets the tests exercise the verb against a fixture
/// checkout without moving the process's working directory.
fn scaffold_worklist(
    directory: &Path,
    identity: &str,
    path: Option<&Path>,
) -> Result<ScaffoldedWorklist> {
    if !safe_component(identity) {
        return Err(invalid(
            "campaign identity must be a safe path component: it becomes the campaign name",
        ));
    }
    let task_id = scaffold_task_id(identity)?;
    // Both sides of the later `strip_prefix` have to be spelled the same way,
    // and the checkout arrives canonical: a relative `--path` is resolved
    // against the canonical directory rather than the logical one.
    let directory = fs::canonicalize(directory)
        .with_context(|| format!("cannot resolve directory {}", directory.display()))?;
    let checkout = scaffold_checkout(&directory)?;
    let target = match path {
        Some(path) => {
            if path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(invalid("--path must not contain '..' components"));
            }
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                directory.join(path)
            }
        }
        None => checkout
            .join(SCAFFOLD_WORKLIST_DIRECTORY)
            .join(format!("{identity}.json")),
    };
    let pattern = target
        .strip_prefix(&checkout)
        .ok()
        .and_then(|relative| relative.to_str())
        .ok_or_else(|| {
            invalid(format!(
                "worklist {} is outside the checkout {} that would arm it",
                target.display(),
                checkout.display()
            ))
        })
        .and_then(parse_worklist_pattern)?;
    if target.exists() {
        return Err(invalid(format!(
            "{} already exists; scaffolding never overwrites an authored worklist",
            target.display()
        )));
    }
    let mut bytes = serde_json::to_string_pretty(&scaffold_worklist_value(identity, &task_id))
        .context("cannot render the scaffolded campaign worklist")?;
    bytes.push('\n');
    let code_repository = scaffold_code_repository(&checkout, "origin")
        .unwrap_or_else(|| SCAFFOLD_CODE_REPOSITORY_PLACEHOLDER.to_owned());
    rehearse_scaffolded_worklist(&bytes, &pattern, &checkout, &code_repository)?;
    Ok(ScaffoldedWorklist {
        target,
        pattern,
        code_repository,
        campaign_name: identity.to_owned(),
        task_id,
        bytes,
    })
}

/// Write the rehearsed bytes, refusing an existing file at the syscall.
fn write_scaffolded_worklist(scaffold: &ScaffoldedWorklist) -> Result<()> {
    if let Some(parent) = scaffold.target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&scaffold.target)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                invalid(format!(
                    "{} already exists; scaffolding never overwrites an authored worklist",
                    scaffold.target.display()
                ))
            } else {
                anyhow::Error::new(error)
                    .context(format!("cannot create {}", scaffold.target.display()))
            }
        })?;
    file.write_all(scaffold.bytes.as_bytes())
        .with_context(|| format!("cannot write {}", scaffold.target.display()))?;
    Ok(())
}

/// The handoff the verb prints so it documents its own next step.
fn scaffold_handoff(scaffold: &ScaffoldedWorklist) -> String {
    let mut lines = vec![
        format!("Wrote {}", scaffold.target.display()),
        String::new(),
        format!(
            "Campaign {:?}, one implementation task {:?}. Replace every value spelled {SCAFFOLD_EDIT_MARKER}, and check:",
            scaffold.campaign_name, scaffold.task_id
        ),
        String::new(),
        "  campaign.agent.adapter    an adapter this host's tally configuration declares".to_owned(),
        "  campaign.gates            the commands that must pass before any task of this campaign merges".to_owned(),
        "  tasks[0].conflictDomains  the paths this task is allowed to write".to_owned(),
        String::new(),
        "Commit the worklist to the base branch, then arm it:".to_owned(),
        String::new(),
        format!("    {}", scaffold.arm_argv().join(" ")),
    ];
    if scaffold.code_repository == SCAFFOLD_CODE_REPOSITORY_PLACEHOLDER {
        lines.extend([
            String::new(),
            format!(
                "This checkout names no remote, so {SCAFFOLD_CODE_REPOSITORY_PLACEHOLDER} above is a placeholder: any stable OWNER/REPO identity will do."
            ),
        ]);
    }
    let mut rendered = lines.join("\n");
    rendered.push('\n');
    rendered
}

fn run_campaign_scaffold(args: CampaignScaffoldArgs) -> Result<()> {
    let directory = std::env::current_dir()
        .context("cannot resolve the current directory for campaign scaffold")?;
    let scaffold = scaffold_worklist(&directory, &args.identity, args.path.as_deref())?;
    write_scaffolded_worklist(&scaffold)?;
    // The compact first line is the scriptable one and human text follows after
    // a blank line, the way `campaign release --plan` already serves both
    // consumers from one rendering path.
    outln!(
        "{}",
        serde_json::to_string(&json!({
            "worklist": scaffold.target,
            "pattern": scaffold.pattern,
            "campaign": scaffold.campaign_name,
            "taskId": scaffold.task_id,
            "armArgv": scaffold.arm_argv(),
        }))?
    );
    outln!();
    outln!("{}", scaffold_handoff(&scaffold).trim_end());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tally_core::campaign_contract::{validate_manifest, BRIEF_SENTINEL};
    use tally_core::campaign_lease::CampaignLeaseAcquisition;
    use tally_core::campaign_publish::PublishProof;
    use tally_core::gate_budget::GateBudgetSource;

    fn manifest_value_for_test(tasks: Value) -> Value {
        json!({
            "schemaVersion": 1,
            "name": "night-build",
            "repository": {
                "checkout": "/tmp/example",
                "baseBranch": "main",
                "remote": "origin",
                "forge": "local"
            },
            "maxTasks": 4,
            "maxParallel": 1,
            "agent": {},
            "gates": [{
                "kind": "command",
                "id": "test",
                "preflightArgv": ["true"],
                "argv": ["true"]
            }],
            "tasks": tasks
        })
    }

    /// A repository with Git in it and nothing else — no spec plane, no
    /// worklist, no commit. The lightweight path has to work here.
    fn bare_scaffold_repository(directory: &Path) -> PathBuf {
        let checkout = fs::canonicalize(directory).unwrap();
        release_fixture_git(&checkout, &["init", "-b", "main"]);
        checkout
    }

    #[test]
    fn scaffold_template_validates_against_the_campaign_contract() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = bare_scaffold_repository(temporary.path());

        let scaffold = scaffold_worklist(&checkout, "night_build", None).unwrap();
        assert_eq!(scaffold.campaign_name, "night_build");
        assert_eq!(scaffold.task_id, "night-build");
        assert_eq!(
            scaffold.pattern,
            "silent-factory-worklists/night_build.json"
        );
        assert_eq!(
            scaffold.arm_argv(),
            [
                "tally",
                "campaign",
                "arm",
                SCAFFOLD_CODE_REPOSITORY_PLACEHOLDER,
                "silent-factory-worklists/night_build.json",
            ]
        );

        // The bytes on disk, not a value the renderer happened to keep: this is
        // what arm would read, so template drift fails here forever.
        write_scaffolded_worklist(&scaffold).unwrap();
        let written = fs::read_to_string(&scaffold.target).unwrap();
        assert_eq!(written, scaffold.bytes);
        let validated = rehearse_scaffolded_worklist(
            &written,
            &scaffold.pattern,
            &checkout,
            &scaffold.code_repository,
        )
        .unwrap();

        validate_manifest(&validated.manifest).unwrap();
        assert_eq!(validated.manifest.name, "night_build");
        assert_eq!(validated.tasks.len(), 1);
        assert_eq!(validated.tasks[0].id, "night-build");
        assert_eq!(validated.tasks[0].kind, "implementation");
        assert_eq!(
            validated.tasks[0].conflict_domains.as_deref(),
            Some(["src".to_owned()].as_slice())
        );
        // maxParallel is the contract's leniency floor: at one, conflictDomains
        // is optional, and the template still declares one boundary to edit.
        assert_eq!(validated.manifest.max_parallel, 1);
        assert!(
            ownership_preflight_warnings(&validated.tasks).is_empty(),
            "an unedited template must arm without advisory ownership warnings"
        );
        assert!(
            validated.tasks[0].body.contains(SCAFFOLD_EDIT_MARKER),
            "the rendered brief must carry the marker the handoff names"
        );
    }

    #[test]
    fn scaffold_worklist_validates_without_a_spec_plane() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = bare_scaffold_repository(temporary.path());
        assert!(
            !checkout.join("specs").exists(),
            "the fixture repository must start without a spec plane"
        );

        let scaffold = scaffold_worklist(&checkout, "ordinary-work", None).unwrap();
        write_scaffolded_worklist(&scaffold).unwrap();
        let written = fs::read_to_string(&scaffold.target).unwrap();
        rehearse_scaffolded_worklist(
            &written,
            &scaffold.pattern,
            &checkout,
            &scaffold.code_repository,
        )
        .unwrap();

        assert!(
            !checkout.join("specs").exists(),
            "scaffolding must neither read nor write a spec plane"
        );
        assert!(
            !written.contains("specs/"),
            "a scaffolded worklist must cite no spec path"
        );
        let mut entries = fs::read_dir(&checkout)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, [".git", SCAFFOLD_WORKLIST_DIRECTORY]);
    }

    #[test]
    fn scaffold_refuses_to_overwrite_an_authored_worklist() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = bare_scaffold_repository(temporary.path());
        let scaffold =
            scaffold_worklist(&checkout, "ordinary-work", Some(Path::new("work.json"))).unwrap();
        assert_eq!(scaffold.pattern, "work.json");
        write_scaffolded_worklist(&scaffold).unwrap();
        fs::write(&scaffold.target, "authored by hand\n").unwrap();

        let refused = scaffold_worklist(&checkout, "ordinary-work", Some(Path::new("work.json")))
            .unwrap_err()
            .to_string();
        assert!(refused.contains("already exists"), "{refused}");
        // The syscall, not the check above, is the guard that cannot race.
        let refused = write_scaffolded_worklist(&scaffold)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("already exists"), "{refused}");
        assert_eq!(
            fs::read_to_string(&scaffold.target).unwrap(),
            "authored by hand\n"
        );

        let outside =
            scaffold_worklist(&checkout, "ordinary-work", Some(Path::new("../work.json")))
                .unwrap_err()
                .to_string();
        assert!(outside.contains("'..'"), "{outside}");
    }

    fn release_summary_ref_for_test(
        reference: String,
        source_sha256: Option<&str>,
        outcome: &str,
    ) -> ReleaseSummaryRef {
        ReleaseSummaryRef {
            reference,
            object_id: "d".repeat(40),
            source_sha256: source_sha256.map(str::to_owned),
            summary: ReleaseClosingSummaryV1 {
                schema_version: RELEASE_SUMMARY_SCHEMA_VERSION,
                kind: "closing-summary".to_owned(),
                campaign: "fixture-release".to_owned(),
                issue_number: LOCAL_CAMPAIGN_ISSUE_NUMBER.to_string(),
                outcome: outcome.to_owned(),
                body: String::new(),
            },
        }
    }

    #[test]
    fn release_closing_summary_resolves_scoped_and_legacy_summary_refs() {
        let state_prefix =
            campaign_state_ref_prefix("fixture-release", LOCAL_CAMPAIGN_ISSUE_NUMBER);
        let source_sha256 = format!("sha256:{}", "a".repeat(64));
        let graph_digest = format!("sha256:{}", "b".repeat(64));
        let source_scoped = release_summary_ref_for_test(
            stage_scoped_summary_ref(&state_prefix, &source_sha256, "complete").unwrap(),
            Some(&source_sha256),
            "complete",
        );
        let graph_scoped = release_summary_ref_for_test(
            stage_scoped_summary_ref(&state_prefix, &graph_digest, "complete").unwrap(),
            Some(&source_sha256),
            "complete",
        );
        let legacy_current = release_summary_ref_for_test(
            format!("{state_prefix}/summary/complete"),
            Some(&source_sha256),
            "complete",
        );
        let legacy_archived = release_summary_ref_for_test(
            format!("{state_prefix}/summary/archive/old-complete"),
            Some(&source_sha256),
            "complete",
        );

        for summary in [
            &source_scoped,
            &graph_scoped,
            &legacy_current,
            &legacy_archived,
        ] {
            assert!(is_release_summary_ref(&summary.reference, &state_prefix));
        }
        assert!(!is_release_summary_ref(
            &format!("{state_prefix}/merge/not-a-summary"),
            &state_prefix
        ));

        for summary in [
            &source_scoped,
            &graph_scoped,
            &legacy_current,
            &legacy_archived,
        ] {
            let resolved = release_closing_summary(
                std::slice::from_ref(summary),
                &state_prefix,
                &source_sha256,
                &graph_digest,
            )
            .unwrap();
            assert_eq!(resolved.reference, summary.reference);
        }

        let summaries = vec![
            legacy_current.clone(),
            source_scoped.clone(),
            graph_scoped.clone(),
        ];
        assert_eq!(
            release_closing_summary(&summaries, &state_prefix, &source_sha256, &graph_digest,)
                .unwrap()
                .reference,
            graph_scoped.reference,
            "the exact admitted-graph ref wins when compatibility copies coexist"
        );
    }

    #[test]
    fn release_artifacts_collect_scoped_and_legacy_summary_refs() {
        let state_prefix =
            campaign_state_ref_prefix("fixture-release", LOCAL_CAMPAIGN_ISSUE_NUMBER);
        let source_sha256 = format!("sha256:{}", "a".repeat(64));
        let historical_digest = format!("sha256:{}", "b".repeat(64));
        let closing = release_summary_ref_for_test(
            stage_scoped_summary_ref(&state_prefix, &source_sha256, "complete").unwrap(),
            Some(&source_sha256),
            "complete",
        );
        let scoped = release_summary_ref_for_test(
            stage_scoped_summary_ref(&state_prefix, &historical_digest, "quiescent").unwrap(),
            None,
            "quiescent",
        );
        let legacy_current = release_summary_ref_for_test(
            format!("{state_prefix}/summary/quiescent"),
            None,
            "quiescent",
        );
        let legacy_archived = release_summary_ref_for_test(
            format!("{state_prefix}/summary/archive/old-quiescent"),
            None,
            "quiescent",
        );
        let summaries = vec![
            closing.clone(),
            scoped.clone(),
            legacy_current.clone(),
            legacy_archived.clone(),
        ];
        let artifacts = release_artifacts(
            &ReleaseGitRef {
                object_id: "c".repeat(40),
                object_type: "commit".to_owned(),
                tree_id: Some("e".repeat(40)),
                reference: "refs/heads/tally/fixture/integration".to_owned(),
            },
            &[],
            &[],
            &[],
            &closing,
            &summaries,
            &ReleaseAttemptLog {
                path: PathBuf::new(),
                present: false,
                bytes: Vec::new(),
                records: Vec::new(),
                witnesses: Vec::new(),
            },
        );
        let archived = artifacts
            .iter()
            .filter(|artifact| artifact.kind == "archived-summary")
            .map(|artifact| artifact.locator.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            archived,
            BTreeSet::from([
                scoped.reference.as_str(),
                legacy_current.reference.as_str(),
                legacy_archived.reference.as_str(),
            ])
        );
    }

    #[test]
    fn release_note_derivation_falls_back_for_a_poisoned_trailer_block() {
        let scopes = BTreeSet::from(["crates/tally".to_owned()]);
        let message = "feat(crates/tally): parse a plausible header\n\nTally-Task: task\npoison the apparent trailer block\nTally-Revision: sha256:abc";

        assert_eq!(
            validated_release_header(message, &scopes, "Template release note"),
            (
                "other".to_owned(),
                None,
                false,
                "Template release note".to_owned()
            )
        );
    }

    #[test]
    fn truncated_release_attempt_log_fails_loud_with_its_repair() {
        let temporary = tempfile::tempdir().unwrap();
        let campaign = "fixture-release";
        let path = local_attempt_receipts_path(temporary.path(), campaign).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, br#"{"schemaVersion":1,"sequence":1"#).unwrap();

        let error =
            read_release_attempt_log(temporary.path(), campaign, LOCAL_CAMPAIGN_ISSUE_NUMBER)
                .unwrap_err()
                .to_string();
        assert!(error.contains(&path.display().to_string()), "{error}");
        assert!(error.contains("truncated final record"), "{error}");
        assert!(
            error.contains("repair the durable log before rendering a release"),
            "{error}"
        );
    }

    #[test]
    fn missing_release_checkpoint_fails_loud_with_its_restore_action() {
        let temporary = tempfile::tempdir().unwrap();
        release_fixture_git(temporary.path(), &["init", "-b", "main"]);
        let (graph, _) = adversarial_release_graph(temporary.path());
        let source_sha256 = format!("sha256:{}", "a".repeat(64));
        let revision = "b".repeat(40);
        let only_gate = ReleaseCheckpoint {
            task_id: "release-gate".to_owned(),
            reference: "refs/tally/fixture/checkpoint/release-gate".to_owned(),
            revision,
            source_sha256: source_sha256.clone(),
        };

        let error = release_current_checkpoints(&graph, &[only_gate], &source_sha256)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("missing checkpoint ref for task \"smoke\""),
            "{error}"
        );
        assert!(
            error.contains("restore its durable checkpoint ref before rendering a release"),
            "{error}"
        );
    }

    #[test]
    fn release_completion_oracle_rejects_git_trailer_poisoning() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("repository");
        fs::create_dir_all(&checkout).unwrap();
        release_fixture_git(&checkout, &["init", "-b", "main"]);
        release_fixture_git(&checkout, &["config", "user.name", "Fixture"]);
        release_fixture_git(
            &checkout,
            &["config", "user.email", "fixture@example.invalid"],
        );
        release_fixture_git(
            &checkout,
            &["commit", "--allow-empty", "-m", "base: fixture"],
        );
        let base = release_fixture_git(&checkout, &["rev-parse", "HEAD"]);
        let (graph, revision) = adversarial_release_graph(&checkout);

        let valid = format!(
            "ship-feature: fixture\n\nTally-Task: ship-feature\nTally-Revision: {revision}\nAssisted-by: fixture"
        );
        release_fixture_git(&checkout, &["commit", "--allow-empty", "-m", &valid]);
        let history = release_integration_history(&checkout, "HEAD").unwrap();
        let revisions = graph_completion_revisions(&graph).unwrap();
        let merged = release_merged_commits(&graph, &revisions, &history, &[]).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].oracle, ReleaseCompletionOracle::Exact);
        assert_eq!(merged[0].bridge_ref, None);

        let poisoned = [
            (
                "case-folded keys",
                format!(
                    "ship-feature: fixture\n\ntally-task: ship-feature\ntally-revision: {revision}"
                ),
            ),
            (
                "completion pair behind another trailer",
                format!(
                    "ship-feature: fixture\n\nAssisted-by: fixture\nTally-Task: ship-feature\nTally-Revision: {revision}"
                ),
            ),
            (
                "poisoned trailer paragraph",
                format!(
                    "ship-feature: fixture\n\nTally-Task: ship-feature\npoison\nTally-Revision: {revision}"
                ),
            ),
            (
                "split completion pair",
                format!(
                    "ship-feature: fixture\n\nTally-Task: ship-feature\n\nTally-Revision: {revision}"
                ),
            ),
            (
                "duplicate completion claim",
                format!(
                    "ship-feature: fixture\n\nTally-Task: ship-feature\nTally-Revision: {revision}\nTally-Task: ship-feature"
                ),
            ),
        ];
        for (case, message) in poisoned {
            release_fixture_git(&checkout, &["checkout", "--detach", &base]);
            release_fixture_git(&checkout, &["commit", "--allow-empty", "-m", &message]);
            let history = release_integration_history(&checkout, "HEAD").unwrap();
            let error = release_merged_commits(&graph, &revisions, &history, &[])
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("is missing the Tally-Task: ship-feature / Tally-Revision:"),
                "{case} unexpectedly reached the completion oracle: {error}"
            );
        }
    }

    #[test]
    fn release_completion_bridge_accepts_a_legacy_revision_with_a_matching_task_ref() {
        let fixture = release_completion_bridge_fixture();
        let history = release_integration_history(&fixture.checkout, "HEAD").unwrap();
        let refs =
            release_local_refs(&fixture.checkout, &["refs/heads/tally/".to_owned()]).unwrap();

        let merged =
            release_merged_commits(&fixture.graph, &fixture.revisions, &history, &refs).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].task_id, "ship-feature");
        assert_eq!(merged[0].commit.object_id, fixture.legacy_commit);
        assert_eq!(merged[0].oracle, ReleaseCompletionOracle::Bridge);
        assert_eq!(
            merged[0].bridge_ref.as_deref(),
            Some(fixture.legacy_ref.as_str())
        );

        let proofs = release_completion_proofs(&merged);
        assert_eq!(
            serde_json::to_value(&proofs[0]).unwrap(),
            json!({
                "taskId": "ship-feature",
                "commit": fixture.legacy_commit,
                "oracle": "bridge",
                "reference": fixture.legacy_ref
            })
        );
    }

    #[test]
    fn release_completion_bridge_without_a_task_ref_keeps_the_exact_missing_error() {
        let fixture = release_completion_bridge_fixture();
        let history = release_integration_history(&fixture.checkout, "HEAD").unwrap();
        let expected = fixture.revisions.get("ship-feature").unwrap();

        let error = release_merged_commits(&fixture.graph, &fixture.revisions, &history, &[])
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!(
                "completed campaign is missing the {TALLY_TASK_PREFIX} ship-feature / {TALLY_REVISION_PREFIX} {expected} trailer proof"
            )
        );
    }

    #[test]
    fn release_completion_exact_match_never_reports_the_bridge() {
        let fixture = release_completion_bridge_fixture();
        let revision = fixture.revisions.get("ship-feature").unwrap();
        let exact_message = format!(
            "ship-feature: exact fixture\n\n{TALLY_TASK_PREFIX} ship-feature\n{TALLY_REVISION_PREFIX} {revision}"
        );
        release_fixture_git(
            &fixture.checkout,
            &["commit", "--allow-empty", "-m", &exact_message],
        );
        let exact_commit = release_fixture_git(&fixture.checkout, &["rev-parse", "HEAD"]);
        let history = release_integration_history(&fixture.checkout, "HEAD").unwrap();
        let refs =
            release_local_refs(&fixture.checkout, &["refs/heads/tally/".to_owned()]).unwrap();

        let merged =
            release_merged_commits(&fixture.graph, &fixture.revisions, &history, &refs).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].commit.object_id, exact_commit);
        assert_eq!(merged[0].oracle, ReleaseCompletionOracle::Exact);
        assert_eq!(merged[0].bridge_ref, None);
        assert_eq!(
            serde_json::to_value(&release_completion_proofs(&merged)[0]).unwrap(),
            json!({
                "taskId": "ship-feature",
                "commit": exact_commit,
                "oracle": "exact"
            })
        );
    }

    #[test]
    fn release_execute_resumes_from_the_local_record_through_an_injected_program() {
        let temporary = tempfile::tempdir().unwrap();
        let state_dir = temporary.path().join("state");
        let calls = temporary.path().join("gh-calls");
        let count = temporary.path().join("gh-count");
        let fail_on = temporary.path().join("gh-fail-on");
        fs::write(&fail_on, "2\n").unwrap();
        let shim = release_recording_gh(temporary.path(), &calls, &count, &fail_on);
        let config = CampaignReleaseExecutionConfig::resolve(Some(shim.clone())).unwrap();
        let plan = release_execution_plan_for_test();

        let failure = execute_campaign_release(&state_dir, &plan, &config)
            .unwrap_err()
            .to_string();
        assert!(
            failure.contains("publishing the release notes"),
            "{failure}"
        );
        assert!(failure.contains("injected failure 2"), "{failure}");

        let release_directory =
            campaign_release_directory(&state_dir, &plan.registration_id).unwrap();
        let record_path = release_directory.join(RELEASE_RECORD_FILE);
        let partial = read_campaign_release_record(&record_path).unwrap().unwrap();
        assert_eq!(
            partial.steps,
            CampaignReleaseStepsV1 {
                tag: true,
                release_notes: false,
                artifacts: false,
            }
        );

        let resumed = execute_campaign_release(&state_dir, &plan, &config).unwrap();
        assert_eq!(resumed.executed_steps, ["release-notes", "artifacts"]);
        assert_eq!(resumed.skipped_steps, ["tag"]);
        let complete = read_campaign_release_record(&record_path).unwrap().unwrap();
        assert_eq!(
            complete.steps,
            CampaignReleaseStepsV1 {
                tag: true,
                release_notes: true,
                artifacts: true,
            }
        );

        let repeated = execute_campaign_release(&state_dir, &plan, &config).unwrap();
        assert!(repeated.executed_steps.is_empty());
        assert_eq!(
            repeated.skipped_steps,
            ["tag", "release-notes", "artifacts"]
        );
        let recorded = fs::read_to_string(&calls).unwrap();
        let call_lines = recorded.lines().collect::<Vec<_>>();
        assert_eq!(call_lines.len(), 4, "{recorded}");
        assert!(
            call_lines[0].starts_with("api\t--method\tPOST\trepos/acme/widgets/git/refs"),
            "{recorded}"
        );
        assert!(
            call_lines[1].starts_with("release\tcreate\t0.0.0+fixture"),
            "{recorded}"
        );
        assert_eq!(
            call_lines[1], call_lines[2],
            "the failed notes step must be retried"
        );
        assert!(
            call_lines[3].starts_with("release\tupload\t0.0.0+fixture"),
            "{recorded}"
        );
        assert!(
            call_lines
                .iter()
                .all(|call| !call.contains("view") && !call.contains("GET")),
            "release idempotency must not inspect public release text: {recorded}"
        );
        assert!(release_directory.join(RELEASE_NOTES_FILE).is_file());
        assert!(release_directory.join(RELEASE_ARTIFACTS_FILE).is_file());

        let mut changed = plan.clone();
        changed.artifacts.push(CampaignReleaseArtifact {
            kind: "late".to_owned(),
            locator: "local://late".to_owned(),
            object_id: None,
            sha256: None,
            bytes: None,
        });
        let mismatch = execute_campaign_release(&state_dir, &changed, &config)
            .unwrap_err()
            .to_string();
        assert!(mismatch.contains("disagrees with the current release plan"));
        assert_eq!(fs::read_to_string(&calls).unwrap(), recorded);
    }

    #[test]
    fn every_release_step_failure_resumes_at_the_first_incomplete_step() {
        let step_names = ["tag", "release-notes", "artifacts"];
        for failed_call in 1..=3 {
            let temporary = tempfile::tempdir().unwrap();
            let state_dir = temporary.path().join("state");
            let calls = temporary.path().join("gh-calls");
            let count = temporary.path().join("gh-count");
            let fail_on = temporary.path().join("gh-fail-on");
            fs::write(&fail_on, format!("{failed_call}\n")).unwrap();
            let shim = release_recording_gh(temporary.path(), &calls, &count, &fail_on);
            let config = CampaignReleaseExecutionConfig::resolve(Some(shim)).unwrap();
            let plan = release_execution_plan_for_test();

            assert!(execute_campaign_release(&state_dir, &plan, &config).is_err());
            let resumed = execute_campaign_release(&state_dir, &plan, &config).unwrap();
            assert_eq!(
                resumed.skipped_steps,
                step_names[..failed_call - 1],
                "failure at forge call {failed_call}"
            );
            assert_eq!(
                resumed.executed_steps,
                step_names[failed_call - 1..],
                "failure at forge call {failed_call}"
            );
            let calls_after_resume = fs::read_to_string(&calls).unwrap();
            assert_eq!(
                calls_after_resume.lines().count(),
                4,
                "failure at forge call {failed_call}: {calls_after_resume}"
            );

            let repeated = execute_campaign_release(&state_dir, &plan, &config).unwrap();
            assert!(repeated.executed_steps.is_empty());
            assert_eq!(repeated.skipped_steps, step_names);
            assert_eq!(fs::read_to_string(&calls).unwrap(), calls_after_resume);
        }
    }

    #[test]
    fn release_execute_crash_child() {
        if std::env::var(TEST_RELEASE_CRASH_CHILD_ENV).as_deref() != Ok("1") {
            return;
        }
        let state_dir = PathBuf::from(
            std::env::var_os(TEST_RELEASE_CRASH_STATE_ENV)
                .expect("crash child state directory is set"),
        );
        let gh_program = PathBuf::from(
            std::env::var_os(TEST_RELEASE_CRASH_GH_ENV).expect("crash child forge program is set"),
        );
        let config = CampaignReleaseExecutionConfig::resolve(Some(gh_program)).unwrap();
        let plan = release_execution_plan_for_test();
        let _ = execute_campaign_release(&state_dir, &plan, &config);
        panic!("release crash injection returned instead of terminating the child process");
    }

    #[test]
    fn process_death_after_every_persisted_release_step_resumes_without_repeating_it() {
        use std::os::unix::process::ExitStatusExt as _;

        let step_names = ["tag", "release-notes", "artifacts"];
        for (index, crashed_after) in step_names.iter().enumerate() {
            let temporary = tempfile::tempdir().unwrap();
            let state_dir = temporary.path().join("state");
            let calls = temporary.path().join("gh-calls");
            let count = temporary.path().join("gh-count");
            let fail_on = temporary.path().join("gh-fail-on");
            let shim = release_recording_gh(temporary.path(), &calls, &count, &fail_on);

            let child = ProcessCommand::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cli::campaign::tests::release_execute_crash_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(TEST_RELEASE_CRASH_CHILD_ENV, "1")
                .env(TEST_RELEASE_CRASH_AFTER_ENV, crashed_after)
                .env(TEST_RELEASE_CRASH_STATE_ENV, &state_dir)
                .env(TEST_RELEASE_CRASH_GH_ENV, &shim)
                .output()
                .unwrap();
            assert_eq!(
                child.status.signal(),
                Some(libc::SIGABRT),
                "crash after {crashed_after} did not abort the isolated release process: {:?}\nstdout:\n{}\nstderr:\n{}",
                child.status,
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );

            let plan = release_execution_plan_for_test();
            let release_directory =
                campaign_release_directory(&state_dir, &plan.registration_id).unwrap();
            let record = read_campaign_release_record(&release_directory.join(RELEASE_RECORD_FILE))
                .unwrap()
                .unwrap();
            assert!(record.steps.tag);
            assert_eq!(record.steps.release_notes, index >= 1);
            assert_eq!(record.steps.artifacts, index >= 2);
            let recorded_before_resume = fs::read_to_string(&calls).unwrap();
            assert_eq!(recorded_before_resume.lines().count(), index + 1);

            let config = CampaignReleaseExecutionConfig::resolve(Some(shim)).unwrap();
            let resumed = execute_campaign_release(&state_dir, &plan, &config).unwrap();
            assert_eq!(resumed.skipped_steps, step_names[..=index]);
            assert_eq!(resumed.executed_steps, step_names[index + 1..]);
            let recorded_after_resume = fs::read_to_string(&calls).unwrap();
            assert_eq!(recorded_after_resume.lines().count(), step_names.len());

            let repeated = execute_campaign_release(&state_dir, &plan, &config).unwrap();
            assert!(repeated.executed_steps.is_empty());
            assert_eq!(repeated.skipped_steps, step_names);
            assert_eq!(fs::read_to_string(&calls).unwrap(), recorded_after_resume);
        }
    }

    #[test]
    fn release_gh_program_is_explicit_and_defaults_only_to_path_gh() {
        assert_eq!(
            CampaignReleaseExecutionConfig::resolve(None)
                .unwrap()
                .gh_program,
            PathBuf::from("gh")
        );
        let parsed = Opts::try_parse_from([
            "tally",
            "campaign",
            "release",
            "acme/widgets",
            "specs/release.json",
            "--gh-program",
            "/tmp/recording-gh",
        ])
        .unwrap();
        let Some(Command::Campaign {
            command: CampaignCommand::Release(args),
        }) = parsed.command
        else {
            panic!("release command did not parse")
        };
        assert_eq!(args.gh_program, Some(PathBuf::from("/tmp/recording-gh")));
        assert!(!args.probe);

        let probe = Opts::try_parse_from([
            "tally",
            "campaign",
            "release",
            "acme/widgets",
            "specs/release.json",
            "--probe",
        ])
        .unwrap();
        assert!(matches!(
            probe.command,
            Some(Command::Campaign {
                command: CampaignCommand::Release(CampaignReleaseArgs {
                    probe: true,
                    plan: false,
                    ..
                })
            })
        ));
        assert!(Opts::try_parse_from([
            "tally",
            "campaign",
            "release",
            "acme/widgets",
            "specs/release.json",
            "--plan",
            "--probe",
        ])
        .is_err());
    }

    fn release_execution_plan_for_test() -> CampaignReleasePlan {
        let revision = "a".repeat(40);
        CampaignReleasePlan {
            schema_version: RELEASE_PLAN_SCHEMA_VERSION,
            mode: "plan",
            campaign: "fixture-release".to_owned(),
            registration_id: "0198a62b-41ee-7000-8000-000000000777".to_owned(),
            repository: "acme/widgets".to_owned(),
            worklist: "specs/release.json".to_owned(),
            version: "0.0.0+fixture".to_owned(),
            revision: revision.clone(),
            integration_ref: "refs/heads/tally/fixture/integration".to_owned(),
            closing_summary: CampaignReleaseSummaryProof {
                reference: "refs/tally/campaign/fixture/summary/complete".to_owned(),
                object_id: "b".repeat(40),
            },
            coverage: None,
            completion_proofs: vec![CampaignReleaseCompletionProof {
                task_id: "ship-feature".to_owned(),
                commit: revision.clone(),
                oracle: ReleaseCompletionOracle::Exact,
                reference: None,
            }],
            release_notes: vec![CampaignReleaseNote {
                task_id: "ship-feature".to_owned(),
                commit: revision.clone(),
                source_ref: Some("refs/heads/tally/fixture/ship-feature".to_owned()),
                source_commit: Some(revision.clone()),
                header: "feat(crates/tally): ship fixture releases".to_owned(),
                kind: "feat".to_owned(),
                scope: Some("crates/tally".to_owned()),
                breaking: false,
                summary: "ship fixture releases".to_owned(),
            }],
            gate_proof: CampaignReleaseGateProof {
                task_id: "release-gate".to_owned(),
                reference: "refs/tally/campaign/fixture/checkpoint/release-gate".to_owned(),
                revision: revision.clone(),
            },
            artifacts: vec![CampaignReleaseArtifact {
                kind: "integration".to_owned(),
                locator: "refs/heads/tally/fixture/integration".to_owned(),
                object_id: Some(revision.clone()),
                sha256: None,
                bytes: None,
            }],
            digest: serde_json::from_value(json!({
                "schemaVersion": 1,
                "campaign": "fixture-release",
                "repository": "acme/widgets",
                "outcome": "complete",
                "source": {
                    "path": "specs/release.json",
                    "sha256": format!("sha256:{}", "c".repeat(64)),
                    "revision": revision,
                },
                "baseRevision": "a".repeat(40),
                "taskCount": 1,
                "merged": [],
                "checkpoints": [],
                "blocked": [],
                "outstanding": [],
                "steering": [],
                "retries": [],
                "deferrals": [],
                "warnings": [],
            }))
            .unwrap(),
            campaign_summary: "Campaign complete.\n".to_owned(),
        }
    }

    fn adversarial_release_graph(checkout: &Path) -> (CanonicalCampaignGraphV1, String) {
        let manifest = admit_manifest_value(json!({
            "schemaVersion": 1,
            "name": "fixture-release",
            "repository": {
                "checkout": checkout,
                "baseBranch": "main",
                "remote": "origin",
                "forge": "local"
            },
            "maxTasks": 4,
            "maxParallel": 1,
            "agent": {},
            "gates": [{
                "kind": "command",
                "id": "test",
                "preflightArgv": ["true"],
                "argv": ["true"]
            }],
            "tasks": [{
                "id": "ship-feature",
                "kind": "implementation",
                "issue": 1,
                "dependencies": [],
                "conflictDomains": ["crates/tally"]
            }, {
                "id": "smoke",
                "kind": "checkpoint",
                "issue": 2,
                "dependencies": ["ship-feature"],
                "argv": ["true"],
                "runtimeMaxSec": 30
            }, {
                "id": "release-gate",
                "kind": "checkpoint",
                "issue": 3,
                "dependencies": ["ship-feature", "smoke"],
                "argv": ["true"],
                "runtimeMaxSec": 30
            }]
        }))
        .unwrap();
        let graph = CanonicalCampaignGraphV1::new(
            manifest,
            vec![
                CanonicalCampaignTaskV1 {
                    number: 1,
                    title: "Ship the fixture feature".to_owned(),
                    body: "Implement the fixture release surface.".to_owned(),
                },
                CanonicalCampaignTaskV1 {
                    number: 2,
                    title: "Smoke the fixture release".to_owned(),
                    body: "Run the fixture smoke checkpoint.".to_owned(),
                },
                CanonicalCampaignTaskV1 {
                    number: 3,
                    title: "Prove the fixture release".to_owned(),
                    body: "Run the fixture release gate.".to_owned(),
                },
            ],
        )
        .unwrap();
        let revision =
            task_completion_revision(&graph.manifest, &graph.manifest.tasks[0], &graph.tasks[0])
                .unwrap();
        (graph, revision)
    }

    struct ReleaseCompletionBridgeFixture {
        _temporary: tempfile::TempDir,
        checkout: PathBuf,
        graph: CanonicalCampaignGraphV1,
        revisions: BTreeMap<String, String>,
        legacy_commit: String,
        legacy_ref: String,
    }

    fn release_completion_bridge_fixture() -> ReleaseCompletionBridgeFixture {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("repository");
        fs::create_dir_all(&checkout).unwrap();
        release_fixture_git(&checkout, &["init", "-b", "main"]);
        release_fixture_git(&checkout, &["config", "user.name", "Fixture"]);
        release_fixture_git(
            &checkout,
            &["config", "user.email", "fixture@example.invalid"],
        );
        fs::write(checkout.join("base.txt"), "base\n").unwrap();
        release_fixture_git(&checkout, &["add", "base.txt"]);
        release_fixture_git(&checkout, &["commit", "-m", "chore: fixture base"]);
        let base = release_fixture_git(&checkout, &["rev-parse", "HEAD"]);
        let (graph, _) = adversarial_release_graph(&checkout);
        let revisions = graph_completion_revisions(&graph).unwrap();
        let legacy_revision = format!("sha256:{}", "b".repeat(64));
        assert_ne!(
            revisions.get("ship-feature"),
            Some(&legacy_revision),
            "the bridge fixture needs distinct revision identities"
        );
        let task_branch = stable_publish_branch(
            "fixture-release",
            "0197a62b-41ee-7000-8000-000000000111",
            "ship-feature",
            Some(&legacy_revision),
        );
        let task_ref = format!("refs/heads/{task_branch}");
        release_fixture_git(&checkout, &["checkout", "-b", &task_branch, &base]);
        fs::write(checkout.join("feature.txt"), "legacy proof\n").unwrap();
        release_fixture_git(&checkout, &["add", "feature.txt"]);
        release_fixture_git(
            &checkout,
            &["commit", "-m", "feat(crates/tally): publish legacy fixture"],
        );
        let task_tree = release_fixture_git(&checkout, &["rev-parse", "HEAD^{tree}"]);

        release_fixture_git(&checkout, &["checkout", "--detach", &base]);
        fs::write(checkout.join("feature.txt"), "legacy proof\n").unwrap();
        release_fixture_git(&checkout, &["add", "feature.txt"]);
        let message = format!(
            "ship-feature: legacy fixture\n\n{TALLY_TASK_PREFIX} ship-feature\n{TALLY_REVISION_PREFIX} {legacy_revision}"
        );
        release_fixture_git(&checkout, &["commit", "-m", &message]);
        let legacy_commit = release_fixture_git(&checkout, &["rev-parse", "HEAD"]);
        assert_eq!(
            release_fixture_git(&checkout, &["rev-parse", "HEAD^{tree}"]),
            task_tree,
            "the durable task ref must expose the integrated snapshot"
        );

        ReleaseCompletionBridgeFixture {
            _temporary: temporary,
            checkout,
            graph,
            revisions,
            legacy_commit,
            legacy_ref: task_ref,
        }
    }

    fn release_recording_gh(
        directory: &Path,
        calls: &Path,
        count: &Path,
        fail_on: &Path,
    ) -> PathBuf {
        const SHELL_COMMAND_PROVIDER: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test/fixtures/shell-command-provider"
        );
        let program = directory.join("recording-gh");
        let repository_list = directory.join("gh-repository-list.json");
        let mut source = OsString::from(program.as_os_str());
        source.push(".tally-test-script");
        fs::write(
            PathBuf::from(source),
            format!(
                r#"#!/bin/sh
set -eu
count=0
if test -f '{count}'; then count=$(cat '{count}'); fi
count=$((count + 1))
printf '%s\n' "$count" > '{count}'
command_name="$1"
subcommand="${{2:-}}"
printf '%s' "$1" >> '{calls}'
shift
for argument in "$@"; do printf '\t%s' "$argument" >> '{calls}'; done
printf '\n' >> '{calls}'
if test -f '{fail_on}' && test "$(cat '{fail_on}' | tr -d '\n')" = "$count"; then
  printf 'injected failure %s\n' "$count" >&2
  exit 23
fi
if test "$command_name" = repo && test "$subcommand" = list && test -f '{repository_list}'; then
  cat '{repository_list}'
fi
"#,
                calls = calls.display(),
                count = count.display(),
                fail_on = fail_on.display(),
                repository_list = repository_list.display(),
            ),
        )
        .unwrap();
        std::os::unix::fs::symlink(SHELL_COMMAND_PROVIDER, &program).unwrap();
        program
    }

    #[test]
    fn release_probe_ttl_sweep_deletes_only_expired_private_non_forks() {
        let temporary = tempfile::tempdir().unwrap();
        let calls = temporary.path().join("gh-calls");
        let count = temporary.path().join("gh-count");
        let fail_on = temporary.path().join("gh-fail-on");
        fs::write(
            temporary.path().join("gh-repository-list.json"),
            serde_json::to_vec(&json!([{
                "nameWithOwner": "acme/tally-probe-20260801-expired1",
                "createdAt": "2026-08-01T00:00:00Z",
                "isFork": false,
                "isPrivate": true
            }, {
                "nameWithOwner": "acme/tally-probe-20260812-current1",
                "createdAt": "2026-08-12T00:00:00Z",
                "isFork": false,
                "isPrivate": true
            }, {
                "nameWithOwner": "acme/product-repository",
                "createdAt": "2020-01-01T00:00:00Z",
                "isFork": false,
                "isPrivate": true
            }]))
            .unwrap(),
        )
        .unwrap();
        let shim = release_recording_gh(temporary.path(), &calls, &count, &fail_on);
        let config = CampaignReleaseExecutionConfig::resolve(Some(shim)).unwrap();
        let now = DateTime::parse_from_rfc3339("2026-08-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(
            sweep_expired_campaign_release_probes(&config, "acme/widgets", now).unwrap(),
            1
        );
        let recorded = fs::read_to_string(calls).unwrap();
        let calls = recorded.lines().collect::<Vec<_>>();
        assert_eq!(calls.len(), 2, "{recorded}");
        assert!(calls[0].starts_with("repo\tlist\tacme\t"));
        assert_eq!(
            calls[1],
            "repo\tdelete\tacme/tally-probe-20260801-expired1\t--yes"
        );

        assert!(
            validate_campaign_release_probe_repository("acme/widgets", "acme/widgets").is_err()
        );
        assert!(validate_campaign_release_probe_repository(
            "acme/widgets",
            "other/tally-probe-20260814-abcdef12"
        )
        .is_err());
    }

    #[test]
    fn failed_release_probe_is_torn_down_and_writes_a_failure_receipt() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("repository");
        let state_dir = temporary.path().join("state");
        fs::create_dir_all(&checkout).unwrap();
        release_fixture_git(&checkout, &["init", "-b", "main"]);
        release_fixture_git(&checkout, &["config", "user.name", "Fixture"]);
        release_fixture_git(
            &checkout,
            &["config", "user.email", "fixture@example.invalid"],
        );
        fs::write(checkout.join("probe.txt"), "probe\n").unwrap();
        release_fixture_git(&checkout, &["add", "probe.txt"]);
        release_fixture_git(&checkout, &["commit", "-m", "test: seed release probe"]);

        let calls = temporary.path().join("gh-calls");
        let count = temporary.path().join("gh-count");
        let fail_on = temporary.path().join("gh-fail-on");
        fs::write(&fail_on, "4\n").unwrap();
        let shim = release_recording_gh(temporary.path(), &calls, &count, &fail_on);
        let config = CampaignReleaseExecutionConfig::resolve(Some(shim)).unwrap();
        let mut plan = release_execution_plan_for_test();
        plan.revision = release_fixture_git(&checkout, &["rev-parse", "HEAD"]);

        let error = execute_campaign_release_probe(&state_dir, &checkout, &plan, &config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("receipt written to"), "{error}");
        let recorded = fs::read_to_string(calls).unwrap();
        let calls = recorded.lines().collect::<Vec<_>>();
        assert_eq!(calls.len(), 5, "{recorded}");
        assert!(calls[3].starts_with("release\tcreate\t"), "{recorded}");
        assert!(calls[4].starts_with("repo\tdelete\t"), "{recorded}");

        let probes = campaign_release_directory(&state_dir, &plan.registration_id)
            .unwrap()
            .join("probes");
        let probe_directories = fs::read_dir(probes)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(probe_directories.len(), 1);
        let probe_directory = &probe_directories[0];
        assert!(!probe_directory.join(".source").exists());
        let receipt: Value = serde_json::from_slice(
            &fs::read(probe_directory.join(RELEASE_PROBE_RECEIPT_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["status"], "failed");
        assert_eq!(receipt["repositoryCreated"], true);
        assert_eq!(receipt["releaseComplete"], false);
        assert_eq!(receipt["teardownComplete"], true);
        assert!(receipt["failure"]
            .as_str()
            .unwrap()
            .contains("injected failure 4"));
    }

    /// The unified contract, seen from the release side: a commit stamped
    /// with the writer's tuple is proven exactly, and the bridge stays home.
    #[test]
    fn an_exact_writer_tuple_match_revives_completion_without_the_bridge() {
        let fixture = completed_release_fixture();
        let plan = render_campaign_release_plan(
            &fixture.state_dir,
            RELEASE_FIXTURE_REPOSITORY,
            RELEASE_FIXTURE_WORKLIST,
        )
        .unwrap();

        // The trailer the fixture's integration commit carries is the writer's
        // tuple this graph computes -- the same identity `campaign.rs` hands
        // the driver to stamp -- so the exact oracle answers.
        assert_eq!(
            fixture.task_revision,
            task_completion_revision(
                &fixture.graph.manifest,
                &fixture.graph.manifest.tasks[0],
                &fixture.graph.tasks[0],
            )
            .unwrap()
        );
        assert_eq!(plan.completion_proofs.len(), 1);
        assert_eq!(plan.completion_proofs[0].task_id, "ship-feature");
        assert_eq!(
            plan.completion_proofs[0].oracle,
            ReleaseCompletionOracle::Exact
        );
        assert_eq!(plan.completion_proofs[0].reference, None);
        assert!(
            !plan
                .digest
                .warnings
                .iter()
                .any(|warning| warning.contains("bridge")),
            "an exact completion must not mention the bridge: {:?}",
            plan.digest.warnings
        );

        // And execution persists both the proofs and the plan document, so the
        // record answers "what did this release claim, and through which
        // oracle" without re-rendering anything.
        let directory = fixture.state_dir.join("release-record");
        let calls = fixture.checkout.join("gh-calls");
        let count = fixture.checkout.join("gh-count");
        let fail_on = fixture.checkout.join("gh-fail-on");
        let shim = release_recording_gh(&fixture.checkout, &calls, &count, &fail_on);
        let config = CampaignReleaseExecutionConfig::resolve(Some(shim)).unwrap();
        execute_campaign_release_in_directory(&directory, &plan, &config).unwrap();
        let record: CampaignReleaseRecordV1 =
            read_campaign_release_record(&directory.join(RELEASE_RECORD_FILE))
                .unwrap()
                .unwrap();
        assert_eq!(record.completion_proofs, plan.completion_proofs);
        assert_eq!(
            record.plan.as_ref().unwrap(),
            &serde_json::to_value(&plan).unwrap()
        );
        assert_eq!(record.plan_sha256, release_plan_sha256(&plan).unwrap());
    }

    /// The demoted bridge is still allowed to answer -- for a proof this graph
    /// no longer computes -- but never silently.
    #[test]
    fn the_completion_bridge_names_itself_whenever_it_answers() {
        let fixture = release_completion_bridge_fixture();
        let history = release_integration_history(&fixture.checkout, "HEAD").unwrap();
        let refs =
            release_local_refs(&fixture.checkout, &["refs/heads/tally/".to_owned()]).unwrap();
        let merged =
            release_merged_commits(&fixture.graph, &fixture.revisions, &history, &refs).unwrap();
        assert_eq!(merged[0].oracle, ReleaseCompletionOracle::Bridge);

        let warnings = release_bridge_warnings(&merged);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("ship-feature")
                && warnings[0].contains("legacy completion bridge")
                && warnings[0].contains(&fixture.legacy_ref)
                && warnings[0].contains(TALLY_REVISION_PREFIX),
            "the bridge must name the task, itself, and the ref it fell back to: {}",
            warnings[0]
        );

        // An exactly proven completion says nothing at all.
        let exact = merged
            .iter()
            .map(|proof| ReleaseMergedCommit {
                oracle: ReleaseCompletionOracle::Exact,
                bridge_ref: None,
                ..proof.clone()
            })
            .collect::<Vec<_>>();
        assert!(release_bridge_warnings(&exact).is_empty());
    }

    /// The release side of the lease model: once the lapse fact is written,
    /// the coverage join is a rendering of durable facts, and no operator has
    /// to hand-author the table it used to be read from.
    #[test]
    fn release_on_a_lapsed_campaign_renders_completion_coverage_from_durable_facts() {
        let fixture = completed_release_fixture();
        let stamped = json!({
            "schemaVersion": ATTEMPT_RECEIPT_SCHEMA_VERSION,
            "sequence": 2,
            "kind": "retry",
            "campaign": RELEASE_FIXTURE_CAMPAIGN,
            "issueNumber": LOCAL_CAMPAIGN_ISSUE_NUMBER.to_string(),
            "armSerial": 1,
            "worklistSha256": format!("sha256:{}", "c".repeat(64)),
            "writtenAt": "2026-08-14T12:30:00Z",
            "actor": "uid:1000",
            "taskId": "ship-feature",
            "attempt": 2,
            "reason": "The fixture lane retried once.",
            "redaction": "conservative-v2"
        });
        let attempt_path =
            local_attempt_receipts_path(&fixture.state_dir, RELEASE_FIXTURE_CAMPAIGN).unwrap();
        let mut log = fs::read_to_string(&attempt_path).unwrap();
        log.push_str(&format!("{}\n", serde_json::to_string(&stamped).unwrap()));
        fs::write(&attempt_path, log).unwrap();

        // Nothing is armed on this path but the identity's own lease, which is
        // what "whenever, by anyone" means.
        let store = CampaignLeaseStore::new(
            &fixture.state_dir,
            RELEASE_FIXTURE_REPOSITORY,
            RELEASE_FIXTURE_WORKLIST,
        );
        let uncovered = render_campaign_release_plan(
            &fixture.state_dir,
            RELEASE_FIXTURE_REPOSITORY,
            RELEASE_FIXTURE_WORKLIST,
        )
        .unwrap();
        assert!(
            uncovered.coverage.is_none(),
            "an unfinished identity has no completion fact to render coverage from"
        );

        let lapse = store
            .acquire(
                &CampaignActivation {
                    campaign: RELEASE_FIXTURE_CAMPAIGN.to_owned(),
                    repository: RELEASE_FIXTURE_REPOSITORY.to_owned(),
                    worklist: RELEASE_FIXTURE_WORKLIST.to_owned(),
                    arm_serial: fixture.registration.arm_serial,
                    graph_digest: fixture.graph.executable_digest.clone(),
                },
                Utc::now(),
            )
            .unwrap()
            .lapse(
                &fixture.integration_tip,
                PublishProof {
                    task_id: "release-gate".to_owned(),
                    reference: fixture.checkpoint_ref.clone(),
                },
                vec!["ship-feature".to_owned(), "release-gate".to_owned()],
                Utc::now(),
            )
            .unwrap();

        let plan = render_campaign_release_plan(
            &fixture.state_dir,
            RELEASE_FIXTURE_REPOSITORY,
            RELEASE_FIXTURE_WORKLIST,
        )
        .unwrap();
        let coverage = plan
            .coverage
            .as_ref()
            .expect("a lapsed identity is covered");
        assert_eq!(coverage.source, "durable-facts");
        assert_eq!(coverage.sha, fixture.integration_tip);
        assert_eq!(coverage.lapsed_at, lapse.lapsed_at);
        assert_eq!(coverage.proven_by.reference, fixture.checkpoint_ref);
        assert!(coverage.warnings.is_empty(), "{:?}", coverage.warnings);

        // Every admitted task is a row, and every cell of it comes from a
        // durable fact: the merge commit and its oracle, the checkpoint ref,
        // the stamped receipt that says who acted and when.
        let rows = coverage
            .rows
            .iter()
            .map(|row| (row.task_id.as_str(), row))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            rows.keys().copied().collect::<Vec<_>>(),
            ["release-gate", "ship-feature"]
        );
        let implementation = rows["ship-feature"];
        assert_eq!(implementation.kind, "implementation");
        assert_eq!(implementation.title, "Ship the fixture feature");
        assert_eq!(
            implementation.claim.as_deref(),
            Some(fixture.task_revision.as_str())
        );
        assert_eq!(
            implementation.proof.as_deref(),
            Some(fixture.integration_tip.as_str())
        );
        assert_eq!(implementation.oracle, Some(ReleaseCompletionOracle::Exact));
        assert_eq!(
            implementation.witnesses,
            ["retry receipt written by uid:1000 at 2026-08-14T12:30:00Z"]
        );
        let checkpoint = rows["release-gate"];
        assert_eq!(checkpoint.kind, "checkpoint");
        assert_eq!(
            checkpoint.claim.as_deref(),
            Some(fixture.source_sha256.as_str())
        );
        assert_eq!(
            checkpoint.proof.as_deref(),
            Some(fixture.checkpoint_ref.as_str())
        );
        assert_eq!(
            checkpoint.witnesses,
            [format!(
                "gate proof {} on {}",
                fixture.checkpoint_ref, fixture.integration_tip
            )]
        );

        // The operator's own summary is carried verbatim beside the derived
        // rows -- and it is not where any of them came from: this fixture's
        // summary is three words and no table.
        let summary_body = format!(
            "{COMPLETE_SUMMARY_MARKER_PREFIX}{} -->\n\n### Campaign complete\n",
            fixture.source_sha256
        );
        assert_eq!(coverage.intent.as_deref(), Some(summary_body.as_str()));
        assert!(
            !summary_body.contains("ship-feature"),
            "the fixture's operator text must not be the source of the coverage rows"
        );

        let human = render_campaign_release_human(&plan);
        assert!(
            human.contains("Coverage from durable facts")
                && human.contains("- ship-feature [implementation]:")
                && human.contains("retry receipt written by uid:1000"),
            "{human}"
        );
        assert!(
            render_campaign_release_notes(&plan).contains("## Coverage"),
            "the published notes carry the rendered join"
        );
    }

    /// The one identity every release fixture below arms.
    const RELEASE_FIXTURE_CAMPAIGN: &str = "fixture-release";
    const RELEASE_FIXTURE_REGISTRATION: &str = "0198a62b-41ee-7000-8000-000000000777";
    const RELEASE_FIXTURE_WORKLIST: &str = "specs/release.json";
    const RELEASE_FIXTURE_REPOSITORY: &str = "acme/widgets";

    /// One campaign that finished: merged lane, gate-proven integration tip,
    /// durable checkpoint and summary refs, and a stamped attempt receipt.
    struct CompletedReleaseFixture {
        _temporary: tempfile::TempDir,
        checkout: PathBuf,
        state_dir: PathBuf,
        graph: CanonicalCampaignGraphV1,
        registration: CampaignRegistration,
        source_revision: String,
        task_revision: String,
        task_branch: String,
        source_commit: String,
        integration_branch: String,
        integration_tip: String,
        source_sha256: String,
        checkpoint_ref: String,
        complete_ref: String,
        legacy_complete_ref: String,
        archive_ref: String,
    }

    fn completed_release_fixture() -> CompletedReleaseFixture {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("repository");
        let state_dir = temporary.path().join("state");
        fs::create_dir_all(&checkout).unwrap();
        release_fixture_git(&checkout, &["init", "-b", "main"]);
        release_fixture_git(&checkout, &["config", "user.name", "Fixture"]);
        release_fixture_git(
            &checkout,
            &["config", "user.email", "fixture@example.invalid"],
        );
        fs::write(checkout.join("base.txt"), "base\n").unwrap();
        release_fixture_git(&checkout, &["add", "base.txt"]);
        release_fixture_git(&checkout, &["commit", "-m", "chore: fixture base"]);
        let source_revision = release_fixture_git(&checkout, &["rev-parse", "HEAD"]);

        let campaign = RELEASE_FIXTURE_CAMPAIGN;
        let registration_id = RELEASE_FIXTURE_REGISTRATION;
        let worklist = RELEASE_FIXTURE_WORKLIST;
        let repository = RELEASE_FIXTURE_REPOSITORY;
        let manifest = admit_manifest_value(json!({
            "schemaVersion": 1,
            "name": campaign,
            "repository": {
                "checkout": checkout,
                "baseBranch": "main",
                "remote": "network-is-forbidden",
                "forge": "local"
            },
            "maxTasks": 4,
            "maxParallel": 1,
            "agent": {},
            "gates": [{
                "kind": "command",
                "id": "test",
                "preflightArgv": ["true"],
                "argv": ["true"]
            }],
            "tasks": [{
                "id": "ship-feature",
                "kind": "implementation",
                "issue": 1,
                "dependencies": [],
                "conflictDomains": ["crates/tally"]
            }, {
                "id": "release-gate",
                "kind": "checkpoint",
                "issue": 2,
                "dependencies": ["ship-feature"],
                "argv": ["true"],
                "runtimeMaxSec": 30
            }]
        }))
        .unwrap();
        let graph = CanonicalCampaignGraphV1::new(
            manifest,
            vec![
                CanonicalCampaignTaskV1 {
                    number: 1,
                    title: "Ship the fixture feature".to_owned(),
                    body: "Implement the fixture release surface.".to_owned(),
                },
                CanonicalCampaignTaskV1 {
                    number: 2,
                    title: "Prove the fixture release".to_owned(),
                    body: "Run the fixture release gate.".to_owned(),
                },
            ],
        )
        .unwrap();
        let task_revision =
            task_completion_revision(&graph.manifest, &graph.manifest.tasks[0], &graph.tasks[0])
                .unwrap();
        let task_branch = stable_publish_branch(
            campaign,
            registration_id,
            "ship-feature",
            Some(&task_revision),
        );
        release_fixture_git(&checkout, &["checkout", "-b", &task_branch]);
        fs::write(checkout.join("feature.txt"), "released\n").unwrap();
        release_fixture_git(&checkout, &["add", "feature.txt"]);
        release_fixture_git(
            &checkout,
            &[
                "commit",
                "-m",
                "feat(crates/tally): render fixture releases",
            ],
        );
        let source_commit = release_fixture_git(&checkout, &["rev-parse", "HEAD"]);
        release_fixture_git(&checkout, &["checkout", "main"]);

        let integration_branch =
            stable_publish_branch(campaign, registration_id, "integration", None);
        release_fixture_git(&checkout, &["checkout", "-b", &integration_branch]);
        let integration_message = format!(
            "ship-feature: Ship the fixture feature\n\n{TALLY_TASK_PREFIX} ship-feature\n{TALLY_REVISION_PREFIX} {task_revision}"
        );
        release_fixture_git(
            &checkout,
            &["commit", "--allow-empty", "-m", &integration_message],
        );
        let integration_tip = release_fixture_git(&checkout, &["rev-parse", "HEAD"]);

        let source_sha256 = format!("sha256:{}", "a".repeat(64));
        let state_prefix = campaign_state_ref_prefix(campaign, LOCAL_CAMPAIGN_ISSUE_NUMBER);
        let checkpoint_ref = format!(
            "{state_prefix}/checkpoint/release-gate-{}/{integration_tip}",
            source_sha256.trim_start_matches("sha256:")
        );
        release_fixture_git(
            &checkout,
            &["update-ref", &checkpoint_ref, &integration_tip],
        );

        let complete_summary = json!({
            "schemaVersion": RELEASE_SUMMARY_SCHEMA_VERSION,
            "kind": "closing-summary",
            "campaign": campaign,
            "issueNumber": LOCAL_CAMPAIGN_ISSUE_NUMBER.to_string(),
            "outcome": "complete",
            "body": format!(
                "{COMPLETE_SUMMARY_MARKER_PREFIX}{source_sha256} -->\n\n### Campaign complete\n"
            )
        });
        let complete_object = release_fixture_blob(&checkout, &complete_summary);
        let complete_ref =
            stage_scoped_summary_ref(&state_prefix, &graph.executable_digest, "complete").unwrap();
        release_fixture_git(&checkout, &["update-ref", &complete_ref, &complete_object]);
        let legacy_complete_ref = format!("{state_prefix}/summary/complete");
        release_fixture_git(
            &checkout,
            &["update-ref", &legacy_complete_ref, &complete_object],
        );
        let archive_summary = json!({
            "schemaVersion": RELEASE_SUMMARY_SCHEMA_VERSION,
            "kind": "closing-summary",
            "campaign": campaign,
            "issueNumber": LOCAL_CAMPAIGN_ISSUE_NUMBER.to_string(),
            "outcome": "quiescent",
            "body": "<!-- tally:campaign-summary:v1 campaign=fixture-release issue=1 outcome=quiescent -->\n"
        });
        let archive_object = release_fixture_blob(&checkout, &archive_summary);
        let archive_ref = format!("{state_prefix}/summary/archive/earlier-quiescent");
        release_fixture_git(&checkout, &["update-ref", &archive_ref, &archive_object]);

        let authority = CampaignRegistrationV4 {
            schema_version: REGISTRY_SCHEMA_VERSION,
            registration_id: registration_id.to_owned(),
            worklist_pattern: worklist.to_owned(),
            code_repository: repository.to_owned(),
            checkout: checkout.clone(),
            base_branch: "main".to_owned(),
            remote: "network-is-forbidden".to_owned(),
            armed_at: "2026-08-14T12:00:00Z".to_owned(),
            arm_serial: 1,
            approved_graph_digest: graph.executable_digest.clone(),
            local_actor: local_actor(),
            allowed_actors: vec!["local".to_owned()],
            last_observation: None,
            flow: checkout.join("never-read-flow.js"),
            driver: checkout.join("never-read-driver"),
            workspace_root: temporary.path().join("campaign-workspaces"),
        };
        let registration_path = release_registration_path(&state_dir, repository, worklist);
        fs::create_dir_all(registration_path.parent().unwrap()).unwrap();
        fs::write(
            &registration_path,
            serde_json::to_vec_pretty(&authority).unwrap(),
        )
        .unwrap();
        let registration = CampaignRegistration::new(authority, None);
        write_approved_graph_snapshot(&state_dir, &registration, &graph).unwrap();
        let attempt_path = local_attempt_receipts_path(&state_dir, campaign).unwrap();
        fs::create_dir_all(attempt_path.parent().unwrap()).unwrap();
        fs::write(
            &attempt_path,
            format!(
                "{}\n",
                serde_json::to_string(&json!({
                    "schemaVersion": LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION,
                    "sequence": 1,
                    "kind": "diagnosis",
                    "campaign": campaign,
                    "issueNumber": LOCAL_CAMPAIGN_ISSUE_NUMBER.to_string(),
                    "taskId": "ship-feature",
                    "attempt": 1,
                    "diagnosis": "The first fixture attempt needed steering.",
                    "redaction": "conservative-v2"
                }))
                .unwrap()
            ),
        )
        .unwrap();

        CompletedReleaseFixture {
            _temporary: temporary,
            checkout,
            state_dir,
            graph,
            registration,
            source_revision,
            task_revision,
            task_branch,
            source_commit,
            integration_branch,
            integration_tip,
            source_sha256,
            checkpoint_ref,
            complete_ref,
            legacy_complete_ref,
            archive_ref,
        }
    }

    #[test]
    fn completed_fixture_campaign_renders_a_plan_and_probes_the_full_release_lifecycle() {
        let fixture = completed_release_fixture();
        let checkout = fixture.checkout.clone();
        let state_dir = fixture.state_dir.clone();
        let registration_id = RELEASE_FIXTURE_REGISTRATION;
        let worklist = RELEASE_FIXTURE_WORKLIST;
        let repository = RELEASE_FIXTURE_REPOSITORY;
        let temporary = &fixture._temporary;
        let source_revision = fixture.source_revision.clone();
        let task_branch = fixture.task_branch.clone();
        let source_commit = fixture.source_commit.clone();
        let integration_branch = fixture.integration_branch.clone();
        let integration_tip = fixture.integration_tip.clone();
        let checkpoint_ref = fixture.checkpoint_ref.clone();
        let complete_ref = fixture.complete_ref.clone();
        let legacy_complete_ref = fixture.legacy_complete_ref.clone();
        let archive_ref = fixture.archive_ref.clone();

        let before = release_fixture_fingerprint(&[&state_dir, &checkout]);
        let plan = render_campaign_release_plan(&state_dir, repository, worklist).unwrap();
        let after = release_fixture_fingerprint(&[&state_dir, &checkout]);

        assert_eq!(before, after, "--plan must not change local durable state");
        assert_eq!(plan.schema_version, RELEASE_PLAN_SCHEMA_VERSION);
        assert_eq!(plan.mode, "plan");
        assert_eq!(plan.revision, integration_tip);
        assert_eq!(
            plan.integration_ref,
            format!("refs/heads/{integration_branch}")
        );
        assert_eq!(plan.closing_summary.reference, complete_ref);
        assert_eq!(plan.gate_proof.task_id, "release-gate");
        assert_eq!(plan.gate_proof.reference, checkpoint_ref);
        assert_eq!(plan.completion_proofs.len(), 1);
        assert_eq!(plan.completion_proofs[0].task_id, "ship-feature");
        assert_eq!(
            plan.completion_proofs[0].oracle,
            ReleaseCompletionOracle::Exact
        );
        assert_eq!(plan.completion_proofs[0].reference, None);
        assert!(
            render_campaign_release_human(&plan).contains("- ship-feature: exact ["),
            "human plan must label each task's completion oracle"
        );
        assert_eq!(plan.release_notes.len(), 1);
        assert_eq!(
            plan.release_notes[0].source_ref.as_deref(),
            Some(format!("refs/heads/{task_branch}").as_str())
        );
        assert_eq!(
            plan.release_notes[0].source_commit.as_deref(),
            Some(source_commit.as_str())
        );
        assert_eq!(
            plan.release_notes[0].header,
            "feat(crates/tally): render fixture releases"
        );
        assert_eq!(plan.release_notes[0].kind, "feat");
        assert_eq!(plan.release_notes[0].scope.as_deref(), Some("crates/tally"));
        assert_eq!(plan.digest.merged[0].task_id, "ship-feature");
        assert_eq!(plan.digest.checkpoints[0].task_id, "release-gate");
        assert_eq!(plan.digest.source.revision, source_revision);
        assert_eq!(plan.digest.base_revision, integration_tip);
        assert_eq!(plan.digest.steering.len(), 1);
        assert!(
            plan.artifacts
                .iter()
                .any(|artifact| artifact.kind == "archived-summary"
                    && artifact.locator == archive_ref)
        );
        assert!(plan
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "archived-summary"
                && artifact.locator == legacy_complete_ref));
        assert!(plan
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "attempt-receipts"));
        assert_eq!(
            plan.version,
            format!("0.0.0+20260814123456.{}", &integration_tip[..7])
        );

        let parsed = Opts::try_parse_from([
            "tally",
            "campaign",
            "release",
            repository,
            worklist,
            "--plan",
            "--state-dir",
            state_dir.to_str().unwrap(),
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Campaign {
                command: CampaignCommand::Release(CampaignReleaseArgs { plan: true, .. })
            })
        ));

        let calls = temporary.path().join("probe-gh-calls");
        let count = temporary.path().join("probe-gh-count");
        let fail_on = temporary.path().join("probe-gh-fail-on");
        let shim = release_recording_gh(temporary.path(), &calls, &count, &fail_on);
        let config = CampaignReleaseExecutionConfig::resolve(Some(shim)).unwrap();
        let receipt = execute_campaign_release_probe(&state_dir, &checkout, &plan, &config)
            .expect("the shim-forge probe must complete");

        assert_eq!(receipt.status, "passed");
        assert_eq!(receipt.source_repository, repository);
        assert_eq!(receipt.expired_repositories_deleted, 0);
        assert!(receipt.repository_created);
        assert!(receipt.release_complete);
        assert!(receipt.teardown_complete);
        assert!(receipt.receipt.is_file());
        assert!(receipt.release_record.is_file());
        assert!(!receipt.receipt.parent().unwrap().join(".source").exists());
        assert!(
            !campaign_release_directory(&state_dir, registration_id)
                .unwrap()
                .join(RELEASE_RECORD_FILE)
                .exists(),
            "a probe must not consume the real release's idempotency record"
        );
        validate_campaign_release_probe_repository(repository, &receipt.probe_repository).unwrap();
        let persisted: Value =
            serde_json::from_slice(&fs::read(&receipt.receipt).unwrap()).unwrap();
        assert_eq!(persisted["status"], "passed");
        assert_eq!(persisted["probeRepository"], receipt.probe_repository);

        let recorded = fs::read_to_string(&calls).unwrap();
        let calls = recorded.lines().collect::<Vec<_>>();
        assert_eq!(calls.len(), 6, "{recorded}");
        assert_eq!(
            calls[0],
            "repo\tlist\tacme\t--limit\t1000\t--json\tnameWithOwner,createdAt,isFork,isPrivate"
        );
        assert!(
            calls[1].starts_with(&format!(
                "repo\tcreate\t{}\t--private\t--source\t",
                receipt.probe_repository
            )),
            "{recorded}"
        );
        assert!(calls[1].ends_with("\t--remote\torigin\t--push"));
        assert!(!calls[1].contains("--fork") && !calls[1].contains("--template"));
        assert!(
            calls[2].starts_with(&format!(
                "api\t--method\tPOST\trepos/{}/git/refs",
                receipt.probe_repository
            )),
            "{recorded}"
        );
        assert!(
            calls[3].contains(&format!("\t--repo\t{}", receipt.probe_repository)),
            "{recorded}"
        );
        assert!(
            calls[4].contains(&format!("\t--repo\t{}", receipt.probe_repository)),
            "{recorded}"
        );
        assert_eq!(
            calls[5],
            format!("repo\tdelete\t{}\t--yes", receipt.probe_repository)
        );
        assert!(
            calls[2..5]
                .iter()
                .all(|call| !call.contains("view") && !call.contains("GET")),
            "probe execution must not read state back from the disposable repository: {recorded}"
        );
    }

    fn release_fixture_git(checkout: &Path, arguments: &[&str]) -> String {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(checkout)
            .args(arguments)
            .env("GIT_AUTHOR_DATE", "2026-08-14T12:34:56Z")
            .env("GIT_COMMITTER_DATE", "2026-08-14T12:34:56Z")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed:\n{}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn release_fixture_blob(checkout: &Path, value: &Value) -> String {
        let path = checkout.join(".release-summary-fixture.json");
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        let object = release_fixture_git(checkout, &["hash-object", "-w", path.to_str().unwrap()]);
        fs::remove_file(path).unwrap();
        object
    }

    fn release_fixture_fingerprint(roots: &[&Path]) -> String {
        fn visit(path: &Path, hasher: &mut Sha256) {
            let metadata = fs::symlink_metadata(path).unwrap();
            hasher.update(path.as_os_str().as_encoded_bytes());
            hasher.update(metadata.mode().to_le_bytes());
            if metadata.file_type().is_dir() {
                let mut children = fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect::<Vec<_>>();
                children.sort();
                for child in children {
                    visit(&child, hasher);
                }
            } else if metadata.file_type().is_symlink() {
                hasher.update(fs::read_link(path).unwrap().as_os_str().as_encoded_bytes());
            } else {
                hasher.update(fs::read(path).unwrap());
            }
        }

        let mut hasher = Sha256::new();
        for root in roots {
            visit(root, &mut hasher);
        }
        format!("{:x}", hasher.finalize())
    }

    #[test]
    fn arm_receipt_has_no_counter_clearing_surface() {
        let warnings = vec!["ownership warning".to_owned()];
        let receipt = arm_receipt(&json!({"status": "armed"}), &warnings, &[]);
        assert!(!receipt.as_object().unwrap().contains_key("autoPardons"));
        assert_eq!(receipt["warnings"], json!(warnings));
    }

    #[test]
    fn legacy_pardon_receipts_remain_readable_but_epochs_supersede_old_attempts() {
        let temporary = tempfile::tempdir().unwrap();
        let campaign = "night-build";
        let issue_number = LOCAL_CAMPAIGN_ISSUE_NUMBER;
        let path = local_attempt_receipts_path(temporary.path(), campaign).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let diagnosis = |sequence: u64, task_id: &str, attempt: u8| {
            json!({
                "schemaVersion": LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION,
                "sequence": sequence,
                "kind": "diagnosis",
                "campaign": campaign,
                "issueNumber": issue_number.to_string(),
                "taskId": task_id,
                "attempt": attempt,
                "diagnosis": format!("diagnosis {task_id} attempt {attempt}"),
                "redaction": "conservative-v2",
            })
        };
        let records = [
            diagnosis(1, "foundation", 1),
            diagnosis(2, "foundation", 2),
            diagnosis(3, "finish", 1),
            diagnosis(4, "finish", 2),
            json!({
                "schemaVersion": LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION,
                "sequence": 5,
                "kind": "escalation",
                "campaign": campaign,
                "issueNumber": issue_number.to_string(),
                "body": "The local frontier is quiescent.",
            }),
            json!({
                "schemaVersion": LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION,
                "sequence": 6,
                "kind": "pardon",
                "campaign": campaign,
                "issueNumber": issue_number.to_string(),
                "tasks": ["foundation"],
            }),
        ];
        let mut encoded = records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes();
        encoded.extend_from_slice(b"\n{\"interrupted\"");
        fs::write(&path, encoded).unwrap();

        let loaded = read_local_attempt_receipts(temporary.path(), campaign, issue_number).unwrap();
        assert_eq!(loaded.len(), 6, "an interrupted append is not a fact");
        let current = BTreeMap::from([
            ("finish".to_owned(), format!("sha256:{}", "a".repeat(64))),
            (
                "foundation".to_owned(),
                format!("sha256:{}", "b".repeat(64)),
            ),
        ]);
        assert_eq!(
            active_escalated_tasks_from_receipts(&loaded, &current, &BTreeMap::new()).unwrap(),
            BTreeSet::from(["finish".to_owned()])
        );

        let old_epoch = format!("sha256:{}", "c".repeat(64));
        let current_epoch = format!("sha256:{}", "d".repeat(64));
        let old_epoch_history = [
            LocalAttemptReceiptV1::Diagnosis {
                sequence: 1,
                task_id: "finish".to_owned(),
                attempt: 1,
                input_epoch: Some(old_epoch.clone()),
                blocks_task: false,
                diagnosis: "the first attempt read the wrong section".to_owned(),
                proposal: None,
                written_at: None,
            },
            LocalAttemptReceiptV1::Diagnosis {
                sequence: 2,
                task_id: "finish".to_owned(),
                attempt: 2,
                input_epoch: Some(old_epoch),
                blocks_task: true,
                diagnosis: "the second attempt cannot proceed".to_owned(),
                proposal: None,
                written_at: None,
            },
            LocalAttemptReceiptV1::Escalation,
        ];
        assert!(active_escalated_tasks_from_receipts(
            &old_epoch_history,
            &current,
            &BTreeMap::from([("finish".to_owned(), current_epoch)]),
        )
        .unwrap()
        .is_empty());

        let mut lifetime_history = (1..=MAX_TASK_LIFETIME_ATTEMPTS)
            .map(|ordinal| LocalAttemptReceiptV1::Diagnosis {
                sequence: u64::try_from(ordinal).unwrap(),
                task_id: "finish".to_owned(),
                attempt: 1,
                input_epoch: Some(format!("sha256:{ordinal:064x}")),
                blocks_task: false,
                diagnosis: "another epoch, another retryable failure".to_owned(),
                proposal: None,
                written_at: None,
            })
            .collect::<Vec<_>>();
        lifetime_history.push(LocalAttemptReceiptV1::Escalation);
        assert_eq!(
            active_escalated_tasks_from_receipts(
                &lifetime_history,
                &current,
                &BTreeMap::from([("finish".to_owned(), format!("sha256:{}", "f".repeat(64)),)]),
            )
            .unwrap(),
            BTreeSet::from(["finish".to_owned()]),
            "the lifetime latch survives every epoch refresh"
        );
    }

    #[test]
    fn needs_authority_and_impossible_render_as_distinct_campaign_outcomes() {
        let authority_revision = format!("sha256:{}", "a".repeat(64));
        let impossible_revision = format!("sha256:{}", "b".repeat(64));
        let revisions = BTreeMap::from([
            ("authority-task".to_owned(), authority_revision.clone()),
            ("impossible-task".to_owned(), impossible_revision.clone()),
        ]);
        let records = vec![
            LocalAttemptReceiptV1::WorkerOutcome(LocalWorkerOutcome {
                sequence: 1,
                task_id: "authority-task".to_owned(),
                task_revision: authority_revision,
                input_epoch: None,
                task_uuid: "00000000-0000-4000-8000-000000000801".to_owned(),
                outcome: WorkerOutcomePayload::NeedsAuthority {
                    paths: vec![
                        ".github/workflows/release.yml".to_owned(),
                        "test/fleet-gate.sh".to_owned(),
                    ],
                },
                written_at: None,
            }),
            LocalAttemptReceiptV1::WorkerOutcome(LocalWorkerOutcome {
                sequence: 2,
                task_id: "impossible-task".to_owned(),
                task_revision: impossible_revision,
                input_epoch: None,
                task_uuid: "00000000-0000-4000-8000-000000000802".to_owned(),
                outcome: WorkerOutcomePayload::Impossible {
                    reason: "The required upstream proof does not exist.".to_owned(),
                },
                written_at: None,
            }),
        ];
        let mut status = json!({
            "tasks": [
                {
                    "taskRef": "registration/authority-task",
                    "title": "Authority task",
                    "status": "pending",
                    "blockedBy": []
                },
                {
                    "taskRef": "registration/impossible-task",
                    "title": "Impossible task",
                    "status": "pending",
                    "blockedBy": []
                }
            ]
        });
        project_campaign_status_outcomes(
            &mut status,
            "fixture",
            &records,
            &revisions,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            status["tasks"][0]["outcome"]["kind"],
            json!("needs-authority")
        );
        assert_eq!(
            status["tasks"][0]["outcome"]["paths"],
            json!([".github/workflows/release.yml", "test/fleet-gate.sh"])
        );
        assert_eq!(status["tasks"][0]["outcome"]["attemptCost"], json!(0));
        assert_eq!(status["tasks"][1]["outcome"]["kind"], json!("impossible"));
        assert_eq!(status["tasks"][1]["outcome"]["claim"], json!(true));

        let mut escalated = records;
        escalated.push(LocalAttemptReceiptV1::Escalation);
        assert_eq!(
            active_escalated_tasks_from_receipts(&escalated, &revisions, &BTreeMap::new(),)
                .unwrap(),
            BTreeSet::from(["authority-task".to_owned()]),
            "needs-authority blocks without manufacturing diagnosis attempts"
        );
    }

    #[test]
    fn task_addressed_local_steering_moves_the_observation_revision() {
        let graph = local_graph_for_test();
        let quiet = CampaignSteering::default();
        let steered = CampaignSteering {
            master: Vec::new(),
            tasks: BTreeMap::from([("foundation".to_owned(), vec![json!({"body": "rerun it"})])]),
        };
        let repository_progress = json!({"base": "a"});
        assert_ne!(
            campaign_observation(&graph, &quiet, &repository_progress, 1).unwrap(),
            campaign_observation(&graph, &steered, &repository_progress, 1).unwrap()
        );
    }

    #[test]
    fn unchanged_poll_arms_only_for_dispatchable_work_with_no_live_nodes() {
        let graph = local_graph_for_test();
        let registration_id = "0198a62b-41ee-7000-8000-000000000523";
        let flow_run_id = "0198a62b-41ee-7000-8000-000000000524";
        let resting = json!({
            "flowRunId": flow_run_id,
            "state": "idle",
            "counts": {"done": 0, "running": 0, "blocked": 0, "pending": 1},
            "tasks": [{
                "taskRef": format!("{registration_id}/foundation"),
                "status": "pending",
                "blockedBy": [],
            }],
            "currentNodes": [],
        });
        assert_eq!(
            dispatchable_poll_liveness_arm(&graph, registration_id, &BTreeSet::new(), &resting,)
                .unwrap(),
            CampaignPassLiveness::Dispatchable(flow_run_id.to_owned()),
            "an unchanged resting campaign must wake when work is dispatchable"
        );

        let mut retryable = resting.clone();
        retryable["counts"]["blocked"] = json!(1);
        retryable["counts"]["pending"] = json!(0);
        retryable["tasks"][0]["status"] = json!("blocked");
        assert_eq!(
            dispatchable_poll_liveness_arm(&graph, registration_id, &BTreeSet::new(), &retryable,)
                .unwrap(),
            CampaignPassLiveness::Dispatchable(flow_run_id.to_owned()),
            "a direct failure remains dispatchable until its escalation is active"
        );

        let mut live = resting.clone();
        live["state"] = json!("running");
        live["counts"]["running"] = json!(1);
        live["currentNodes"] = json!([{"state": "pending"}]);
        assert_eq!(
            dispatchable_poll_liveness_arm(&graph, registration_id, &BTreeSet::new(), &live,)
                .unwrap(),
            CampaignPassLiveness::Live { nodes: 1 },
            "a live pass already owns campaign progress"
        );

        let escalated = BTreeSet::from(["foundation".to_owned()]);
        assert_eq!(
            dispatchable_poll_liveness_arm(&graph, registration_id, &escalated, &resting).unwrap(),
            CampaignPassLiveness::AtRest,
            "an escalated task must not create a periodic busy-loop"
        );

        let mut dependency_blocked = resting.clone();
        dependency_blocked["tasks"][0]["status"] = json!("blocked");
        dependency_blocked["tasks"][0]["blockedBy"] = json!(["prerequisite"]);
        assert_eq!(
            dispatchable_poll_liveness_arm(
                &graph,
                registration_id,
                &BTreeSet::new(),
                &dependency_blocked,
            )
            .unwrap(),
            CampaignPassLiveness::AtRest,
            "dependency-blocked work must remain at rest"
        );

        let mut complete = resting;
        complete["state"] = json!("complete");
        complete["counts"]["done"] = json!(1);
        complete["counts"]["pending"] = json!(0);
        complete["tasks"][0]["status"] = json!("done");
        assert_eq!(
            dispatchable_poll_liveness_arm(&graph, registration_id, &BTreeSet::new(), &complete,)
                .unwrap(),
            CampaignPassLiveness::AtRest,
            "completed work must not create a periodic busy-loop"
        );
    }

    #[test]
    fn repository_progress_tracks_the_driver_base_and_scoped_refs() {
        assert_eq!(
            campaign_state_ref_prefix("final-bar", 7),
            "refs/tally/spec-build/v1/049836c3e38c7ecc9c638e9c"
        );

        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("checkout");
        let remote = temporary.path().join("remote.git");
        fs::create_dir(&checkout).unwrap();
        let bare = ProcessCommand::new("git")
            .args(["init", "--bare", "--quiet", "--initial-branch=main"])
            .arg(&remote)
            .status()
            .unwrap();
        assert!(bare.success());
        let git = |arguments: &[&str]| -> String {
            let output = ProcessCommand::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(arguments)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                arguments,
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.name", "Campaign Test"]);
        git(&["config", "user.email", "campaign@example.invalid"]);
        fs::write(checkout.join("README.md"), "base\n").unwrap();
        git(&["add", "README.md"]);
        git(&["commit", "--quiet", "-m", "base"]);
        git(&["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&["push", "--quiet", "--set-upstream", "origin", "main"]);

        let mut graph = local_graph_for_test();
        graph.canonical.manifest.repository.checkout = checkout.clone();
        graph.canonical.manifest.repository.forge = "local".to_owned();
        let initial = repository_progress_value(&graph).unwrap();

        fs::write(checkout.join("README.md"), "base\nmerged\n").unwrap();
        git(&["add", "README.md"]);
        git(&["commit", "--quiet", "-m", "merge"]);
        git(&["push", "--quiet", "origin", "main"]);
        let merged = repository_progress_value(&graph).unwrap();
        assert_ne!(merged, initial, "a local base merge must wake a plain poll");

        fs::write(checkout.join("checkpoint.json"), "{}\n").unwrap();
        let object = git(&["hash-object", "-w", "checkpoint.json"])
            .trim()
            .to_owned();
        let reference = format!(
            "{}/checkpoint/gate",
            campaign_state_ref_prefix(&graph.canonical.manifest.name, LOCAL_CAMPAIGN_ISSUE_NUMBER,)
        );
        let refspec = format!("{object}:{reference}");
        git(&["push", "--quiet", "origin", &refspec]);
        let checkpointed = repository_progress_value(&graph).unwrap();
        assert_ne!(
            checkpointed, merged,
            "a campaign-scoped checkpoint must wake a plain poll"
        );
    }

    const READMISSION_REPOSITORY: &str = "acme/widgets";

    /// The prefix every lane identity in this module carries, written once so
    /// no two lanes can be told apart by a literal somebody retyped.
    const READMISSION_CAMPAIGN_PREFIX: &str = "night-readmission-";

    /// The longest scope a lane identity can carry.
    ///
    /// A campaign name is a safe path component bounded at 80 bytes
    /// (`campaign_contract`'s `safe_component`), and the prefix above spends
    /// eighteen of them.
    const READMISSION_SCOPE_MAX: usize = 80 - READMISSION_CAMPAIGN_PREFIX.len();

    /// How much of the opening test's name a scope keeps before the digest
    /// that makes it unique takes over.
    const READMISSION_SCOPE_STEM_MAX: usize = READMISSION_SCOPE_MAX - 1 - 8;

    /// A campaign identity no other lane in this process can name, derived
    /// from the test that opened it.
    ///
    /// A campaign identity is what names durable state: the lease directory
    /// is `sha256(repository, worklist)`, the receipts log and the state ref
    /// prefix are the campaign name. Lanes that share one identity are one
    /// campaign to every one of those artifacts, and the only thing keeping
    /// two of them apart is that each happens to have rooted its state
    /// somewhere else — an isolation the tests never state and a single
    /// shared root would silently remove, with `cargo test` running the lanes
    /// concurrently.
    ///
    /// A counter said only "not the same as the last one", and left *which*
    /// lane got which number to the scheduler: the name in a failure was a
    /// number that meant a different test on every run, and one missed call
    /// site aliased two live lanes. The opening test's own name says it
    /// outright — two tests cannot share a name, because the compiler will
    /// not compile a module that declares one twice — so a collision is not
    /// a discipline a later lane has to keep.
    macro_rules! readmission_scope {
        () => {{
            // A test-local item, named so its type path ends at the test.
            fn scope() {}
            readmission_scope_from(std::any::type_name_of_val(&scope))
        }};
    }

    /// Derive one lane's scope from the type path of its test-local probe and
    /// claim it.
    ///
    /// The stem is the test's own name, so a failure names the test that
    /// produced it; the digest is of the whole name, so the truncation the
    /// 80-byte bound forces cannot make two long test names one scope.
    fn readmission_scope_from(probe_type_name: &str) -> String {
        let path = probe_type_name
            .strip_suffix("::scope")
            .unwrap_or_else(|| panic!("scope probe is not this module's: {probe_type_name:?}"));
        // An `async` test's body is a closure of the test, so the path can end
        // in one or more `{{closure}}` segments; the test is the first segment
        // that is not one.
        let test = path
            .rsplit("::")
            .find(|segment| !segment.starts_with("{{"))
            .unwrap_or_default();
        assert!(
            !test.is_empty()
                && test.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                }),
            "readmission_scope! belongs in a test function, whose name is the \
             identity: {path:?}"
        );
        let digest = format!("{:x}", Sha256::digest(test.as_bytes()));
        let stem = &test[..test.len().min(READMISSION_SCOPE_STEM_MAX)];
        let scope = format!("{}-{}", stem.replace('_', "-"), &digest[..8]);
        assert!(
            readmission_scope_is_fresh(&scope),
            "lane scope {scope:?} was claimed twice: one test opened two lanes \
             under one identity, which is the aliasing this derivation exists to \
             make impossible"
        );
        scope
    }

    /// Record a scope as claimed, answering whether it was new.
    ///
    /// The registry is the second half of the proof. Derivation makes two
    /// *tests* unable to collide; this makes one test unable to collide with
    /// itself, and turns what used to surface as another lane's `Held` lease
    /// into a refusal at the moment the second lane is opened.
    fn readmission_scope_is_fresh(scope: &str) -> bool {
        static CLAIMED: std::sync::Mutex<BTreeSet<String>> =
            std::sync::Mutex::new(BTreeSet::new());
        CLAIMED
            .lock()
            .expect("readmission scope registry lock poisoned")
            .insert(scope.to_owned())
    }

    /// One armed identity whose authority remote is a real bare repository,
    /// because re-admission is defined against what `git fetch` reports and
    /// nothing weaker can prove a push was observed.
    struct ReadmissionLane {
        temporary: tempfile::TempDir,
        checkout: PathBuf,
        state_dir: PathBuf,
        config: PathBuf,
        flow: PathBuf,
        driver: PathBuf,
        /// This lane's own campaign name and worklist path.
        campaign: String,
        worklist: String,
        /// Whether the worklist this lane pushes ends in a chapter gate — the
        /// checkpoint task whose proof a lapse rests on.
        chapter_gate: bool,
    }

    fn readmission_git(checkout: &Path, arguments: &[&str]) {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(checkout)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    const READMISSION_CHAPTER_GATE: &str = "chapter-gate-c1";

    fn readmission_worklist(
        campaign: &str,
        goal: &str,
        gate_runtime_max_sec: Option<u64>,
        chapter_gate: bool,
    ) -> Value {
        let mut gate = json!({
            "kind": "command",
            "id": "tests",
            "preflightArgv": ["true"],
            "argv": ["true"]
        });
        if let Some(seconds) = gate_runtime_max_sec {
            gate.as_object_mut()
                .unwrap()
                .insert("runtimeMaxSec".to_owned(), json!(seconds));
        }
        let mut tasks = vec![json!({
            "id": "foundation",
            "kind": "implementation",
            "title": "Build the foundation",
            "goal": goal,
            "deliveredBehaviors": ["The foundation exists"],
            "readFirst": {"specSections": ["specs/night/spec.md"], "styleReferences": []},
            "acceptanceCriteria": [{
                "id": "green",
                "description": "The suite passes.",
                "argv": ["true"]
            }],
            "dependencies": [],
            "conflictDomains": ["src"]
        })];
        if chapter_gate {
            tasks.push(json!({
                "id": READMISSION_CHAPTER_GATE,
                "kind": "checkpoint",
                "title": "Prove the chapter",
                "dependencies": ["foundation"],
                "argv": ["true"],
                "runtimeMaxSec": 60
            }));
        }
        json!({
            "schemaVersion": 1,
            "campaign": {
                "name": campaign,
                "maxTasks": 4,
                "maxParallel": 1,
                "agent": {},
                "gates": [gate]
            },
            "tasks": tasks
        })
    }

    impl ReadmissionLane {
        fn open(scope: String, goal: &str) -> Self {
            Self::open_with(scope, goal, None, false)
        }

        fn open_declaring(scope: String, goal: &str, gate_runtime_max_sec: Option<u64>) -> Self {
            Self::open_with(scope, goal, gate_runtime_max_sec, false)
        }

        /// A lane whose worklist ends in a chapter gate, which is the only
        /// shape a campaign can ever finish in.
        fn open_gated(scope: String, goal: &str) -> Self {
            Self::open_with(scope, goal, None, true)
        }

        /// `scope` is always `readmission_scope!()` at the call: the identity
        /// is the opening test's, and nothing here can invent one.
        fn open_with(
            scope: String,
            goal: &str,
            gate_runtime_max_sec: Option<u64>,
            chapter_gate: bool,
        ) -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let checkout = temporary.path().join("checkout");
            let remote = temporary.path().join("remote.git");
            let state_dir = temporary.path().join("state");
            fs::create_dir(&checkout).unwrap();
            assert!(ProcessCommand::new("git")
                .args(["init", "--bare", "--quiet", "--initial-branch=main"])
                .arg(&remote)
                .status()
                .unwrap()
                .success());
            readmission_git(&checkout, &["init", "--quiet", "--initial-branch=main"]);
            readmission_git(&checkout, &["config", "user.name", "Campaign Test"]);
            readmission_git(
                &checkout,
                &["config", "user.email", "campaign@example.invalid"],
            );
            readmission_git(
                &checkout,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );

            let assets = temporary.path().join("assets");
            fs::create_dir(&assets).unwrap();
            let flow = assets.join("spec-build.js");
            let driver = assets.join("spec-build-driver");
            fs::write(&flow, "fixture flow\n").unwrap();
            fs::write(&driver, "fixture driver\n").unwrap();
            let config = assets.join("config.json");
            fs::write(
                &config,
                serde_json::to_vec(&json!({
                    "pools": {
                        "flow": {},
                        "campaign-agent": {},
                        "campaign-control": {}
                    },
                    "adapters": {
                        "shell": {},
                        "spec-build-driver": {},
                        "codex": {}
                    }
                }))
                .unwrap(),
            )
            .unwrap();

            let lane = Self {
                temporary,
                checkout,
                state_dir,
                config,
                flow,
                driver,
                campaign: format!("{READMISSION_CAMPAIGN_PREFIX}{scope}"),
                worklist: format!("specs/night/tasks-{scope}.json"),
                chapter_gate,
            };
            lane.push_worklist_declaring(goal, gate_runtime_max_sec);
            lane
        }

        /// Append one stamped receipt to this identity's own append-only
        /// attempt-receipts log, exactly as a pass's driver appends it.
        ///
        /// `payload` carries the kind-specific fields; the identity, the
        /// sequence, and the stamp are the log's own and are filled in here.
        fn seed_receipt(&self, kind: &str, written_at: &str, payload: Value) -> u64 {
            let path = local_attempt_receipts_path(&self.state_dir, &self.campaign).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let sequence = u64::try_from(
                fs::read_to_string(&path)
                    .map(|text| text.lines().count())
                    .unwrap_or_default(),
            )
            .unwrap()
                + 1;
            let mut record = json!({
                "schemaVersion": ATTEMPT_RECEIPT_SCHEMA_VERSION,
                "sequence": sequence,
                "kind": kind,
                "campaign": self.campaign,
                "issueNumber": LOCAL_CAMPAIGN_ISSUE_NUMBER.to_string(),
                "armSerial": 1,
                "worklistSha256": format!("sha256:{}", "b".repeat(64)),
                "writtenAt": written_at,
                "actor": "spec-build-driver",
            });
            let object = record.as_object_mut().unwrap();
            for (field, value) in payload.as_object().unwrap() {
                object.insert(field.clone(), value.clone());
            }
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(file, "{record}").unwrap();
            sequence
        }

        /// Append one recorded firing of a gate id to this identity's own
        /// attempt-receipts log — the evidence a silent budget derives from.
        fn seed_gate_observation(&self, gate_id: &str, duration_sec: u64) {
            self.seed_receipt(
                "gate-observation",
                "2026-08-17T12:00:00Z",
                json!({"gateId": gate_id, "durationSec": duration_sec}),
            );
        }

        /// The arming act itself: an amended worklist committed and pushed to
        /// the identity's authority remote, with no tally verb involved.
        fn push_worklist(&self, goal: &str) {
            self.push_worklist_declaring(goal, None);
        }

        fn push_worklist_declaring(&self, goal: &str, gate_runtime_max_sec: Option<u64>) {
            let path = self.checkout.join(&self.worklist);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                serde_json::to_vec_pretty(&readmission_worklist(
                    &self.campaign,
                    goal,
                    gate_runtime_max_sec,
                    self.chapter_gate,
                ))
                .unwrap(),
            )
            .unwrap();
            readmission_git(&self.checkout, &["add", &self.worklist]);
            readmission_git(&self.checkout, &["commit", "--quiet", "-m", "worklist"]);
            readmission_git(&self.checkout, &["push", "--quiet", "origin", "main"]);
        }

        /// The revision the authority remote publishes on its base branch.
        fn published_head(&self) -> String {
            let output = ProcessCommand::new("git")
                .arg("-C")
                .arg(&self.checkout)
                .args(["rev-parse", "refs/remotes/origin/main"])
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        }

        /// Push one campaign-scoped durable fact, exactly as a pass's merge,
        /// checkpoint, or publication does.
        fn seed_campaign_ref(&self, reference: &str, object: &str) {
            readmission_git(&self.checkout, &["fetch", "--quiet", "origin"]);
            readmission_git(
                &self.checkout,
                &[
                    "push",
                    "--quiet",
                    "origin",
                    &format!("{object}:{reference}"),
                ],
            );
        }

        fn state_prefix(&self) -> String {
            campaign_state_ref_prefix(&self.campaign, LOCAL_CAMPAIGN_ISSUE_NUMBER)
        }

        /// The merge receipt that makes one implementation task terminal.
        fn seed_merge_receipt(&self, task_id: &str, revision: &str) {
            self.seed_campaign_ref(
                &format!("{}/merge/{task_id}", self.state_prefix()),
                revision,
            );
        }

        /// The chapter gate's own proof of one revision.
        fn seed_gate_proof(&self, graph: &CampaignGraph, revision: &str) {
            self.seed_campaign_ref(
                &format!(
                    "{}/checkpoint/{READMISSION_CHAPTER_GATE}-{}/{revision}",
                    self.state_prefix(),
                    &graph.worklist_sha256[7..]
                ),
                revision,
            );
        }

        /// What a poll pass reads: the graph committed at the authority
        /// remote's base branch, never the working tree beside it.
        fn live_graph(&self) -> CampaignGraph {
            self.live_graph_result().unwrap()
        }

        fn live_graph_result(&self) -> Result<CampaignGraph> {
            local_campaign_graph_from_worklist(
                &self.state_dir,
                CampaignRepository {
                    checkout: self.checkout.clone(),
                    base_branch: "main".to_owned(),
                    remote: "origin".to_owned(),
                    forge: "local".to_owned(),
                },
                READMISSION_REPOSITORY,
                &self.worklist,
                &load_client_config(Some(&self.config)).unwrap().adapters,
            )
        }

        fn registry(&self) -> CampaignRegistry {
            CampaignRegistry::open(&self.state_dir).unwrap()
        }

        /// Epoch one, written exactly as `campaign arm` writes it.
        fn arm(&self, graph: &CampaignGraph) -> CampaignRegistration {
            let mut registration = CampaignRegistration::new(
                CampaignRegistrationV4 {
                    schema_version: REGISTRY_SCHEMA_VERSION,
                    registration_id: "0198a62b-41ee-7000-8000-0000000005c1".to_owned(),
                    worklist_pattern: self.worklist.clone(),
                    code_repository: READMISSION_REPOSITORY.to_owned(),
                    checkout: self.checkout.clone(),
                    base_branch: "main".to_owned(),
                    remote: "origin".to_owned(),
                    armed_at: Utc::now().to_rfc3339(),
                    arm_serial: 1,
                    approved_graph_digest: graph.canonical.executable_digest.clone(),
                    local_actor: local_actor(),
                    allowed_actors: vec![LOCAL_ALLOWED_ACTOR.to_owned()],
                    last_observation: None,
                    flow: self.flow.clone(),
                    driver: self.driver.clone(),
                    workspace_root: self.temporary.path().join("workspaces"),
                },
                None,
            );
            write_local_attempt_receipt_authority(&self.state_dir, graph, 1).unwrap();
            write_approved_graph_snapshot(&self.state_dir, &registration, &graph.canonical)
                .unwrap();
            self.registry().write(&mut registration).unwrap();
            registration
        }

        /// Activation over this lane, taken exactly as a pass takes it.
        fn lease(
            &self,
            registration: &CampaignRegistration,
            graph: &CampaignGraph,
        ) -> CampaignLeaseGuard {
            campaign_lease_store(&self.state_dir, registration)
                .acquire(&campaign_activation(graph, registration), Utc::now())
                .unwrap()
        }

        fn receipt_authority(&self, graph: &CampaignGraph) -> AttemptReceiptAuthorityV1 {
            let path = local_attempt_receipt_authority_path(
                &self.state_dir,
                &graph.canonical.manifest.name,
            )
            .unwrap();
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
        }
    }

    /// The lease race's structural cure, stated as a property (ext2's
    /// flaky-test class).
    ///
    /// `lease_concurrent_passes_never_double_dispatch_one_frontier` panicked
    /// on a `Held` lease for scope `night-readmission-6` after the counter
    /// was supposed to have made lane identities unique: the counter promised
    /// only "different from the last one", so which lane held which number
    /// was the scheduler's to decide and the failure named a scope that meant
    /// a different test on every run. The two facts that replace it are here:
    /// a lane's identity is the opening test's own name, and a second claim
    /// of one identity is refused where it happens rather than surfacing much
    /// later as somebody else's contended lease.
    #[test]
    fn lease_scopes_are_isolation_keyed_by_the_test_name() {
        // The stem is the test's name; the suffix is a digest of the whole of
        // it. Two tests, two identities, with nothing shared to schedule.
        let one = readmission_scope_from("tally::cli::campaign::tests::a_lease_test::scope");
        let two = readmission_scope_from("tally::cli::campaign::tests::another_lease_test::scope");
        assert!(one.starts_with("a-lease-test-"), "{one}");
        assert_ne!(one, two);

        // The 80-byte campaign-name bound truncates the stem, and the digest
        // is what keeps truncation from merging two lanes into one identity.
        let shared_stem = "a".repeat(READMISSION_SCOPE_STEM_MAX + 4);
        let long = readmission_scope_from(&format!("tests::{shared_stem}_alpha::scope"));
        let longer = readmission_scope_from(&format!("tests::{shared_stem}_beta::scope"));
        assert_ne!(long, longer, "truncation must not merge two lanes");
        for scope in [&one, &two, &long, &longer] {
            let campaign = format!("{READMISSION_CAMPAIGN_PREFIX}{scope}");
            assert!(campaign.len() <= 80, "unusable campaign name: {campaign}");
        }

        // Claiming is what makes it a guard: one test opening two lanes under
        // one name is refused at the second lane, not raced into the first
        // lane's durable state.
        assert!(
            !readmission_scope_is_fresh(&one),
            "a claimed scope must never be handed out twice"
        );

        // And the derived identity is the one every durable artifact is named
        // for — the receipts log and the state-ref prefix through the
        // campaign name, the lease directory through the worklist path.
        let lane = ReadmissionLane::open(
            readmission_scope!(),
            "Build the foundation as first authored.",
        );
        let derived = "lease-scopes-are-isolation-keyed-by-the-test-name-";
        assert!(
            lane.campaign
                .starts_with(&format!("{READMISSION_CAMPAIGN_PREFIX}{derived}")),
            "{}",
            lane.campaign
        );
        assert!(
            lane.worklist
                .starts_with(&format!("specs/night/tasks-{derived}")),
            "{}",
            lane.worklist
        );
        assert!(!readmission_scope_is_fresh(
            lane.campaign
                .strip_prefix(READMISSION_CAMPAIGN_PREFIX)
                .expect("the campaign name carries this module's prefix")
        ));
    }

    /// The red one: a worklist that declares no gate budget used to be handed
    /// 900 by a serde default nobody measured. It now gets the never-fired
    /// floor until the gate fires, and its own receipts after that.
    #[test]
    fn gate_budget_derives_from_seeded_receipts_when_the_worklist_is_silent() {
        let lane = ReadmissionLane::open(readmission_scope!(), "Build the foundation as first authored.");

        let unobserved = lane.live_graph();
        assert_eq!(
            unobserved.canonical.manifest.gates[0].runtime_max_sec(),
            GATE_BUDGET_UNOBSERVED_SEC,
            "a gate with no recorded firing binds the stated floor, not a guess"
        );
        let [floor_budget] = unobserved.gate_budgets.as_slice() else {
            panic!("one gate, one budget: {:?}", unobserved.gate_budgets);
        };
        assert_eq!(floor_budget.source, GateBudgetSource::Unobserved);
        assert_eq!(floor_budget.observations, 0);

        lane.seed_gate_observation("tests", 310);
        lane.seed_gate_observation("tests", 620);
        lane.seed_gate_observation("other-gate", 4_000);

        let derived = lane.live_graph();
        assert_eq!(
            derived.canonical.manifest.gates[0].runtime_max_sec(),
            1_240,
            "the budget is this gate's own high water times the stated slack"
        );
        let [budget] = derived.gate_budgets.as_slice() else {
            panic!("one gate, one budget: {:?}", derived.gate_budgets);
        };
        assert_eq!(budget.gate_id, "tests");
        assert_eq!(budget.source, GateBudgetSource::Derived);
        assert_eq!(
            budget.observations, 2,
            "another gate id's firings are not this gate's evidence"
        );
        assert_eq!(budget.observed_high_water_sec, Some(620));

        // The derivation is readable in the rehearsal an operator runs before
        // any pass: `campaign arm --no-enqueue` prints exactly this object.
        let receipt = arm_receipt(
            &json!({"status": "armed", "enqueued": false}),
            &[],
            &derived.gate_budgets,
        );
        let rendered = receipt["gateBudgets"][0]["derivation"]
            .as_str()
            .expect("the rehearsal must carry a derivation sentence")
            .to_owned();
        assert!(
            rendered.contains("gate tests")
                && rendered.contains("1240s")
                && rendered.contains("high water 620s")
                && rendered.contains("2 receipt observation(s)"),
            "the rehearsal must say which budget binds and why: {rendered}"
        );
        assert_eq!(receipt["gateBudgets"][0]["runtimeMaxSec"], 1_240);

        assert_ne!(
            derived.canonical.executable_digest, unobserved.canonical.executable_digest,
            "a gate budget is global execution policy; moving it is a new epoch, \
             not a silent re-pricing of proof already taken"
        );
    }

    /// The permanence half: a declared number is the budget, and no amount of
    /// receipt evidence revises it.
    #[test]
    fn gate_budget_declared_by_the_worklist_is_honored_verbatim() {
        let lane =
            ReadmissionLane::open_declaring(readmission_scope!(), "Build the foundation as first authored.", Some(45));
        lane.seed_gate_observation("tests", 620);
        lane.seed_gate_observation("tests", 3_000);

        let graph = lane.live_graph();
        assert_eq!(
            graph.canonical.manifest.gates[0].runtime_max_sec(),
            45,
            "the declared number IS the budget"
        );
        let [budget] = graph.gate_budgets.as_slice() else {
            panic!("one gate, one budget: {:?}", graph.gate_budgets);
        };
        assert_eq!(budget.source, GateBudgetSource::Declared);
        assert_eq!(budget.runtime_max_sec, 45);
        assert_eq!(
            budget.observed_high_water_sec,
            Some(3_000),
            "the receipts stay visible to the operator even when they do not bind"
        );
        assert!(
            budget.derivation.contains("honored verbatim"),
            "{}",
            budget.derivation
        );
    }

    /// A declared zero is still a refusal, and a corrupt duration cannot buy an
    /// unbounded budget for every later pass.
    #[test]
    fn gate_budget_declarations_and_observations_stay_bounded() {
        let lane = ReadmissionLane::open(readmission_scope!(), "Build the foundation as first authored.");
        lane.push_worklist_declaring("Build the foundation as first authored.", Some(0));
        let failure = local_campaign_graph_from_worklist(
            &lane.state_dir,
            CampaignRepository {
                checkout: lane.checkout.clone(),
                base_branch: "main".to_owned(),
                remote: "origin".to_owned(),
                forge: "local".to_owned(),
            },
            READMISSION_REPOSITORY,
            &lane.worklist,
            &load_client_config(Some(&lane.config)).unwrap().adapters,
        )
        .unwrap_err()
        .to_string();
        assert!(
            failure.contains("runtimeMaxSec must be positive"),
            "{failure}"
        );

        lane.push_worklist("Build the foundation as first authored.");
        lane.seed_gate_observation("tests", MAX_GATE_OBSERVATION_SEC + 1);
        let failure = lane.live_graph_result().unwrap_err().to_string();
        assert!(failure.contains("durationSec"), "{failure}");
    }

    /// The escalation stops living in pass stderr. A blocked attempt writes
    /// its typed question and the amendment it prepared into the campaign's
    /// own append-only receipts, and one verb reads them back as structured
    /// entries — task, attempt, epoch, question, prepared diff, evidence —
    /// with no supervising session grepping anything.
    #[test]
    fn inbox_blocked_escalation_lands_as_one_structured_entry() {
        let lane = ReadmissionLane::open(readmission_scope!(), "Build the foundation as first authored.");
        let graph = lane.live_graph();
        let mut registration = lane.arm(&graph);
        let epoch =
            current_task_input_epochs(&graph.task_input_hashes, &CampaignSteering::default())
                .unwrap()["foundation"]
                .clone();
        let proposal = json!({
            "kind": "amendment-task",
            "paths": ["specs/night/spec.md"],
            "goal": "Author the section the foundation's readFirst cites.",
            "acceptanceCriteria": [{
                "id": "green",
                "description": "The suite passes.",
                "argv": ["true"]
            }],
            "dependencies": []
        });
        let question = "specs/night/spec.md carries no section the goal cites.";
        let blocked = lane.seed_receipt(
            "diagnosis",
            "2026-08-17T12:00:00Z",
            json!({
                "taskId": "foundation",
                "attempt": 1,
                "diagnosis": question,
                "redaction": "conservative-v1",
                "verdict": "blocked",
                "proposal": proposal,
                "inputEpoch": epoch,
            }),
        );
        lane.seed_receipt(
            "escalation",
            "2026-08-17T12:00:01Z",
            json!({"body": "foundation is blocked on its first attempt."}),
        );

        let inbox = campaign_inbox_value(&lane.state_dir, &registration, &lane.campaign).unwrap();
        assert_eq!(inbox["campaign"], json!(lane.campaign));
        assert_eq!(inbox["open"], json!(1));
        let entries = inbox["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "one escalation, one entry: {inbox}");
        let entry = &entries[0];
        assert_eq!(entry["kind"], json!("blocked"));
        assert_eq!(entry["taskId"], json!("foundation"));
        assert_eq!(entry["attempt"], json!(1));
        assert_eq!(entry["inputEpoch"], json!(epoch));
        assert_eq!(entry["writtenAt"], json!("2026-08-17T12:00:00Z"));
        assert_eq!(entry["question"], json!(question));
        assert_eq!(entry["proposal"], proposal);
        assert_eq!(entry["state"], json!("open"));
        assert!(entry["answeredBy"].is_null(), "{entry}");
        assert_eq!(
            entry["receipt"],
            json!(local_attempt_receipt_url(&lane.campaign, blocked))
        );

        // The other typed doubt a lane can raise reads the same way, and its
        // evidence is the authority surface it named.
        lane.seed_receipt(
            "worker-outcome",
            "2026-08-17T12:05:00Z",
            json!({
                "taskId": "foundation",
                "taskRevision": format!("sha256:{}", "a".repeat(64)),
                "taskUuid": "00000000-0000-4000-8000-0000000008a1",
                "outcome": "needs-authority",
                "paths": [".github/workflows/release.yml"],
                "reason": Value::Null,
                "inputEpoch": epoch,
            }),
        );
        let inbox = campaign_inbox_value(&lane.state_dir, &registration, &lane.campaign).unwrap();
        assert_eq!(inbox["open"], json!(2));
        let authority = &inbox["entries"].as_array().unwrap()[1];
        assert_eq!(authority["kind"], json!("needs-authority"));
        assert_eq!(
            authority["evidence"],
            json!([".github/workflows/release.yml"])
        );

        // Entries are facts. A re-admission moves the epoch and rewrites the
        // approved graph, and an unanswered entry is still there afterwards —
        // the surface holds what nobody answered, not what is still current.
        lane.push_worklist("Build the foundation and the amended edge case.");
        let pushed = lane.live_graph();
        readmit_campaign_epoch(
            &lane.state_dir,
            &lane.registry(),
            &mut registration,
            &pushed,
            Some(&lane.config),
        )
        .unwrap();
        assert_eq!(registration.arm_serial, 2);
        let readmitted =
            campaign_inbox_value(&lane.state_dir, &registration, &lane.campaign).unwrap();
        assert_eq!(readmitted["open"], json!(2));
        assert_eq!(readmitted["entries"], inbox["entries"]);
        print_campaign_inbox_human(&readmitted).unwrap();
    }

    /// The loop the epoch-budget derivation already understood, now closed by
    /// a surface a human can reach: an answer is ordinary steering addressed
    /// to the task, it marks the entry rather than deleting it, and it is new
    /// input — so the epoch the exhausted budget was stamped with stops being
    /// current and the task is free to run again.
    #[test]
    fn inbox_answer_becomes_steering_that_refreshes_the_task_budget() {
        let lane = ReadmissionLane::open(readmission_scope!(), "Build the foundation as first authored.");
        let graph = lane.live_graph();
        let registration = lane.arm(&graph);
        let revisions = graph_completion_revisions(&graph.canonical).unwrap();
        let unanswered =
            current_task_input_epochs(&graph.task_input_hashes, &CampaignSteering::default())
                .unwrap();
        let question = "the acceptance argv names a gate this worklist never declared.";
        lane.seed_receipt(
            "diagnosis",
            "2026-08-17T12:00:00Z",
            json!({
                "taskId": "foundation",
                "attempt": 2,
                "diagnosis": question,
                "redaction": "conservative-v2",
                "inputEpoch": unanswered["foundation"],
            }),
        );
        lane.seed_receipt(
            "escalation",
            "2026-08-17T12:00:01Z",
            json!({"body": "foundation exhausted its budget."}),
        );

        // The budget is spent: the task is held, and the entry is what the
        // operator is being asked about.
        let records = read_local_attempt_receipts(
            &lane.state_dir,
            &lane.campaign,
            LOCAL_CAMPAIGN_ISSUE_NUMBER,
        )
        .unwrap();
        assert_eq!(
            active_escalated_tasks_from_receipts(&records, &revisions, &unanswered).unwrap(),
            BTreeSet::from(["foundation".to_owned()])
        );
        let before = campaign_inbox_value(&lane.state_dir, &registration, &lane.campaign).unwrap();
        assert_eq!(before["open"], json!(1));

        // The answer: one steer addressed to the task, appended to the
        // campaign's own ordered log. No second apparatus.
        let answer = append_local_steering_at(
            &lane.state_dir,
            &registration,
            Some("foundation".to_owned()),
            "The gate is `tests`; read specs/night/spec.md §4 for its argv.".to_owned(),
            "2026-08-17T13:00:00Z".parse::<DateTime<Utc>>().unwrap(),
        )
        .unwrap();

        // Answering marks. The entry, its question, and its evidence are
        // exactly what they were; only the answer beside it is new.
        let after = campaign_inbox_value(&lane.state_dir, &registration, &lane.campaign).unwrap();
        let entries = after["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "answering never deletes: {after}");
        assert_eq!(after["open"], json!(0));
        assert_eq!(entries[0]["question"], json!(question));
        assert_eq!(entries[0]["state"], json!("answered"));
        assert_eq!(entries[0]["answeredBy"]["sequence"], json!(answer.sequence));
        assert_eq!(entries[0]["answeredBy"]["taskId"], json!("foundation"));
        assert_eq!(entries[0]["inputEpoch"], before["entries"][0]["inputEpoch"]);

        // And the budget refreshed by derivation, not by a verb: the steer is
        // new input, so the epoch the escalation was stamped with is no
        // longer the task's, and nothing holds the task any more.
        let steering = read_existing_local_steering(&lane.state_dir, &registration).unwrap();
        let answered = current_task_input_epochs(&graph.task_input_hashes, &steering).unwrap();
        assert_ne!(answered["foundation"], unanswered["foundation"]);
        assert!(
            active_escalated_tasks_from_receipts(&records, &revisions, &answered)
                .unwrap()
                .is_empty(),
            "an answered escalation stops holding its task"
        );
        print_campaign_inbox_human(&after).unwrap();
    }

    /// The race the lease exists for. Two reconcile passes observe the same
    /// pushed worklist at the same moment — the timer's and the one a
    /// finishing pass admitted as its own successor. Exactly one may admit
    /// that epoch and own the frontier it dispatches.
    ///
    /// Before the lease both passes compared the remote against the snapshot
    /// they had each read, both bumped the arm serial, and the identity ended
    /// two epochs on with two dispatches of one frontier in flight.
    #[test]
    fn lease_concurrent_passes_never_double_dispatch_one_frontier() {
        let lane = ReadmissionLane::open(readmission_scope!(), "Build the foundation as first authored.");
        let first = lane.live_graph();
        let armed = lane.arm(&first);
        lane.push_worklist("Build the foundation and the amended edge case.");
        let pushed = lane.live_graph();

        // Both passes start from the pre-push snapshot, which is exactly what
        // a poll holds when it reaches the identity.
        let start = std::sync::Barrier::new(2);
        let admissions = std::thread::scope(|scope| {
            let passes = [0, 1].map(|_| {
                scope.spawn(|| {
                    let registry = lane.registry();
                    let mut registration = armed.clone();
                    start.wait();
                    let admission = open_campaign_pass(
                        &lane.state_dir,
                        &registry,
                        &mut registration,
                        &pushed,
                        Some(&lane.config),
                        Utc::now(),
                    )
                    .unwrap();
                    (admission, registration.arm_serial)
                })
            });
            passes.map(|pass| pass.join().unwrap())
        });

        // Whatever order the two passes reached the lock in, the push was
        // admitted exactly once: a deferred pass never got activation, and a
        // pass that inherited it re-read the identity under the lease and
        // found the epoch already admitted.
        let admitted = admissions
            .iter()
            .filter(|(admission, _)| {
                matches!(
                    admission,
                    CampaignPassAdmission::Open {
                        superseded_graph_digest: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(admitted, 1, "one push, one admission: {admissions:?}");
        for (admission, arm_serial) in &admissions {
            match admission {
                // A pass that holds activation serves the admitted epoch,
                // whether it admitted it or inherited it.
                CampaignPassAdmission::Open { .. } => assert_eq!(*arm_serial, 2),
                // A deferred pass changed nothing at all: it still holds the
                // snapshot it walked in with.
                CampaignPassAdmission::Deferred { detail } => {
                    assert_eq!(*arm_serial, 1);
                    assert!(detail.contains("already leased"), "{detail}");
                }
                CampaignPassAdmission::Complete(lapse) => {
                    panic!("nothing finished here: {lapse:?}")
                }
            }
        }

        // And the durable epoch counter moved once, not twice.
        let reread = lane
            .registry()
            .read_campaign(READMISSION_REPOSITORY, &lane.worklist)
            .unwrap()
            .unwrap();
        assert_eq!(reread.arm_serial, 2);
        assert_eq!(
            reread.approved_graph_digest,
            pushed.canonical.executable_digest
        );

        // The frontier is dispatched under the lease, never beside it: only a
        // pass holding activation can produce the guard `dispatch_campaign`
        // requires, and while it holds one nothing else can take it.
        let store = campaign_lease_store(&lane.state_dir, &reread);
        let activation = campaign_activation(&pushed, &reread);
        assert!(
            admissions
                .iter()
                .any(|(admission, _)| matches!(admission, CampaignPassAdmission::Open { .. })),
            "one pass must own the frontier: {admissions:?}"
        );
        let contended = store.acquire(&activation, Utc::now()).unwrap_err();
        assert!(
            matches!(contended, CampaignLeaseError::Held { .. }),
            "a third pass is refused while the frontier is owned: {contended}"
        );
        assert_eq!(store.read().unwrap().unwrap().arm_serial, 2);

        // Reclamation needs no verb: the holders going away is the whole of
        // it, whether they exited or died.
        drop(admissions);
        assert_eq!(
            store
                .acquire(&activation, Utc::now())
                .unwrap()
                .acquisition(),
            CampaignLeaseAcquisition::Resumed
        );
    }

    /// Completion is a written fact, not an observed silence: the last task
    /// goes terminal under a gate-proven, published head, the pass lapses the
    /// lease, and everything after that reads the fact.
    /// The seam the unified contract rests on: what an armed campaign hands
    /// the driver to stamp is the writer's tuple, task for task, and not the
    /// attempt-epoch input hash beside it.
    #[test]
    fn an_armed_graph_hands_down_the_completion_identity_release_recomputes() {
        let lane = ReadmissionLane::open_gated(readmission_scope!(), "Build the foundation as first authored.");
        let graph = lane.live_graph();
        let expected = graph
            .canonical
            .manifest
            .tasks
            .iter()
            .zip(&graph.canonical.tasks)
            .map(|(reference, content)| {
                (
                    reference.id.clone(),
                    task_completion_revision(&graph.canonical.manifest, reference, content)
                        .unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();

        assert_eq!(graph.task_completion_revisions, expected);
        assert_eq!(
            graph.task_completion_revisions.keys().collect::<Vec<_>>(),
            graph.task_input_hashes.keys().collect::<Vec<_>>(),
            "both maps cover the admitted frontier"
        );
        assert!(
            graph
                .task_completion_revisions
                .iter()
                .all(|(task_id, revision)| graph.task_input_hashes[task_id] != *revision),
            "the completion identity and the attempt epoch input are different contracts"
        );
    }

    #[test]
    fn lease_completion_writes_the_lapse_fact_a_release_can_rely_on() {
        let lane = ReadmissionLane::open_gated(readmission_scope!(), "Build the foundation as first authored.");
        let graph = lane.live_graph();
        let registration = lane.arm(&graph);
        let lease = lane.lease(&registration, &graph);
        let quiescent = || {
            run_campaign_quiescent(CampaignQuiescentArgs {
                state_dir: Some(lane.state_dir.clone()),
            })
        };

        // An armed identity with unfinished work is live, and the reason the
        // lease renews is the sentence a poll event carries.
        let disposition = lease_disposition(
            &campaign_lease_facts(&graph, &repository_progress_value(&graph).unwrap(), 0).unwrap(),
        );
        assert!(!disposition.lapses(), "{disposition:?}");
        assert!(
            disposition.reason().contains("foundation"),
            "{disposition:?}"
        );
        assert!(quiescent().is_err(), "unfinished work is not quiescence");

        // A merged frontier alone does not finish a campaign: until the
        // chapter gate has proven the published head, the lease renews.
        let head = lane.published_head();
        lane.seed_merge_receipt("foundation", &head);
        let disposition = lease_disposition(
            &campaign_lease_facts(&graph, &repository_progress_value(&graph).unwrap(), 0).unwrap(),
        );
        assert!(!disposition.lapses(), "{disposition:?}");
        assert!(
            disposition.reason().contains(READMISSION_CHAPTER_GATE),
            "{disposition:?}"
        );

        // With the gate's proof of that same head on the remote, the last
        // task is terminal and the pass writes the lapse.
        lane.seed_gate_proof(&graph, &head);
        let facts =
            campaign_lease_facts(&graph, &repository_progress_value(&graph).unwrap(), 0).unwrap();
        let CampaignLeaseDisposition::Lapse {
            sha,
            proven_by,
            tasks,
            ..
        } = lease_disposition(&facts)
        else {
            panic!("a proven, published frontier finishes the campaign");
        };
        assert_eq!(sha, head);
        assert_eq!(proven_by.task_id, READMISSION_CHAPTER_GATE);
        let lapse = lease.lapse(&sha, proven_by, tasks, Utc::now()).unwrap();
        assert_eq!(lapse.sha, head);
        assert_eq!(lapse.arm_serial, registration.arm_serial);
        assert_eq!(lapse.graph_digest, graph.canonical.executable_digest);
        assert_eq!(lapse.tasks, ["foundation", READMISSION_CHAPTER_GATE]);

        // The fact outlives the pass that wrote it, which is the whole point:
        // release reads it with no registration and no host in hand.
        let record = campaign_lease_store(&lane.state_dir, &registration)
            .read()
            .unwrap()
            .unwrap();
        assert!(record.is_lapsed() && record.holder.is_none());
        assert_eq!(record.lapse.as_ref().unwrap(), &lapse);
        assert_eq!(record.campaign, lane.campaign);

        // Quiescence is now that fact rather than an operator's disarm: the
        // identity is still armed and still listed.
        quiescent().unwrap();
        assert_eq!(campaign_list_values(&lane.registry()).unwrap().len(), 1);

        // And a poll against the complete campaign is a no-op by
        // construction. It cannot take activation at all, so it admits
        // nothing, dispatches nothing, and reports the revision that
        // finished.
        let mut later = registration.clone();
        let admission = open_campaign_pass(
            &lane.state_dir,
            &lane.registry(),
            &mut later,
            &graph,
            Some(&lane.config),
            Utc::now(),
        )
        .unwrap();
        let CampaignPassAdmission::Complete(reported) = admission else {
            panic!("a lapsed identity admits nothing: {admission:?}");
        };
        assert_eq!(reported, lapse);

        // Until a push re-admits the identity, which reactivates the lease
        // with no operator verb between the two.
        lane.push_worklist("Build the foundation and the amended edge case.");
        let pushed = lane.live_graph();
        let admission = open_campaign_pass(
            &lane.state_dir,
            &lane.registry(),
            &mut later,
            &pushed,
            Some(&lane.config),
            Utc::now(),
        )
        .unwrap();
        assert!(
            matches!(
                admission,
                CampaignPassAdmission::Open {
                    superseded_graph_digest: Some(_),
                    ..
                }
            ),
            "a pushed worklist reopens a lapsed identity: {admission:?}"
        );
        assert!(quiescent().is_err(), "a reactivated identity is live again");
    }

    #[test]
    fn poll_readmission_admits_a_pushed_worklist_with_no_operator_verb() {
        let lane = ReadmissionLane::open(readmission_scope!(), "Build the foundation as first authored.");
        let first = lane.live_graph();
        let mut registration = lane.arm(&first);
        let registry = lane.registry();

        lane.push_worklist("Build the foundation and the amended edge case.");
        let second = lane.live_graph();
        assert_ne!(
            second.canonical.executable_digest, first.canonical.executable_digest,
            "the pushed amendment must change the executable graph"
        );

        let superseded = readmit_campaign_epoch(
            &lane.state_dir,
            &registry,
            &mut registration,
            &second,
            Some(&lane.config),
        )
        .unwrap();

        assert_eq!(superseded, first.canonical.executable_digest);
        assert_eq!(registration.arm_serial, 2);
        assert_eq!(
            registration.approved_graph_digest,
            second.canonical.executable_digest
        );

        // Durable, not merely in hand: the next pass reads this from disk.
        let reread = registry
            .read_campaign(READMISSION_REPOSITORY, &lane.worklist)
            .unwrap()
            .unwrap();
        assert_eq!(reread.arm_serial, 2);
        assert_eq!(
            reread.approved_graph_digest,
            second.canonical.executable_digest
        );
        assert_eq!(
            read_approved_graph_snapshot(&lane.state_dir, &reread)
                .unwrap()
                .unwrap()
                .executable_digest,
            second.canonical.executable_digest
        );

        // Attempts opened from here stamp the new epoch.
        let authority = lane.receipt_authority(&second);
        assert_eq!(authority.arm_serial, 2);
        assert_eq!(authority.worklist_sha256, second.worklist_sha256);

        // And the pass is work, not a no-op: both the graph and the arm term
        // of the observation moved, so the poll dispatches rather than
        // reporting the registration unchanged.
        let steering = read_local_steering_snapshot(&lane.state_dir, &reread).unwrap();
        let progress = json!({});
        assert_ne!(
            campaign_observation(&first, &steering.steering, &progress, 1).unwrap(),
            campaign_observation(&second, &steering.steering, &progress, 2).unwrap(),
        );
    }

    #[tokio::test]
    async fn poll_readmission_refuses_a_straddling_attempt_as_a_digest_mismatch() {
        let lane = ReadmissionLane::open(readmission_scope!(), "Build the foundation as first authored.");
        let first = lane.live_graph();
        let mut registration = lane.arm(&first);
        let registry = lane.registry();

        lane.push_worklist("Build the foundation and the amended edge case.");
        let second = lane.live_graph();
        readmit_campaign_epoch(
            &lane.state_dir,
            &registry,
            &mut registration,
            &second,
            Some(&lane.config),
        )
        .unwrap();

        // The epoch the in-flight attempt was prepared under survives the
        // flip. Without this the refusal below could name only one digest.
        let (superseded_arm, superseded_graph) =
            read_superseded_graph_snapshot(&lane.state_dir, &registration)
                .unwrap()
                .unwrap();
        assert_eq!(superseded_arm, 1);
        assert_eq!(
            superseded_graph.executable_digest,
            first.canonical.executable_digest
        );

        // A pass still carrying the superseded graph is the straddle. It is
        // refused before it can enqueue anything, and the refusal is about
        // two epochs -- never about an agent that produced no commit.
        let mut lease = lane.lease(&registration, &second);
        let refusal = dispatch_campaign(
            CampaignHost {
                socket: &lane.temporary.path().join("absent.sock"),
                config_path: Some(&lane.config),
                state_dir: &lane.state_dir,
                rpc_timeout: Duration::from_secs(1),
            },
            &first,
            &json!({}),
            &mut registration,
            false,
            None,
            &mut lease,
        )
        .await
        .unwrap_err();
        let mismatch = refusal
            .downcast_ref::<CampaignDigestMismatch>()
            .unwrap_or_else(|| panic!("straddle must be typed, got {refusal:#}"));
        assert_eq!(
            mismatch.prepared_graph_digest,
            first.canonical.executable_digest
        );
        assert_eq!(mismatch.prepared_arm_serial, Some(1));
        assert_eq!(
            mismatch.admitted_graph_digest,
            second.canonical.executable_digest
        );
        assert_eq!(mismatch.admitted_arm_serial, 2);
        let rendered = refusal.to_string();
        assert!(rendered.contains("digest-mismatch"), "{rendered}");
        assert!(
            rendered.contains(&first.canonical.executable_digest)
                && rendered.contains(&second.canonical.executable_digest),
            "{rendered}"
        );
        for forbidden in ["produced no commit", "campaign arm", "re-arm"] {
            assert!(!rendered.contains(forbidden), "{forbidden:?} in {rendered}");
        }

        // Retention is one generation deep. A second re-admission ages the
        // first epoch out, and the refusal degrades by dropping the arm
        // attribution only -- both digests still stand.
        lane.push_worklist("Build the foundation, the amended edge case, and one more.");
        let third = lane.live_graph();
        readmit_campaign_epoch(
            &lane.state_dir,
            &registry,
            &mut registration,
            &third,
            Some(&lane.config),
        )
        .unwrap();
        assert!(read_graph_snapshot(&lane.state_dir, &registration, 1)
            .unwrap()
            .is_none());
        let stale = campaign_digest_mismatch(
            &lane.state_dir,
            &registration,
            &first.canonical.executable_digest,
            "campaign attempt foundation".to_owned(),
        );
        assert_eq!(stale.prepared_arm_serial, None);
        let rendered = stale.to_string();
        assert!(
            rendered.contains(&first.canonical.executable_digest)
                && rendered.contains(&third.canonical.executable_digest),
            "{rendered}"
        );
    }

    #[test]
    fn poll_readmission_refuses_a_push_this_host_cannot_serve_without_losing_the_epoch() {
        let lane = ReadmissionLane::open(readmission_scope!(), "Build the foundation as first authored.");
        let first = lane.live_graph();
        let mut registration = lane.arm(&first);
        let registry = lane.registry();

        lane.push_worklist("Build the foundation and the amended edge case.");
        let second = lane.live_graph();
        // A host that configures no campaign pools cannot run this graph.
        let hostile = lane.temporary.path().join("assets/unhosted.json");
        fs::write(&hostile, b"{}\n").unwrap();
        let refusal = readmit_campaign_epoch(
            &lane.state_dir,
            &registry,
            &mut registration,
            &second,
            Some(&hostile),
        )
        .unwrap_err()
        .to_string();
        assert!(refusal.contains("require configured pool"), "{refusal}");

        // The good epoch is untouched: a bad push costs nothing.
        assert_eq!(registration.arm_serial, 1);
        let reread = registry
            .read_campaign(READMISSION_REPOSITORY, &lane.worklist)
            .unwrap()
            .unwrap();
        assert_eq!(reread.arm_serial, 1);
        assert_eq!(
            reread.approved_graph_digest,
            first.canonical.executable_digest
        );
        assert_eq!(lane.receipt_authority(&first).arm_serial, 1);
    }

    fn local_graph_for_test() -> CampaignGraph {
        let manifest = serde_json::from_value(manifest_value_for_test(json!([{
            "id": "foundation",
            "kind": "implementation",
            "issue": 43,
            "dependencies": [],
            "conflictDomains": []
        }])))
        .unwrap();
        let canonical = CanonicalCampaignGraphV1::new(
            manifest,
            vec![CanonicalCampaignTaskV1 {
                number: 43,
                title: "Foundation".to_owned(),
                body: "Build the foundation.".to_owned(),
            }],
        )
        .unwrap();
        let task_input_hashes = canonical_task_input_hashes(&canonical).unwrap();
        let task_completion_revisions = graph_completion_revisions(&canonical).unwrap();
        CampaignGraph {
            canonical,
            ownership_preflight_warnings: Vec::new(),
            worklist_sha256: format!("sha256:{}", "a".repeat(64)),
            task_input_hashes,
            task_completion_revisions,
            gate_budgets: Vec::new(),
        }
    }

    fn canonical_graph_for_amendment(task_a_dependencies: &[&str]) -> CanonicalCampaignGraphV1 {
        let manifest: CampaignManifest = serde_json::from_value(manifest_value_for_test(json!([
            {
                "id": "prerequisite",
                "kind": "implementation",
                "issue": 43,
                "dependencies": [],
                "conflictDomains": []
            },
            {
                "id": "task-a",
                "kind": "implementation",
                "issue": 44,
                "dependencies": task_a_dependencies,
                "conflictDomains": []
            },
            {
                "id": "task-b",
                "kind": "implementation",
                "issue": 45,
                "dependencies": [],
                "conflictDomains": []
            }
        ])))
        .unwrap();
        CanonicalCampaignGraphV1::new(
            manifest,
            vec![
                CanonicalCampaignTaskV1 {
                    number: 43,
                    title: "Prerequisite".to_owned(),
                    body: "Prepare the dependency.".to_owned(),
                },
                CanonicalCampaignTaskV1 {
                    number: 44,
                    title: "Task A".to_owned(),
                    body: "Implement task A.".to_owned(),
                },
                CanonicalCampaignTaskV1 {
                    number: 45,
                    title: "Task B".to_owned(),
                    body: "Implement task B.".to_owned(),
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn namespace_runner_bypasses_config_while_explicit_runner_keeps_mutex_validation() {
        let mut pools = BTreeMap::new();
        validate_campaign_runner_pool("campaign/acme/widgets", &pools).unwrap();
        let missing = validate_campaign_runner_pool("legacy-runner", &pools)
            .unwrap_err()
            .to_string();
        assert!(missing.contains("require configured pool"), "{missing}");

        pools.insert(
            "legacy-runner".to_owned(),
            PoolConfig {
                resource: Some(ResourceKind::Mutex),
                capacity: 1,
                ..PoolConfig::default()
            },
        );
        validate_campaign_runner_pool("legacy-runner", &pools).unwrap();
        pools.get_mut("legacy-runner").unwrap().capacity = 2;
        let invalid = validate_campaign_runner_pool("legacy-runner", &pools)
            .unwrap_err()
            .to_string();
        assert!(invalid.contains("capacity-1 mutex"), "{invalid}");
    }

    #[test]
    fn worklist_steward_resolves_only_direct_subprocess_configuration() {
        let document = json!({
            "schemaVersion": 1,
            "campaign": {
                "agent": {"adapter": "codex"},
                "steward": "narrator",
                "stewardArgv": ["--json"],
                "gates": [{
                    "kind": "command",
                    "id": "tests",
                    "preflightArgv": ["true"],
                    "argv": ["true"]
                }]
            },
            "tasks": []
        });
        let committed = CommittedLocalWorklist {
            document,
            source_path: "specs/night/epsilon.json".to_owned(),
            source_sha256: format!("sha256:{}", "a".repeat(64)),
        };
        let repository = CampaignRepository {
            checkout: PathBuf::from("/srv/acme/widgets"),
            base_branch: "main".to_owned(),
            remote: "origin".to_owned(),
            forge: "local".to_owned(),
        };
        let narrator: AdapterConfig = serde_json::from_value(json!({
            "argv": ["narrator"],
            "env": {"NARRATOR_ENDPOINT": "https://narrator.invalid/v1"},
            "scrape": {
                "finalMessage": {
                    "stream": "stdout",
                    "mode": "regex",
                    "pattern": "^NARRATION=(.*)$"
                }
            }
        }))
        .unwrap();
        let mut adapters = BTreeMap::from([
            ("codex".to_owned(), AdapterConfig::default()),
            ("narrator".to_owned(), narrator),
        ]);
        let receipts = tempfile::tempdir().unwrap();
        let (config, _) = manifest_config_from_worklist(
            receipts.path(),
            &committed,
            &repository,
            "acme/widgets",
            &adapters,
        )
        .unwrap();
        assert_eq!(config["steward"]["argv"], json!(["narrator", "--json"]));
        assert_eq!(
            config["steward"]["env"]["NARRATOR_ENDPOINT"],
            "https://narrator.invalid/v1"
        );
        assert_eq!(config["steward"]["finalMessagePattern"], "^NARRATION=(.*)$");
        assert_eq!(config["steward"]["runtimeMaxSec"], 120);

        adapters.get_mut("narrator").unwrap().hardening = AdapterHardening::Strict;
        let failure = manifest_config_from_worklist(
            receipts.path(),
            &committed,
            &repository,
            "acme/widgets",
            &adapters,
        )
        .unwrap_err()
        .to_string();
        assert!(failure.contains("direct narration subprocess"), "{failure}");

        adapters.get_mut("narrator").unwrap().hardening = AdapterHardening::None;
        adapters
            .get_mut("narrator")
            .unwrap()
            .scrape
            .get_mut("finalMessage")
            .unwrap()
            .stream = ScrapeStream::Stderr;
        let failure = manifest_config_from_worklist(
            receipts.path(),
            &committed,
            &repository,
            "acme/widgets",
            &adapters,
        )
        .unwrap_err()
        .to_string();
        assert!(failure.contains("non-empty stdout regex"), "{failure}");
    }

    /// The worklist stays adapter-neutral bytes; the arm binds its silence.
    ///
    /// A worklist that writes no policy key used to be filled by
    /// campaign-contract constants holding one preset's vocabulary, so the same
    /// neutral bytes armed against any other adapter died at render quoting a
    /// policy nobody wrote. The silence is bound here instead, from the adapter
    /// the campaign selected and only from what that adapter declares.
    #[test]
    fn a_policy_less_worklist_binds_its_silence_to_the_selected_adapter() {
        let document = json!({
            "schemaVersion": 1,
            "campaign": {
                "agent": {"adapter": "codex"},
                "gates": [{
                    "kind": "command",
                    "id": "tests",
                    "preflightArgv": ["true"],
                    "argv": ["true"]
                }]
            },
            "tasks": []
        });
        let committed = CommittedLocalWorklist {
            document,
            source_path: "specs/night/epsilon.json".to_owned(),
            source_sha256: format!("sha256:{}", "a".repeat(64)),
        };
        let repository = CampaignRepository {
            checkout: PathBuf::from("/srv/acme/widgets"),
            base_branch: "main".to_owned(),
            remote: "origin".to_owned(),
            forge: "local".to_owned(),
        };

        // An adapter that declares nothing answers silence with silence: the
        // manifest carries no policy name it could not render.
        let silent_catalog = BTreeMap::from([("codex".to_owned(), codex_shaped_adapter(&[]))]);
        let receipts = tempfile::tempdir().unwrap();
        let (config, _) = manifest_config_from_worklist(
            receipts.path(),
            &committed,
            &repository,
            "acme/widgets",
            &silent_catalog,
        )
        .unwrap();
        assert_eq!(config["agent"]["approvalPolicy"], Value::Null);
        assert_eq!(config["agent"]["sandboxPolicy"], Value::Null);
        assert_eq!(config["agent"]["diagnosisSandboxPolicy"], Value::Null);

        // The codex preset's shape: it wants `never` and `danger-full-access`
        // for its lanes and says so itself, and its diagnosis answer is
        // workspace-write rather than the read-only jailer that kills a
        // diagnosing agent's own exec machinery.
        let mut declaring = codex_shaped_adapter(&["danger-full-access"]);
        for (key, policy) in [
            (tally_core::adapters::DEFAULT_APPROVAL_POLICY_KEY, "never"),
            (
                tally_core::adapters::DEFAULT_SANDBOX_POLICY_KEY,
                "danger-full-access",
            ),
            (
                tally_core::adapters::DEFAULT_DIAGNOSIS_SANDBOX_POLICY_KEY,
                "workspace-write",
            ),
        ] {
            declaring.extra_config.insert(key.to_owned(), json!(policy));
        }
        let catalog = BTreeMap::from([("codex".to_owned(), declaring)]);
        let (config, _) = manifest_config_from_worklist(
            receipts.path(),
            &committed,
            &repository,
            "acme/widgets",
            &catalog,
        )
        .unwrap();
        assert_eq!(config["agent"]["approvalPolicy"], "never");
        assert_eq!(config["agent"]["sandboxPolicy"], "danger-full-access");
        assert_eq!(config["agent"]["diagnosisSandboxPolicy"], "workspace-write");
    }

    #[test]
    fn packaged_campaign_assets_are_resolved_beside_the_tally_binary() {
        let temporary = tempfile::tempdir().unwrap();
        let bin = temporary.path().join("bin");
        let flow = temporary.path().join("share/tally/flows/spec-build.js");
        let driver = temporary.path().join("libexec/tally/spec-build-driver");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(flow.parent().unwrap()).unwrap();
        fs::create_dir_all(driver.parent().unwrap()).unwrap();
        let executable = bin.join("tally");
        fs::write(&executable, "tally\n").unwrap();
        fs::write(&flow, "flow\n").unwrap();
        fs::write(&driver, "driver\n").unwrap();

        assert_eq!(
            packaged_campaign_asset_from_executable(
                &executable,
                Path::new("../share/tally/flows/spec-build.js"),
                "flow",
            )
            .unwrap(),
            fs::canonicalize(&flow).unwrap()
        );
        fs::remove_file(&driver).unwrap();
        let failure = packaged_campaign_asset_from_executable(
            &executable,
            Path::new("../libexec/tally/spec-build-driver"),
            "driver",
        )
        .unwrap_err()
        .to_string();
        assert!(failure.contains("packaged campaign driver is missing"));
        assert!(failure.contains("libexec/tally/spec-build-driver"));
    }

    #[test]
    fn local_arm_ingests_committed_worklist_policy_and_gate_edits_change_the_digest() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("checkout");
        let remote = temporary.path().join("remote.git");
        fs::create_dir(&checkout).unwrap();
        let run_git = |directory: &Path, arguments: &[&str]| {
            let output = ProcessCommand::new("git")
                .args(arguments)
                .current_dir(directory)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {arguments:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git(&checkout, &["init", "--quiet", "--initial-branch=main"]);
        run_git(&checkout, &["config", "user.name", "Campaign Test"]);
        run_git(
            &checkout,
            &["config", "user.email", "campaign@example.invalid"],
        );
        run_git(
            temporary.path(),
            &[
                "init",
                "--bare",
                "--quiet",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        run_git(
            &checkout,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        let checkout = fs::canonicalize(checkout).unwrap();
        let document = json!({
            "schemaVersion": 1,
            "campaign": {
                "maxTasks": 4,
                "maxParallel": 1,
                "agent": {"adapter": "codex"},
                "gates": [{
                    "kind": "command",
                    "id": "test",
                    "preflightArgv": ["true"],
                    "argv": ["true"]
                }]
            },
            "tasks": [
                {
                    "id": "foundation",
                    "kind": "implementation",
                    "title": "Foundation",
                    "goal": "Build the local foundation in src/lib.rs.",
                    "deliveredBehaviors": ["The local foundation exists"],
                    "readFirst": {"specSections": ["Foundation"], "styleReferences": []},
                    "acceptanceCriteria": [{
                        "id": "foundation-green",
                        "description": "The foundation test passes.",
                        "argv": ["test", "-f", "src/lib.rs"]
                    }],
                    "dependencies": [],
                    "conflictDomains": ["src"]
                },
                {
                    "id": "finish",
                    "kind": "implementation",
                    "title": "Finish",
                    "goal": "Finish tests/finish.rs while changing src/lib.rs:12.",
                    "deliveredBehaviors": ["The campaign is finished"],
                    "readFirst": {"specSections": ["Finish"], "styleReferences": []},
                    "acceptanceCriteria": [{
                        "id": "finish-green",
                        "description": "The finish test passes.",
                        "argv": ["bash", "-lc", "test -f tests/finish.rs && test -f crates/tally/src/main.rs"]
                    }],
                    "dependencies": ["foundation"],
                    "conflictDomains": ["tests"]
                }
            ]
        });
        let worklist = checkout.join("specs/night/epsilon.json");
        fs::create_dir_all(worklist.parent().unwrap()).unwrap();
        fs::write(&worklist, serde_json::to_vec(&document).unwrap()).unwrap();
        run_git(&checkout, &["add", "specs/night/epsilon.json"]);
        run_git(&checkout, &["commit", "--quiet", "-m", "worklist"]);
        run_git(&checkout, &["push", "--quiet", "-u", "origin", "main"]);

        let repository = CampaignRepository {
            checkout: checkout.clone(),
            base_branch: "main".to_owned(),
            remote: "origin".to_owned(),
            forge: "local".to_owned(),
        };
        let adapters = BTreeMap::from([("codex".to_owned(), AdapterConfig::default())]);

        // The checkout may be dirty; the fetched remote base remains the only
        // authority admitted by arm.
        fs::write(&worklist, b"not json\n").unwrap();
        let committed = committed_local_worklist(&repository, "specs/*/epsilon.json").unwrap();
        assert_eq!(committed.document, document);
        assert_eq!(committed.source_path, "specs/night/epsilon.json");
        let receipts = tempfile::tempdir().unwrap();
        let (manifest_config, gate_budgets) = manifest_config_from_worklist(
            receipts.path(),
            &committed,
            &repository,
            "acme/widgets",
            &adapters,
        )
        .unwrap();
        let validated =
            validate_local_worklist_document(&committed.document, &manifest_config).unwrap();
        assert_eq!(validated.manifest.name, "epsilon");
        assert_eq!(validated.manifest.pool, "campaign/acme/widgets");
        assert_eq!(validated.manifest.merge_method, "squash");
        assert_eq!(validated.manifest.driver_runtime_max_sec, 900);
        assert_eq!(validated.manifest.runtime_max_sec, None);
        assert_eq!(
            validated
                .manifest
                .tasks
                .iter()
                .map(|task| task.issue)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        let graph =
            local_campaign_graph(validated, committed.source_sha256.clone(), gate_budgets).unwrap();
        assert!(graph.canonical.tasks[0]
            .body
            .contains("Build the local foundation in src/lib.rs."));
        assert_eq!(graph.canonical.tasks[1].number, 2);
        assert_eq!(graph.ownership_preflight_warnings.len(), 2);
        assert!(graph.ownership_preflight_warnings.iter().any(|warning| {
            warning.contains("task \"finish\"")
                && warning.contains("\"src/lib.rs\"")
                && warning.contains("in goal")
                && warning.contains("conflictDomains [\"tests\"]")
        }));
        assert!(graph.ownership_preflight_warnings.iter().any(|warning| {
            warning.contains("task \"finish\"")
                && warning.contains("\"crates/tally/src/main.rs\"")
                && warning.contains("acceptanceCriteria[0].argv")
                && warning.contains("arming continues")
        }));
        let receipt = arm_receipt(
            &json!({"status": "armed"}),
            &graph.ownership_preflight_warnings,
            &[],
        );
        assert_eq!(
            receipt["warnings"],
            json!(graph.ownership_preflight_warnings)
        );

        let mut forge_field = document.clone();
        forge_field["campaign"]["label"] = json!("must-not-be-accepted");
        let failure = manifest_config_from_worklist(
            receipts.path(),
            &CommittedLocalWorklist {
                document: forge_field,
                source_path: committed.source_path.clone(),
                source_sha256: committed.source_sha256.clone(),
            },
            &repository,
            "acme/widgets",
            &adapters,
        )
        .unwrap_err()
        .to_string();
        assert!(failure.contains("unknown field `label`"), "{failure}");

        let mut unknown_agent = document.clone();
        unknown_agent["campaign"]["agent"]["adapter"] = json!("missing");
        let failure = manifest_config_from_worklist(
            receipts.path(),
            &CommittedLocalWorklist {
                document: unknown_agent,
                source_path: committed.source_path,
                source_sha256: committed.source_sha256,
            },
            &repository,
            "acme/widgets",
            &adapters,
        )
        .unwrap_err()
        .to_string();
        assert!(
            failure.contains("unknown agent adapter \"missing\""),
            "{failure}"
        );

        let mut revised = document;
        revised["campaign"]["gates"][0]["argv"] = json!(["false"]);
        fs::write(&worklist, serde_json::to_vec(&revised).unwrap()).unwrap();
        run_git(&checkout, &["add", "specs/night/epsilon.json"]);
        run_git(&checkout, &["commit", "--quiet", "-m", "edit gate"]);
        run_git(&checkout, &["push", "--quiet", "origin", "main"]);
        let revised_graph = local_campaign_graph_from_worklist(
            receipts.path(),
            repository,
            "acme/widgets",
            "specs/*/epsilon.json",
            &adapters,
        )
        .unwrap();
        assert_ne!(
            graph.canonical.executable_digest, revised_graph.canonical.executable_digest,
            "a committed campaign gate edit must require re-arm"
        );
    }

    #[test]
    fn forge_native_continuation_re_enters_through_the_registry_scan() {
        let host = CampaignHost {
            socket: Path::new("/run/user/1000/tally/tally.sock"),
            config_path: Some(Path::new("/home/operator/.config/tally/config.json")),
            state_dir: Path::new("/home/operator/.local/state/tally"),
            rpc_timeout: Duration::from_secs(30),
        };
        assert_eq!(
            host.tally_argv_prefix(Path::new("/nix/store/tally/bin/tally")),
            vec![
                "/nix/store/tally/bin/tally",
                "--config",
                "/home/operator/.config/tally/config.json",
                "--socket",
                "/run/user/1000/tally/tally.sock",
            ]
        );
        assert_eq!(
            host.dispatch_flow_argv(
                Path::new("/nix/store/tally/bin/tally"),
                Path::new("/nix/store/spec-build.js"),
                16,
                None,
            ),
            vec![
                "/nix/store/tally/bin/tally",
                "--config",
                "/home/operator/.config/tally/config.json",
                "--socket",
                "/run/user/1000/tally/tally.sock",
                "flow",
                "run",
                "/nix/store/spec-build.js",
                "--args-from-brief",
                "--max-nodes",
                "16",
            ]
        );
        // Byte-for-byte the public poll an operator or timer runs. Durable Git
        // progress, not a private argument, gives the successor a fresh
        // observation and enqueue identity.
        assert_eq!(
            host.continuation_argv(Path::new("/nix/store/tally/bin/tally")),
            vec![
                "/nix/store/tally/bin/tally",
                "--config",
                "/home/operator/.config/tally/config.json",
                "--socket",
                "/run/user/1000/tally/tally.sock",
                "campaign",
                "poll",
                "--once",
                "--state-dir",
                "/home/operator/.local/state/tally",
            ]
        );
        assert_eq!(
            host.events_dir(),
            Path::new("/home/operator/.local/state/tally/events")
        );
        let without_config = CampaignHost {
            config_path: None,
            ..host
        };
        assert_eq!(
            without_config.tally_argv_prefix(Path::new("/nix/store/tally/bin/tally")),
            vec![
                "/nix/store/tally/bin/tally",
                "--socket",
                "/run/user/1000/tally/tally.sock",
            ],
            "an omitted config locator must not synthesize an XDG path or a \
             --config flag"
        );
        assert_eq!(
            without_config.continuation_argv(Path::new("/nix/store/tally/bin/tally")),
            vec![
                "/nix/store/tally/bin/tally",
                "--socket",
                "/run/user/1000/tally/tally.sock",
                "campaign",
                "poll",
                "--once",
                "--state-dir",
                "/home/operator/.local/state/tally",
            ]
        );
    }

    #[test]
    fn flow_node_bound_includes_pass_maintenance_and_cleanup() {
        let mut value = manifest_value_for_test(json!([]));
        let object = value.as_object_mut().unwrap();
        object.insert("maxParallel".into(), json!(3));
        object.insert(
            "gates".into(),
            json!([
                {
                    "kind": "command",
                    "id": "test",
                    "preflightArgv": ["true"],
                    "argv": ["true"]
                },
                {
                    "kind": "forbidPaths",
                    "id": "no-databases",
                    "forbidPaths": ["*.db"]
                }
            ]),
        );
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        // The Nix module computes this budget independently in
        // campaignMaxNodes. Its fixture campaign has this exact shape and is
        // asserted to be 55 too; change one side and the other must follow.
        // 3 + (2 + 2*1) + 3*(12 + 2*2) = 55.
        assert_eq!(max_flow_nodes(&manifest), 55);
    }

    #[test]
    fn flow_node_bound_covers_lanes_that_fail_at_merge() {
        // A lane that fails at merge spends every success-path node and then
        // its machinery retry, diff, diagnosis, and steering on top. maxNodes
        // counts cumulative rows, so the budget must hold all of them at once:
        // a machinery fault past its retry budget records the retry receipt
        // node and is steered in the same pass. On top of that, a pass before
        // the first merge also pays for the pristine-base preflight lane: its
        // prep and cleanup, plus a gating probe and a non-gating real-argv
        // witness for every command gate.
        const PASS_MAINTENANCE: usize = 3;
        const LANE_SUCCESS_PATH: usize = 8;
        const LANE_FAILURE_PATH: usize = 4;
        const PREFLIGHT_LANE: usize = 2;
        const PREFLIGHT_PER_COMMAND_GATE: usize = 2;

        for max_parallel in 1..=4 {
            for command_gates in 0..=2 {
                for constraint_gates in 0..=3 {
                    let mut value = manifest_value_for_test(json!([]));
                    let object = value.as_object_mut().unwrap();
                    object.insert("maxParallel".into(), json!(max_parallel));
                    object.insert(
                        "gates".into(),
                        Value::Array(
                            (0..command_gates)
                                .map(|index| {
                                    json!({
                                        "kind": "command",
                                        "id": format!("tests-{index}"),
                                        "preflightArgv": ["true"],
                                        "argv": ["true"]
                                    })
                                })
                                .chain((0..constraint_gates).map(|index| {
                                    json!({
                                        "kind": "forbidPaths",
                                        "id": format!("no-databases-{index}"),
                                        "forbidPaths": ["*.db"]
                                    })
                                }))
                                .collect(),
                        ),
                    );
                    let manifest: CampaignManifest = serde_json::from_value(value).unwrap();

                    let preflight = if command_gates == 0 {
                        0
                    } else {
                        PREFLIGHT_LANE + PREFLIGHT_PER_COMMAND_GATE * command_gates
                    };
                    let gate_count = command_gates + constraint_gates;
                    let worst_case = PASS_MAINTENANCE
                        + preflight
                        + max_parallel * (LANE_SUCCESS_PATH + LANE_FAILURE_PATH + 2 * gate_count);
                    assert!(
                        max_flow_nodes(&manifest) as usize >= worst_case,
                        "maxParallel {max_parallel} with {command_gates} command and \
                         {constraint_gates} constraint gates budgets {} nodes but a frontier \
                         failing at merge after a full preflight needs {worst_case}",
                        max_flow_nodes(&manifest)
                    );
                }
            }
        }
    }

    #[test]
    fn manifest_defaults_to_squash_with_no_steward_and_refuses_other_methods() {
        // The campaign default is squash on both sides of the seam: the Nix
        // module renders it into the brief and a local manifest that
        // names nothing gets the same integration.
        let tasks = json!([{ "id": "task-1", "kind": "implementation", "issue": 8 }]);
        let manifest: CampaignManifest =
            serde_json::from_value(manifest_value_for_test(tasks.clone())).unwrap();
        assert_eq!(manifest.merge_method, "squash");
        assert!(manifest.steward.is_none());
        validate_manifest(&manifest).unwrap();

        let mut value = manifest_value_for_test(tasks.clone());
        value
            .as_object_mut()
            .unwrap()
            .insert("mergeMethod".into(), json!("rebase"));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("mergeMethod must be merge or squash"),
            "{error}"
        );

        let mut value = manifest_value_for_test(tasks);
        let object = value.as_object_mut().unwrap();
        object.insert("mergeMethod".into(), json!("merge"));
        object.insert(
            "steward".into(),
            json!({"adapter": "narrator", "argv": ["narrate", "--json"]}),
        );
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        validate_manifest(&manifest).unwrap();
        assert_eq!(manifest.merge_method, "merge");
        assert_eq!(manifest.steward.as_ref().unwrap().adapter, "narrator");

        // The adapter entry's environment and declared capture ride along; a
        // steward that carried only argv could never be pointed at a real
        // endpoint.
        let mut value = manifest_value_for_test(json!([
            { "id": "task-1", "kind": "implementation", "issue": 8 }
        ]));
        value.as_object_mut().unwrap().insert(
            "steward".into(),
            json!({
                "adapter": "narrator",
                "argv": ["narrate"],
                "env": {"NARRATOR_ENDPOINT": "https://narrator.invalid/v1"},
                "finalMessagePattern": "^NARRATOR_RESULT=(.*)$"
            }),
        );
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        validate_manifest(&manifest).unwrap();
        let steward = manifest.steward.as_ref().unwrap();
        assert_eq!(
            steward.env.get("NARRATOR_ENDPOINT").map(String::as_str),
            Some("https://narrator.invalid/v1")
        );
        assert_eq!(steward.final_message_pattern, "^NARRATOR_RESULT=(.*)$");

        // TALLY_BRIEF is the publish node's own; a steward may not redefine it.
        let mut value = manifest_value_for_test(json!([
            { "id": "task-1", "kind": "implementation", "issue": 8 }
        ]));
        value.as_object_mut().unwrap().insert(
            "steward".into(),
            json!({
                "adapter": "narrator",
                "argv": ["narrate"],
                "env": {"TALLY_BRIEF": "/tmp/x"}
            }),
        );
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("not an assignable environment identifier"),
            "{error}"
        );

        // An empty narration argv would render a steward that cannot be run.
        let mut value = manifest_value_for_test(json!([
            { "id": "task-1", "kind": "implementation", "issue": 8 }
        ]));
        value
            .as_object_mut()
            .unwrap()
            .insert("steward".into(), json!({"adapter": "narrator", "argv": []}));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("steward argv must be a non-empty direct argv"),
            "{error}"
        );
    }

    #[test]
    fn manifest_agent_model_must_be_non_empty_when_present() {
        // An empty model would render a job asking the adapter for nothing at
        // all, and a trailer naming nothing at all.
        let mut value = manifest_value_for_test(json!([
            { "id": "task-1", "kind": "implementation", "issue": 8 }
        ]));
        value
            .as_object_mut()
            .unwrap()
            .insert("agent".into(), json!({"adapter": "codex", "model": ""}));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("agent limits, policy names, and model must be non-empty and bounded"),
            "{error}"
        );
    }

    #[test]
    fn manifest_accepts_native_checkpoints_and_rejects_unknown_kinds() {
        let value = manifest_value_for_test(json!([
            {
                "id": "build",
                "kind": "implementation",
                "issue": 43,
                "dependencies": [],
                "conflictDomains": []
            },
            {
                "id": "verify",
                "kind": "checkpoint",
                "issue": 44,
                "dependencies": ["build"],
                "argv": ["nix", "flake", "check"],
                "runtimeMaxSec": 900
            }
        ]));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        validate_manifest(&manifest).unwrap();
        let checkpoint_with_domains = manifest_value_for_test(json!([
            {
                "id": "build",
                "kind": "implementation",
                "issue": 43,
                "dependencies": []
            },
            {
                "id": "verify",
                "kind": "checkpoint",
                "issue": 44,
                "dependencies": ["build"],
                "conflictDomains": [],
                "argv": ["true"],
                "runtimeMaxSec": 30
            }
        ]));
        let manifest: CampaignManifest = serde_json::from_value(checkpoint_with_domains).unwrap();
        assert!(validate_manifest(&manifest).is_err());
        let mut invalid = manifest_value_for_test(json!([{
            "id": "mystery",
            "kind": "approval",
            "issue": 43,
            "dependencies": [],
            "conflictDomains": []
        }]));
        let manifest: CampaignManifest = serde_json::from_value(invalid.take()).unwrap();
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn arm_campaign_policy_acceptance_is_pinned() {
        assert!(validate_argv(&["true".into(), "".into()], "argv").is_err());
        assert!(validate_argv(&["true".into(), "line\nbreak".into()], "argv").is_err());
        validate_argv(&["true".into(), "--flag".into()], "argv").unwrap();

        let cases = json!([
            {
                "gates": [{
                    "kind": "command",
                    "id": "tests",
                    "preflightArgv": ["true"],
                    "argv": ["true"]
                }]
            },
            {
                "name": "night-build",
                "maxTasks": 128,
                "maxParallel": 2,
                "mergeMethod": "merge",
                "driverRuntimeMaxSec": 60,
                "runtimeMaxSec": null,
                "agent": {
                    "adapter": "codex",
                    "argv": [BRIEF_SENTINEL],
                    "model": null,
                    "priority": "low",
                    "approvalPolicy": "never",
                    "sandboxPolicy": "danger-full-access",
                    "diagnosisSandboxPolicy": "read-only",
                    "runtimeMaxSec": 14400
                },
                "steward": "narrator",
                "stewardArgv": ["--json"],
                "stewardRuntimeMaxSec": 120,
                "gates": [{
                    "kind": "forbidPaths",
                    "id": "no-state",
                    "forbidPaths": ["*.db", "generated/**"]
                }]
            },
            {},
            {"gates": [], "label": "forge-only"},
            {"maxTasks": 129, "gates": [{"kind": "command", "id": "x", "preflightArgv": ["true"], "argv": ["true"]}]},
            {"maxTasks": 1, "maxParallel": 2, "gates": [{"kind": "command", "id": "x", "preflightArgv": ["true"], "argv": ["true"]}]},
            {"name": null, "gates": [{"kind": "command", "id": "x", "preflightArgv": ["true"], "argv": ["true"]}]},
            {"mergeMethod": null, "gates": [{"kind": "command", "id": "x", "preflightArgv": ["true"], "argv": ["true"]}]},
            {"stewardArgv": ["--json"], "gates": [{"kind": "command", "id": "x", "preflightArgv": ["true"], "argv": ["true"]}]},
            {"agent": {"argv": ["line\nbreak"]}, "gates": [{"kind": "command", "id": "x", "preflightArgv": ["true"], "argv": ["true"]}]},
            {"agent": {"argv": ["delete\u{007f}control"]}, "gates": [{"kind": "command", "id": "x", "preflightArgv": ["true"], "argv": ["true"]}]},
            {"gates": [{"kind": "forbidPaths", "id": "line-break", "forbidPaths": ["generated\nstate"]}]},
            {"gates": [
                {"kind": "command", "id": "same", "preflightArgv": ["true"], "argv": ["true"]},
                {"kind": "forbidPaths", "id": "same", "forbidPaths": ["*.db"]}
            ]}
        ]);
        let acceptance = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|campaign| {
                parse_worklist_campaign_policy(
                    &json!({"campaign": campaign}),
                    "specs/night/epsilon.json",
                )
                .is_ok()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            acceptance,
            vec![
                true, true, false, false, false, false, false, false, false, false, false, true,
                false,
            ]
        );
    }

    #[test]
    fn canonical_digest_matches_the_driver_contract() {
        let value = json!({"z": [1, "é"], "a": {"b": true, "a": null}});
        assert_eq!(
            sha256_json(&value).unwrap(),
            "sha256:356741b14061aca3cb3e9abc01fe332af042dfcd59d81c56ee9fb57832dc6429"
        );
    }

    #[test]
    fn registration_v4_round_trips_local_repository_authority() {
        let root = tempfile::tempdir().unwrap();
        let state_dir = root.path();
        let forge_actor = "operator".to_owned();
        let flow = root.path().join("flow.js");
        let driver = root.path().join("driver");
        fs::write(&flow, "flow fixture\n").unwrap();
        fs::write(&driver, "driver fixture\n").unwrap();
        let mut registration = CampaignRegistration::new(
            CampaignRegistrationV4 {
                schema_version: REGISTRY_SCHEMA_VERSION,
                registration_id: uuid::Uuid::now_v7().to_string(),
                worklist_pattern: "specs/night/tasks.json".to_owned(),
                code_repository: "acme/widgets".to_owned(),
                checkout: PathBuf::from("/srv/acme/widgets"),
                base_branch: "stable".to_owned(),
                remote: "upstream".to_owned(),
                armed_at: "2026-08-01T00:00:00Z".to_owned(),
                arm_serial: 1,
                approved_graph_digest: format!("sha256:{}", "a".repeat(64)),
                local_actor: local_actor(),
                allowed_actors: normalize_allowed_actors(&["Reviewer".into()], &forge_actor)
                    .unwrap(),
                last_observation: None,
                flow,
                driver,
                workspace_root: PathBuf::from("/srv/tally-campaigns"),
            },
            Some(240_000),
        );
        let registry = CampaignRegistry::open(state_dir).unwrap();
        registry.write(&mut registration).unwrap();
        let loaded = registry
            .read_campaign(
                &registration.code_repository,
                &registration.worklist_pattern,
            )
            .unwrap()
            .unwrap();
        assert_eq!(loaded.registration_id, registration.registration_id);
        assert_eq!(loaded.checkout, Path::new("/srv/acme/widgets"));
        assert_eq!(loaded.base_branch, "stable");
        assert_eq!(loaded.remote, "upstream");
        assert_eq!(loaded.allowed_actors, ["operator", "reviewer"]);
        assert_eq!(
            loaded.approved_graph_digest,
            registration.approved_graph_digest
        );
        // #432: the durable projection wait survives the round trip, which is
        // what makes `campaign poll` dispatch later passes with the same
        // widened window the operator armed with.
        assert_eq!(loaded.projection_wait_ms, Some(240_000));
    }

    #[test]
    fn local_steering_embargo_and_cursor_bound_dispatch() {
        let temporary = tempfile::tempdir().unwrap();
        let registration = CampaignRegistration::new(
            CampaignRegistrationV4 {
                schema_version: REGISTRY_SCHEMA_VERSION,
                registration_id: "0198a62b-41ee-7000-8000-000000000571".to_owned(),
                worklist_pattern: "specs/night/tasks.json".to_owned(),
                code_repository: "acme/widgets".to_owned(),
                checkout: PathBuf::from("/srv/acme/widgets"),
                base_branch: "main".to_owned(),
                remote: "origin".to_owned(),
                armed_at: "2026-08-01T00:00:00Z".to_owned(),
                arm_serial: 1,
                approved_graph_digest: format!("sha256:{}", "a".repeat(64)),
                local_actor: local_actor(),
                allowed_actors: vec!["operator".to_owned()],
                last_observation: None,
                flow: PathBuf::from("/nix/store/spec-build.js"),
                driver: PathBuf::from("/nix/store/spec-build-driver"),
                workspace_root: PathBuf::from("/srv/tally-campaigns"),
            },
            None,
        );
        let now = DateTime::parse_from_rfc3339("2026-08-13T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let first = append_local_steering_at(
            temporary.path(),
            &registration,
            None,
            "Keep the bounded path.".to_owned(),
            now,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&first.comment).unwrap(),
            json!({
                "id": 1,
                "url": "local://campaign/0198a62b-41ee-7000-8000-000000000571/steering/1",
                "author": local_actor(),
                "body": "Keep the bounded path.",
                "createdAt": "2026-08-13T10:00:00.000Z",
                "updatedAt": "2026-08-13T10:00:00.000Z",
            })
        );
        for forged in [
            "Tally-Task: task-1",
            "tally-revision: sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let error = append_local_steering_at(
                temporary.path(),
                &registration,
                None,
                forged.to_owned(),
                now,
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("reserved tally completion trailer"),
                "{error:#}"
            );
        }

        let paths = local_steering_paths(temporary.path(), &registration.registration_id);
        assert!(!paths.cursor.exists());
        match open_local_steering_dispatch_at(temporary.path(), &registration, now).unwrap() {
            LocalSteeringDispatchState::Embargoed(until) => assert_eq!(
                until,
                DateTime::parse_from_rfc3339("2026-08-13T10:00:01Z").unwrap()
            ),
            LocalSteeringDispatchState::Ready(_) => panic!("steering dispatched inside embargo"),
        }

        // Dropping a prepared dispatch models an enqueue failure: its lock is
        // released, but the durable high-water cursor must not move.
        let uncommitted = match open_local_steering_dispatch_at(
            temporary.path(),
            &registration,
            now + chrono::Duration::seconds(1),
        )
        .unwrap()
        {
            LocalSteeringDispatchState::Ready(dispatch) => dispatch,
            LocalSteeringDispatchState::Embargoed(_) => panic!("embargo did not expire"),
        };
        assert_eq!(uncommitted.snapshot.source.prepared_cursor, 1);
        assert_eq!(uncommitted.snapshot.steering.master.len(), 1);
        drop(uncommitted);
        assert!(!paths.cursor.exists());

        let committed = match open_local_steering_dispatch_at(
            temporary.path(),
            &registration,
            now + chrono::Duration::seconds(1),
        )
        .unwrap()
        {
            LocalSteeringDispatchState::Ready(dispatch) => dispatch,
            LocalSteeringDispatchState::Embargoed(_) => panic!("embargo did not expire"),
        };
        committed
            .commit(
                &format!("sha256:{}", "b".repeat(64)),
                now + chrono::Duration::seconds(1),
            )
            .unwrap();
        let first_cursor: LocalSteeringCursorV1 =
            serde_json::from_slice(&fs::read(&paths.cursor).unwrap()).unwrap();
        assert_eq!(first_cursor.high_water, 1);

        let second = append_local_steering_at(
            temporary.path(),
            &registration,
            Some("task-1".to_owned()),
            "Use the local receipt.".to_owned(),
            now,
        )
        .unwrap();
        assert_eq!(second.sequence, 2);
        assert_eq!(second.comment.created_at, "2026-08-13T10:00:00.001Z");
        let unchanged_cursor: LocalSteeringCursorV1 =
            serde_json::from_slice(&fs::read(&paths.cursor).unwrap()).unwrap();
        assert_eq!(unchanged_cursor.high_water, 1);

        let committed = match open_local_steering_dispatch_at(
            temporary.path(),
            &registration,
            now + chrono::Duration::seconds(3),
        )
        .unwrap()
        {
            LocalSteeringDispatchState::Ready(dispatch) => dispatch,
            LocalSteeringDispatchState::Embargoed(_) => panic!("second embargo did not expire"),
        };
        assert_eq!(committed.snapshot.source.prepared_cursor, 2);
        assert_eq!(committed.snapshot.steering.tasks["task-1"].len(), 1);
        committed
            .commit(
                &format!("sha256:{}", "c".repeat(64)),
                now + chrono::Duration::seconds(3),
            )
            .unwrap();
        let second_cursor: LocalSteeringCursorV1 =
            serde_json::from_slice(&fs::read(&paths.cursor).unwrap()).unwrap();
        assert_eq!(second_cursor.high_water, 2);
    }

    #[test]
    fn approved_graph_snapshots_are_generation_scoped_and_digest_checked() {
        let temporary = tempfile::tempdir().unwrap();
        let prior = canonical_graph_for_amendment(&[]);
        let amended = canonical_graph_for_amendment(&["prerequisite"]);
        let mut registration = CampaignRegistration::new(
            CampaignRegistrationV4 {
                schema_version: REGISTRY_SCHEMA_VERSION,
                registration_id: uuid::Uuid::now_v7().to_string(),
                worklist_pattern: "specs/night/tasks.json".to_owned(),
                code_repository: "acme/widgets".to_owned(),
                checkout: PathBuf::from("/srv/acme/widgets"),
                base_branch: "main".to_owned(),
                remote: "origin".to_owned(),
                armed_at: "2026-08-01T00:00:00Z".to_owned(),
                arm_serial: 1,
                approved_graph_digest: prior.executable_digest.clone(),
                local_actor: local_actor(),
                allowed_actors: vec!["operator".to_owned()],
                last_observation: None,
                flow: PathBuf::from("/nix/store/flow.js"),
                driver: PathBuf::from("/nix/store/driver"),
                workspace_root: PathBuf::from("/srv/tally-campaigns"),
            },
            None,
        );

        assert!(
            read_approved_graph_snapshot(temporary.path(), &registration)
                .unwrap()
                .is_none(),
            "a pre-snapshot registration remains readable and simply cannot prove an amendment"
        );
        write_approved_graph_snapshot(temporary.path(), &registration, &prior).unwrap();
        assert_eq!(
            read_approved_graph_snapshot(temporary.path(), &registration)
                .unwrap()
                .unwrap(),
            prior
        );
        let old_path = approved_graph_path(temporary.path(), &registration);

        registration.arm_serial = 2;
        registration.approved_graph_digest = amended.executable_digest.clone();
        write_approved_graph_snapshot(temporary.path(), &registration, &amended).unwrap();
        assert_eq!(
            read_approved_graph_snapshot(temporary.path(), &registration)
                .unwrap()
                .unwrap(),
            amended
        );
        prune_approved_graph_snapshots(temporary.path(), &registration).unwrap();
        assert!(
            old_path.exists(),
            "the immediately superseded generation is retained so an attempt \
             straddling the flip can still be told which digest it owns"
        );
        assert_eq!(
            read_superseded_graph_snapshot(temporary.path(), &registration)
                .unwrap()
                .unwrap(),
            (1, prior)
        );

        // Retention is exactly one generation deep: a third epoch collects
        // the first, so the directory cannot grow with the arm serial.
        let third = canonical_graph_for_amendment(&["prerequisite", "task-b"]);
        registration.arm_serial = 3;
        registration.approved_graph_digest = third.executable_digest.clone();
        write_approved_graph_snapshot(temporary.path(), &registration, &third).unwrap();
        prune_approved_graph_snapshots(temporary.path(), &registration).unwrap();
        assert!(
            !old_path.exists(),
            "a generation two epochs back must be pruned"
        );
        assert_eq!(
            read_superseded_graph_snapshot(temporary.path(), &registration)
                .unwrap()
                .unwrap(),
            (2, amended)
        );
    }

    /// #432 acceptance 2, the DELIVERY half of the seam.
    ///
    /// Recording `--projection-wait-ms` in the registration is worth nothing on
    /// its own: what the operator is promised is that every pass this campaign
    /// dispatches waits that long. `CampaignHost::dispatch_flow_argv` is the
    /// only place that promise is kept, so it is asserted here directly — a
    /// registration carrying `Some(n)` must put
    /// `--result-projection-wait-ms n` on the dispatched pass's argv, spelled
    /// exactly as `FlowRunArgs` parses it.
    ///
    /// The `None` half is not decoration. This argv is hashed into the enqueue
    /// payload, so a stray element would move the payload identity of every
    /// campaign armed without the flag; it is asserted element-by-element.
    ///
    /// Deleting the `--result-projection-wait-ms` push from the host's dispatch
    /// argv makes this test red — that mutation used to leave the whole crate
    /// green.
    #[test]
    fn a_recorded_projection_wait_reaches_the_dispatched_pass_argv() {
        let executable = Path::new("/nix/store/tally/bin/tally");
        let flow = Path::new("/nix/store/spec-build.js");
        let host = CampaignHost {
            socket: Path::new("/run/user/1000/tally/tally.sock"),
            config_path: None,
            state_dir: Path::new("/home/operator/.local/state/tally"),
            rpc_timeout: Duration::from_secs(30),
        };

        // No projection-wait flag means no projection-wait elements. A host
        // without an explicit config likewise emits no --config pair, while
        // the socket locator still precedes the flow subcommand.
        let unset = host.dispatch_flow_argv(executable, flow, 51, None);
        assert_eq!(
            unset,
            vec![
                "/nix/store/tally/bin/tally".to_owned(),
                "--socket".to_owned(),
                "/run/user/1000/tally/tally.sock".to_owned(),
                "flow".to_owned(),
                "run".to_owned(),
                "/nix/store/spec-build.js".to_owned(),
                "--args-from-brief".to_owned(),
                "--max-nodes".to_owned(),
                "51".to_owned(),
            ],
            "a campaign armed without --projection-wait-ms must dispatch the \
             same argv without projection-wait elements; this vector is hashed \
             into the enqueue payload"
        );

        // The recorded wait, delivered.
        let widened = host.dispatch_flow_argv(executable, flow, 51, Some(240_000));
        assert_eq!(
            widened,
            [
                unset.as_slice(),
                &[
                    "--result-projection-wait-ms".to_owned(),
                    "240000".to_owned()
                ]
            ]
            .concat(),
            "a registration carrying a projection wait must put it on the \
             dispatched pass's argv"
        );

        // The flag this argv names must be the flag `flow run` parses, or the
        // dispatched pass dies on an unknown argument instead of waiting.
        let parsed = Opts::try_parse_from(widened.iter().map(String::as_str))
            .expect("the dispatched argv must parse as a tally invocation");
        assert!(matches!(
            parsed.command,
            Some(Command::Flow {
                command: FlowCommand::Run(FlowRunArgs {
                    args_from_brief: true,
                    max_nodes: 51,
                    result_projection_wait_ms: Some(240_000),
                    ..
                })
            })
        ));
    }

    /// #432, the arm-side half of the refusal (the flow-side zero and
    /// unparsable refusals are pinned in `cli::flow::tests`). A zero recorded
    /// here would be durable: every pass this campaign ever dispatches,
    /// including the unattended poll ones, would then die on its own argv.
    #[test]
    fn a_zero_projection_wait_is_refused_at_arm() {
        assert_eq!(validated_projection_wait_ms(None).unwrap(), None);
        assert_eq!(
            validated_projection_wait_ms(Some(240_000)).unwrap(),
            Some(240_000)
        );
        assert_eq!(validated_projection_wait_ms(Some(1)).unwrap(), Some(1));
        let refused = validated_projection_wait_ms(Some(0)).unwrap_err();
        assert!(
            refused.to_string().contains("--projection-wait-ms"),
            "{refused}"
        );
    }

    /// #432 acceptance 2, the seam that actually reaches a campaign pass. A
    /// registration written before `--projection-wait-ms` existed carries no
    /// field at all; it must still load with the historical 10-second value
    /// rather than being refused or defaulted to zero.
    #[test]
    fn a_registration_without_a_projection_wait_still_loads() {
        let root = tempfile::tempdir().unwrap();
        let state_dir = root.path();
        let code_repository = "acme/widgets";
        let worklist_pattern = "specs/night/tasks.json";
        let registry = CampaignRegistry::open(state_dir).unwrap();
        let path = registry.registration_path(code_repository, worklist_pattern);
        fs::write(
            &path,
            serde_json::to_string(&json!({
                "schemaVersion": REGISTRY_SCHEMA_VERSION,
                "registrationId": uuid::Uuid::now_v7().to_string(),
                "worklistPattern": worklist_pattern,
                "codeRepository": code_repository,
                "checkout": "/srv/acme/widgets",
                "baseBranch": "main",
                "remote": "origin",
                "armedAt": "2026-08-01T00:00:00Z",
                "armSerial": 1,
                "approvedGraphDigest": format!("sha256:{}", "a".repeat(64)),
                "localActor": local_actor(),
                "allowedActors": ["operator"],
                "flow": "/nix/store/flow.js",
                "driver": "/nix/store/driver",
                "workspaceRoot": "/srv/tally-campaigns",
            }))
            .unwrap(),
        )
        .unwrap();
        let loaded = registry.read(&path).unwrap();
        assert_eq!(
            loaded.projection_wait_ms,
            Some(tally_core::campaign_registry::DEFAULT_CAMPAIGN_PROJECTION_WAIT_MS)
        );
    }

    fn codex_shaped_adapter(commit_capable: &[&str]) -> AdapterConfig {
        AdapterConfig {
            argv: vec![
                "codex".to_owned(),
                "exec".to_owned(),
                "--json".to_owned(),
                "--".to_owned(),
            ],
            launch: tally_core::adapters::AdapterLaunchConfig {
                approval_policies: BTreeMap::from([(
                    "never".to_owned(),
                    vec!["-c".to_owned(), "approval_policy=\"never\"".to_owned()],
                )]),
                sandbox_policies: BTreeMap::from([
                    (
                        "workspace-write".to_owned(),
                        vec!["--sandbox".to_owned(), "workspace-write".to_owned()],
                    ),
                    (
                        "danger-full-access".to_owned(),
                        vec!["--sandbox".to_owned(), "danger-full-access".to_owned()],
                    ),
                    (
                        "read-only".to_owned(),
                        vec!["--sandbox".to_owned(), "read-only".to_owned()],
                    ),
                ]),
                commit_capable_sandbox_policies: commit_capable
                    .iter()
                    .map(|policy| (*policy).to_owned())
                    .collect(),
                ..tally_core::adapters::AdapterLaunchConfig::default()
            },
            ..AdapterConfig::default()
        }
    }

    fn agent_with(sandbox: Option<&str>) -> CampaignAgent {
        let mut agent: CampaignAgent = serde_json::from_value(json!({})).unwrap();
        agent.sandbox_policy = sandbox.map(str::to_owned);
        agent
    }

    #[test]
    fn a_policy_less_campaign_commits_under_the_adapters_own_declared_default() {
        // A worklist that names no policy now carries none: the contract has no
        // adapter's vocabulary to fall back on.
        let silent = agent_with(None);
        assert_eq!(silent.approval_policy, None);
        assert_eq!(silent.sandbox_policy, None);
        assert_eq!(silent.diagnosis_sandbox_policy, None);

        // A codex-shaped adapter that declares its own answers binds that
        // silence at the arm seam, and the resolved pairing is one it can
        // commit under.
        let mut declaring = codex_shaped_adapter(&["danger-full-access"]);
        for (key, policy) in [
            (tally_core::adapters::DEFAULT_APPROVAL_POLICY_KEY, "never"),
            (
                tally_core::adapters::DEFAULT_SANDBOX_POLICY_KEY,
                "danger-full-access",
            ),
            (
                tally_core::adapters::DEFAULT_DIAGNOSIS_SANDBOX_POLICY_KEY,
                "workspace-write",
            ),
        ] {
            declaring.extra_config.insert(key.to_owned(), json!(policy));
        }
        let resolved = resolve_worklist_agent_policies(silent.clone(), &declaring);
        assert_eq!(resolved.approval_policy.as_deref(), Some("never"));
        assert_eq!(
            resolved.sandbox_policy.as_deref(),
            Some("danger-full-access")
        );
        // Not the lane default, and not read-only: a diagnosing agent under
        // codex's read-only jailer cannot write /dev/shm or a tempdir and dies
        // inside its own exec machinery.
        assert_eq!(
            resolved.diagnosis_sandbox_policy.as_deref(),
            Some("workspace-write")
        );
        validate_agent_policies(&resolved, &declaring).unwrap();

        // An adapter that declares nothing answers silence with silence, and
        // the commit obligation it cannot show it can honour is still refused.
        let undeclared = codex_shaped_adapter(&["danger-full-access"]);
        let unresolved = resolve_worklist_agent_policies(silent, &undeclared);
        assert_eq!(unresolved.sandbox_policy, None);
        let error = validate_agent_policies(&unresolved, &undeclared).unwrap_err();
        assert!(error.to_string().contains("<adapter default>"), "{error}");

        // An explicit worklist value wins outright over any declaration.
        let explicit = resolve_worklist_agent_policies(
            CampaignAgent {
                diagnosis_sandbox_policy: Some("read-only".to_owned()),
                ..agent_with(Some("workspace-write"))
            },
            &declaring,
        );
        assert_eq!(explicit.sandbox_policy.as_deref(), Some("workspace-write"));
        assert_eq!(
            explicit.diagnosis_sandbox_policy.as_deref(),
            Some("read-only")
        );

        // The estate workaround already deployed by the consumer: both values
        // explicit, approval disabled outright.
        let workaround = CampaignAgent {
            approval_policy: None,
            ..agent_with(Some("danger-full-access"))
        };
        validate_agent_policies(&workaround, &undeclared).unwrap();
    }

    #[test]
    fn missing_agent_final_message_capture_warns_before_worker_findings_are_lost() {
        let agent = agent_with(Some("danger-full-access"));
        let mut adapter = codex_shaped_adapter(&["danger-full-access"]);
        let warning = worker_findings_warning(&agent, &adapter).unwrap();
        assert!(warning.contains("scrape.finalMessage"), "{warning}");
        assert!(
            warning.contains("worker findings will not be retained"),
            "{warning}"
        );

        adapter.scrape.insert(
            "finalMessage".to_owned(),
            serde_json::from_value(json!({
                "mode": "jsonPathLast",
                "pattern": "$[?@.type == 'item.completed'].item.text"
            }))
            .unwrap(),
        );
        assert_eq!(worker_findings_warning(&agent, &adapter), None);
    }

    fn manifest_with_checkpoint_and_gate_argv(argv: Vec<String>) -> CampaignManifest {
        let mut value = manifest_value_for_test(json!([{
            "id": "checkpoint",
            "kind": "checkpoint",
            "issue": 43,
            "dependencies": [],
            "argv": argv,
            "runtimeMaxSec": 900
        }]));
        value["gates"] = json!([{
            "kind": "command",
            "id": "flake-check",
            "preflightArgv": value["tasks"][0]["argv"].clone(),
            "argv": value["tasks"][0]["argv"].clone(),
            "runtimeMaxSec": 900
        }]);
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn argv_hazards_are_silent_without_a_hardening_tier() {
        let hazardous = vec![
            "sh".to_owned(),
            "-euc".to_owned(),
            "nix build /tmp/staged; mkdir -p \"$HOME/output\"".to_owned(),
        ];
        assert!(
            argv_hazard_warnings(
                &manifest_with_checkpoint_and_gate_argv(hazardous),
                AdapterHardening::None,
            )
            .is_empty(),
            "a host without a hardening preset must not receive hardened-tier argv warnings"
        );
    }

    #[test]
    fn hardened_argv_hazards_warn_for_checkpoints_and_gates_but_hermetic_argv_is_silent() {
        let bare_nix = vec!["nix".to_owned(), "build".to_owned(), ".#checks".to_owned()];
        let hermetic_nix = vec![
            "sh".to_owned(),
            "-euc".to_owned(),
            "export XDG_CACHE_HOME=/tmp/nix-cache XDG_STATE_HOME=/tmp/nix-state; mkdir -p \"$XDG_CACHE_HOME\" \"$XDG_STATE_HOME\"; exec nix build .#checks".to_owned(),
        ];

        let warnings = argv_hazard_warnings(
            &manifest_with_checkpoint_and_gate_argv(bare_nix),
            AdapterHardening::Strict,
        );
        assert_eq!(warnings.len(), 3, "{warnings:#?}");
        for context in [
            "checkpoint task \"checkpoint\" argv",
            "campaign gate \"flake-check\" preflightArgv",
            "campaign gate \"flake-check\" argv",
        ] {
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains(context) && warning.contains("nix")),
                "missing warning for {context}: {warnings:#?}"
            );
        }

        assert!(
            argv_hazard_warnings(
                &manifest_with_checkpoint_and_gate_argv(hermetic_nix),
                AdapterHardening::Strict,
            )
            .is_empty(),
            "the documented private-cache argv must not warn"
        );
    }

    #[test]
    fn argv_hazards_ignore_self_created_tmp_paths_and_non_evaluating_nix_probes() {
        let benign = vec![
            "sh".to_owned(),
            "-euc".to_owned(),
            "command -v nix >/dev/null; mkdir -p /tmp/tally-gate; test -d /tmp/tally-gate/output"
                .to_owned(),
        ];
        assert!(
            argv_hazard_warnings(
                &manifest_with_checkpoint_and_gate_argv(benign),
                AdapterHardening::Production,
            )
            .is_empty(),
            "in-unit /tmp creation and a nix availability probe are safe under PrivateTmp"
        );

        for subcommand in ["develop", "build", "shell", "run"] {
            let warnings = argv_hazard_warnings(
                &manifest_with_checkpoint_and_gate_argv(vec![
                    "nix".to_owned(),
                    subcommand.to_owned(),
                ]),
                AdapterHardening::Workspace,
            );
            assert_eq!(warnings.len(), 3, "nix {subcommand}: {warnings:#?}");
        }

        for staged in [
            "mkdir -p /tmp/owned; cat /tmp/staged",
            "cat /tmp/late; mkdir -p /tmp/late",
        ] {
            assert!(
                argv_has_staged_tmp_reference(&[
                    "sh".to_owned(),
                    "-c".to_owned(),
                    staged.to_owned()
                ]),
                "an unrelated or later mkdir must not suppress {staged:?}"
            );
        }
    }

    #[test]
    fn a_sandbox_that_cannot_commit_is_refused_at_arm_time() {
        let adapter = codex_shaped_adapter(&["danger-full-access"]);
        for sandbox in [Some("workspace-write"), None] {
            let error = validate_agent_policies(&agent_with(sandbox), &adapter)
                .unwrap_err()
                .to_string();
            assert!(error.contains("cannot create a commit"), "{error}");
            assert!(error.contains("danger-full-access"), "{error}");
        }

        // An adapter that declares no commit capability is not second-guessed.
        let silent = codex_shaped_adapter(&[]);
        validate_agent_policies(&agent_with(Some("workspace-write")), &silent).unwrap();
        validate_agent_policies(&agent_with(None), &silent).unwrap();
    }

    #[test]
    fn an_undeclared_policy_name_is_still_refused_at_arm_time() {
        let adapter = codex_shaped_adapter(&["danger-full-access"]);
        let error = validate_agent_policies(&agent_with(Some("not-declared")), &adapter)
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not authorized by adapter"), "{error}");

        let mut agent = agent_with(Some("danger-full-access"));
        agent.diagnosis_sandbox_policy = Some("not-declared".to_owned());
        let error = validate_agent_policies(&agent, &adapter)
            .unwrap_err()
            .to_string();
        assert!(error.contains("diagnosisSandboxPolicy"), "{error}");
    }
}
