use super::text::compact_text;
use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsString};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::process::{Command as ProcessCommand, Stdio};

use chrono::{DateTime, SecondsFormat};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tally_core::adapters::{AdapterConfig, AdapterHardening, ScrapeMode, ScrapeStream};
use tally_core::campaign_contract::{
    admit_manifest_value, task_completion_revision, validate_agent, validate_argv, validate_gates,
    CampaignAgent, CampaignGate, CampaignManifest, CampaignRepository, CampaignSteward,
    CanonicalCampaignGraphV1, CanonicalCampaignTaskV1, BRIEF_SENTINEL, CAMPAIGN_SCHEMA_VERSION,
    DEFAULT_AGENT_APPROVAL_POLICY, DEFAULT_AGENT_DIAGNOSIS_SANDBOX_POLICY, DEFAULT_AGENT_PRIORITY,
    DEFAULT_AGENT_RUNTIME_MAX_SEC, DEFAULT_AGENT_SANDBOX_POLICY, DEFAULT_DRIVER_RUNTIME_MAX_SEC,
    DEFAULT_MAX_TASKS, DEFAULT_STEWARD_FINAL_MESSAGE_PATTERN, DEFAULT_STEWARD_RUNTIME_MAX_SEC,
    MAX_CAMPAIGN_TASKS,
};
use tally_core::campaign_folds::{
    campaign_digest, render_campaign_summary, stable_publish_branch, BlockedFact, CampaignDigest,
    CampaignReconciliation, CampaignSource, CheckpointFact, DeferralFact, DiagnosisFact,
    MergedFact, ReconciledTask, RetryFact, TALLY_REVISION_PREFIX, TALLY_TASK_PREFIX,
};
use tally_core::campaign_poll::{CampaignPollEvent, CampaignPollStatus};
use tally_core::campaign_registry::{
    CampaignRegistration, CampaignRegistrationV4, CampaignRegistry, REGISTRY_SCHEMA_VERSION,
};
use tally_core::config::{PoolConfig, ResourceKind};
use tally_core::lease::{is_campaign_pool_name, CAMPAIGN_POOL_PREFIX};

const COMPLETION_TRAILER_PREFIXES: [&str; 2] = ["Tally-Task:", "Tally-Revision:"];
const APPROVED_GRAPH_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const MAX_APPROVED_GRAPH_SNAPSHOT_BYTES: u64 = 32 * 1024 * 1024;
const CAMPAIGN_STEERING_SCHEMA_VERSION: u32 = 1;
const CAMPAIGN_STEERING_CURSOR_SCHEMA_VERSION: u32 = 1;
const CAMPAIGN_STEERING_EMBARGO_MILLISECONDS: i64 = 1_000;
const MAX_CAMPAIGN_STEERING_LOG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CAMPAIGN_STEERING_BODY_CHARS: usize = 64_000;
const MAX_CAMPAIGN_STEERING_PER_TARGET: usize = 1_000;
const ATTEMPT_RECEIPTS_SCHEMA_VERSION: u64 = 1;
const ATTEMPT_RECEIPTS_FILE: &str = "attempt-receipts-v1.jsonl";
const MAX_ATTEMPT_RECEIPTS_LOG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DIAGNOSIS_CHARS: usize = 12_000;
const MAX_RETRY_CHARS: usize = 2_000;
const LOCAL_CAMPAIGN_ISSUE_NUMBER: u64 = 1;
const LOCAL_ALLOWED_ACTOR: &str = "local";
const RELEASE_PLAN_SCHEMA_VERSION: u32 = 1;
const RELEASE_RECORD_SCHEMA_VERSION: u32 = 1;
const RELEASE_ARTIFACTS_SCHEMA_VERSION: u32 = 1;
const RELEASE_SUMMARY_SCHEMA_VERSION: u32 = 1;
const MAX_RELEASE_REGISTRATION_BYTES: u64 = 1024 * 1024;
const MAX_RELEASE_RECORD_BYTES: u64 = 1024 * 1024;
const MAX_RELEASE_PAYLOAD_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RELEASE_GIT_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const RELEASE_RECORD_FILE: &str = "release-record-v1.json";
const RELEASE_NOTES_FILE: &str = "release-notes.md";
const RELEASE_ARTIFACTS_FILE: &str = "release-artifacts-v1.json";
const COMPLETE_SUMMARY_MARKER_PREFIX: &str = "<!-- tally:campaign-complete:v1 source=";

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
    gates: Vec<CampaignGate>,
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
        approval_policy: Some(DEFAULT_AGENT_APPROVAL_POLICY.to_owned()),
        sandbox_policy: Some(DEFAULT_AGENT_SANDBOX_POLICY.to_owned()),
        diagnosis_sandbox_policy: Some(DEFAULT_AGENT_DIAGNOSIS_SANDBOX_POLICY.to_owned()),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedAutoPardon {
    task_id: String,
    added_dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AutoPardonReceipt {
    task_id: String,
    added_dependencies: Vec<String>,
    resume_receipt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PardonScope {
    All,
    Tasks(BTreeSet<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalAttemptReceiptV1 {
    Diagnosis { task_id: String, attempt: u8 },
    Retry,
    Escalation,
    Pardon { tasks: Option<BTreeSet<String>> },
}

#[derive(Debug, Clone)]
enum ReleaseAttemptReceipt {
    Diagnosis {
        task_id: String,
        attempt: u64,
        diagnosis: String,
    },
    Retry {
        task_id: String,
        attempt: u64,
        reason: String,
    },
    Escalation,
    Pardon {
        sequence: u64,
        tasks: Option<BTreeSet<String>>,
    },
}

#[derive(Debug)]
struct ReleaseAttemptLog {
    path: PathBuf,
    present: bool,
    bytes: Vec<u8>,
    records: Vec<ReleaseAttemptReceipt>,
}

#[derive(Debug, Clone)]
struct ReleaseGitRef {
    object_id: String,
    object_type: String,
    reference: String,
}

#[derive(Debug, Clone)]
struct ReleaseCommit {
    object_id: String,
    parents: Vec<String>,
    committed_at: i64,
    message: String,
    task_values: Vec<String>,
    revision_values: Vec<String>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    Unchanged,
    RearmRequired {
        approved_graph_digest: String,
        live_graph_digest: String,
    },
}

pub(super) async fn run_campaign(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    command: CampaignCommand,
) -> Result<()> {
    match command {
        CampaignCommand::Arm(args) => {
            run_campaign_arm(socket, config_path, rpc_timeout, args).await
        }
        CampaignCommand::Steer(args) => run_campaign_steer(args),
        CampaignCommand::Resume(args) => {
            run_campaign_resume(socket, config_path, rpc_timeout, args).await
        }
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
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "cannot create campaign release directory {}",
            directory.display()
        )
    })?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).with_context(|| {
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

    let execution = execute_campaign_release_locked(&directory, plan, config);
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
        executed_steps.push("tag");
    }

    if record.steps.release_notes {
        skipped_steps.push("release-notes");
    } else {
        publish_campaign_release_notes(config, plan, &payloads.notes)?;
        record.steps.release_notes = true;
        write_campaign_release_record(directory, &record_path, &record)?;
        executed_steps.push("release-notes");
    }

    if record.steps.artifacts {
        skipped_steps.push("artifacts");
    } else {
        attach_campaign_release_artifacts(config, plan, &payloads.artifacts)?;
        record.steps.artifacts = true;
        write_campaign_release_record(directory, &record_path, &record)?;
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

fn campaign_release_directory(state_dir: &Path, registration_id: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(registration_id)
        .context("campaign release registration ID is not a UUID")?;
    Ok(state_dir.join("campaigns/releases").join(registration_id))
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
    if output.status.success() {
        return Ok(());
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
    let campaign_branch_prefix = format!(
        "refs/heads/{}",
        integration_branch
            .strip_suffix("integration")
            .expect("the integration branch ends with its fixed leaf")
    );
    let state_prefix = campaign_state_ref_prefix(campaign, LOCAL_CAMPAIGN_ISSUE_NUMBER);
    let refs = release_local_refs(
        &registration.checkout,
        &[
            integration_ref.clone(),
            campaign_branch_prefix,
            format!("{state_prefix}/"),
        ],
    )?;
    let integration = release_required_ref(&refs, &integration_ref, "commit")?;
    let history = release_integration_history(&registration.checkout, &integration_ref)?;
    let revisions = release_task_revisions(&graph)?;
    let merged_commits = release_merged_commits(&graph, &revisions, &history)?;
    let source_revision = merged_commits
        .first()
        .and_then(|(_, commit)| commit.parents.first())
        .cloned()
        .unwrap_or_else(|| integration.object_id.clone());

    let all_checkpoints =
        release_checkpoint_refs(&graph, &refs, &state_prefix, &integration.object_id)?;
    let gate_checkpoint =
        release_gate_checkpoint(&graph, &all_checkpoints, &integration.object_id)?;
    let checkpoints =
        release_current_checkpoints(&graph, &all_checkpoints, &gate_checkpoint.source_sha256)?;

    let summaries = release_summary_refs(&registration.checkout, &refs, &state_prefix, campaign)?;
    let closing_summary =
        release_closing_summary(&summaries, &state_prefix, &gate_checkpoint.source_sha256)?;
    let attempt_log = read_release_attempt_log(state_dir, campaign, LOCAL_CAMPAIGN_ISSUE_NUMBER)?;

    let task_ids = graph
        .manifest
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<BTreeSet<_>>();
    let (diagnoses, retries, warnings) = release_attempt_facts(&attempt_log.records, &task_ids);
    let merged = merged_commits
        .iter()
        .map(|(task_id, commit)| MergedFact {
            task_id: task_id.clone(),
            pull_request: format!(
                "local://{code_repository}/{}",
                stable_publish_branch(
                    campaign,
                    &registration.registration_id,
                    task_id,
                    revisions.get(task_id).map(String::as_str),
                )
            ),
            merge_commit: commit.object_id.clone(),
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

fn release_local_refs(checkout: &Path, prefixes: &[String]) -> Result<Vec<ReleaseGitRef>> {
    let mut arguments = vec![
        "for-each-ref".to_owned(),
        "--format=%(objectname)%09%(objecttype)%09%(refname)".to_owned(),
    ];
    arguments.extend(prefixes.iter().cloned());
    let stdout = release_git_read(checkout, &arguments, "listing local campaign refs")?;
    let stdout = String::from_utf8(stdout).context("local campaign refs were not UTF-8")?;
    let mut refs = Vec::new();
    for line in stdout.lines().filter(|line| !line.is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3
            || !is_git_object_id(fields[0])
            || !matches!(fields[1], "blob" | "commit")
            || !fields[2].starts_with("refs/")
        {
            bail!("local campaign ref listing returned a malformed row");
        }
        refs.push(ReleaseGitRef {
            object_id: fields[0].to_owned(),
            object_type: fields[1].to_owned(),
            reference: fields[2].to_owned(),
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
        "%H%x00%P%x00%ct%x00%B%x00%(trailers:key={task_key},valueonly,unfold=true,separator=%x1f)%x00%(trailers:key={revision_key},valueonly,unfold=true,separator=%x1f)"
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
    if fields.len() % 6 != 0 {
        bail!("local integration trailer listing returned malformed output");
    }
    fields
        .chunks_exact(6)
        .map(|fields| {
            let object_id = fields[0];
            let parents = fields[1]
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if !is_git_object_id(object_id)
                || parents.iter().any(|parent| !is_git_object_id(parent))
            {
                bail!("local integration history returned a malformed commit");
            }
            let committed_at = fields[2]
                .parse::<i64>()
                .context("local integration history returned a malformed timestamp")?;
            Ok(ReleaseCommit {
                object_id: object_id.to_owned(),
                parents,
                committed_at,
                message: fields[3].to_owned(),
                task_values: split_release_trailers(fields[4]),
                revision_values: split_release_trailers(fields[5]),
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

fn release_task_revisions(graph: &CanonicalCampaignGraphV1) -> Result<BTreeMap<String, String>> {
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
) -> Result<Vec<(String, ReleaseCommit)>> {
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
            if safe_task_id(task_id) && is_sha256_identity(revision) {
                claims
                    .entry((task_id.clone(), revision.clone()))
                    .or_default()
                    .push(commit);
            }
        }
    }
    for task_id in &implementation_ids {
        let revision = revisions
            .get(*task_id)
            .expect("every graph task has a computed revision");
        match claims.get(&(String::from(*task_id), revision.clone())) {
            Some(matches) if matches.len() == 1 => {}
            Some(_) => bail!(
                "multiple local integration commits claim campaign task {task_id:?} revision {revision}"
            ),
            None => bail!(
                "completed campaign is missing the {TALLY_TASK_PREFIX} {task_id} / {TALLY_REVISION_PREFIX} {revision} trailer proof"
            ),
        }
    }

    let mut merged = Vec::new();
    for commit in history.iter().rev() {
        let ([task_id], [revision]) = (
            commit.task_values.as_slice(),
            commit.revision_values.as_slice(),
        ) else {
            continue;
        };
        if implementation_ids.contains(task_id.as_str()) && revisions.get(task_id) == Some(revision)
        {
            merged.push((task_id.clone(), commit.clone()));
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
        bail!("completed campaign has no checkpoint ref for integration tip {integration_tip}");
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
            "completed campaign has no gate-proof checkpoint ref at integration tip {integration_tip}"
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
                    "completed campaign is missing checkpoint ref for task {:?} and source {}",
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
    let prefix = format!("{state_prefix}/summary/");
    refs.iter()
        .filter(|reference| reference.reference.starts_with(&prefix))
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
) -> Result<&'a ReleaseSummaryRef> {
    let mut matches = summaries
        .iter()
        .filter(|summary| {
            summary.summary.outcome == "complete"
                && summary.source_sha256.as_deref() == Some(source_sha256)
        })
        .collect::<Vec<_>>();
    let current = format!("{state_prefix}/summary/complete");
    matches.sort_by_key(|summary| summary.reference != current);
    match matches.as_slice() {
        [] => bail!(
            "completed campaign has no local complete-summary ref for source {source_sha256}; restore or archive its durable summary before rendering a release"
        ),
        [summary] => Ok(*summary),
        [summary, ..] if summary.reference == current => Ok(*summary),
        _ => bail!(
            "completed campaign has multiple archived complete summaries for source {source_sha256}"
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
                LocalAttemptReceiptV1::Diagnosis { task_id, attempt } => {
                    ReleaseAttemptReceipt::Diagnosis {
                        task_id,
                        attempt: u64::from(attempt),
                        diagnosis: object["diagnosis"]
                            .as_str()
                            .expect("validated diagnosis is text")
                            .to_owned(),
                    }
                }
                LocalAttemptReceiptV1::Retry => ReleaseAttemptReceipt::Retry {
                    task_id: object["taskId"]
                        .as_str()
                        .expect("validated retry task is text")
                        .to_owned(),
                    attempt: object["attempt"]
                        .as_u64()
                        .expect("validated retry attempt is an integer"),
                    reason: object["reason"]
                        .as_str()
                        .expect("validated retry reason is text")
                        .to_owned(),
                },
                LocalAttemptReceiptV1::Escalation => ReleaseAttemptReceipt::Escalation,
                LocalAttemptReceiptV1::Pardon { tasks } => {
                    ReleaseAttemptReceipt::Pardon { sequence, tasks }
                }
            };
            records.push(record);
        }
        Ok((bytes, records))
    })();
    let unlock = FileExt::unlock(&file)
        .with_context(|| format!("cannot unlock attempt-receipts log {}", path.display()));
    let (bytes, records) = match (read, unlock) {
        (Err(error), _) => return Err(error),
        (Ok(_), Err(error)) => return Err(error),
        (Ok(value), Ok(())) => value,
    };
    Ok(ReleaseAttemptLog {
        path,
        present: true,
        bytes,
        records,
    })
}

fn release_attempt_facts(
    records: &[ReleaseAttemptReceipt],
    task_ids: &BTreeSet<String>,
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
            } if task_ids.contains(task_id) => diagnoses.push(DiagnosisFact {
                task_id: task_id.clone(),
                attempt: *attempt,
                diagnosis: diagnosis.clone(),
            }),
            ReleaseAttemptReceipt::Retry {
                task_id,
                attempt,
                reason,
            } if task_ids.contains(task_id) => retries.push(RetryFact {
                task_id: task_id.clone(),
                attempt: *attempt,
                reason: reason.clone(),
            }),
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
            | ReleaseAttemptReceipt::Escalation => {}
        }
    }
    (diagnoses, retries, warnings)
}

fn release_notes(
    registration: &CampaignRegistration,
    graph: &CanonicalCampaignGraphV1,
    refs: &[ReleaseGitRef],
    revisions: &BTreeMap<String, String>,
    merged_commits: &[(String, ReleaseCommit)],
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
        .map(|(task_id, merged)| {
            let branch = stable_publish_branch(
                &graph.manifest.name,
                &registration.registration_id,
                task_id,
                revisions.get(task_id).map(String::as_str),
            );
            let reference = format!("refs/heads/{branch}");
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
                None => (None, None, merged.message.clone()),
            };
            let header = commit_header(&message).to_owned();
            let fallback = titles.get(task_id.as_str()).copied().unwrap_or(task_id);
            let (kind, scope, breaking, summary) =
                validated_release_header(&message, &scopes, fallback);
            Ok(CampaignReleaseNote {
                task_id: task_id.clone(),
                commit: merged.object_id.clone(),
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
            .filter(|summary| {
                summary.reference != closing_summary.reference
                    && summary.reference.contains("/summary/archive/")
            })
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
        "Release notes".to_owned(),
    ];
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

fn arm_receipt(result: &Value, auto_pardons: &[AutoPardonReceipt], warnings: &[String]) -> Value {
    let mut value = if result.is_object() {
        result.clone()
    } else {
        json!({"result": result})
    };
    let object = value.as_object_mut().expect("arm receipt is an object");
    object.insert("autoPardons".to_owned(), json!(auto_pardons));
    let mut combined = object
        .remove("warnings")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    combined.extend(warnings.iter().map(|warning| json!(warning)));
    object.insert("warnings".to_owned(), Value::Array(combined));
    value
}

/// Every steering surface a pass reads: campaign-wide records plus records
/// addressed to one task. Task keys are task IDs, never task numbers.
#[derive(Debug, Clone, Default)]
struct CampaignSteering {
    master: Vec<Value>,
    tasks: BTreeMap<String, Vec<Value>>,
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
    let common = [
        "schemaVersion",
        "sequence",
        "kind",
        "campaign",
        "issueNumber",
    ];
    let (required, allowed): (Vec<&str>, BTreeSet<&str>) = match kind {
        "diagnosis" => {
            let specific = ["taskId", "attempt", "diagnosis", "redaction"];
            (
                common.into_iter().chain(specific).collect(),
                common.into_iter().chain(specific).collect(),
            )
        }
        "retry" => {
            let specific = ["taskId", "attempt", "reason", "redaction"];
            (
                common.into_iter().chain(specific).collect(),
                common.into_iter().chain(specific).collect(),
            )
        }
        "escalation" => {
            let specific = ["body"];
            (
                common.into_iter().chain(specific).collect(),
                common.into_iter().chain(specific).collect(),
            )
        }
        "pardon" => (
            common.into_iter().chain(["tasks"]).collect(),
            common
                .into_iter()
                .chain(["tasks", "reason", "actor", "nonce"])
                .collect(),
        ),
        _ => return Err(invalid(format!("{context} has unknown kind {kind:?}"))),
    };
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
    if object.get("schemaVersion").and_then(Value::as_u64) != Some(ATTEMPT_RECEIPTS_SCHEMA_VERSION)
        || object.get("sequence").and_then(Value::as_u64) != Some(expected_sequence)
        || object.get("campaign").and_then(Value::as_str) != Some(campaign)
        || object.get("issueNumber").and_then(Value::as_str) != Some(expected_issue_number.as_str())
    {
        return Err(invalid(format!(
            "{context} has invalid identity or sequence"
        )));
    }

    let task_attempt = |payload: &str, maximum: usize| -> Result<(String, u8)> {
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
        validate_attempt_receipt_text(
            object
                .get(payload)
                .expect("receipt payload is required above"),
            &format!("{context}.{payload}"),
            maximum,
        )?;
        Ok((task_id.to_owned(), attempt))
    };

    match kind {
        "diagnosis" => {
            let (task_id, attempt) = task_attempt("diagnosis", MAX_DIAGNOSIS_CHARS)?;
            Ok(LocalAttemptReceiptV1::Diagnosis { task_id, attempt })
        }
        "retry" => {
            task_attempt("reason", MAX_RETRY_CHARS)?;
            Ok(LocalAttemptReceiptV1::Retry)
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

fn active_escalated_tasks_from_receipts(
    records: &[LocalAttemptReceiptV1],
    current_tasks: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    #[derive(Default)]
    struct Escalation {
        contributors: BTreeSet<String>,
        covered: BTreeSet<String>,
    }

    let mut diagnoses = BTreeMap::<String, Vec<u8>>::new();
    let mut escalations = Vec::<Escalation>::new();
    for record in records {
        match record {
            LocalAttemptReceiptV1::Diagnosis {
                task_id, attempt, ..
            } if current_tasks.contains(task_id) => {
                diagnoses.entry(task_id.clone()).or_default().push(*attempt);
            }
            LocalAttemptReceiptV1::Diagnosis { .. } => {}
            LocalAttemptReceiptV1::Retry => {}
            LocalAttemptReceiptV1::Escalation => {
                let contributors = diagnoses
                    .iter()
                    .filter(|(_, attempts)| {
                        attempts.iter().copied().collect::<BTreeSet<_>>() == BTreeSet::from([1, 2])
                    })
                    .map(|(task_id, _)| task_id.clone())
                    .collect();
                escalations.push(Escalation {
                    contributors,
                    covered: BTreeSet::new(),
                });
            }
            LocalAttemptReceiptV1::Pardon { tasks: None, .. } => {
                diagnoses.clear();
                escalations.clear();
            }
            LocalAttemptReceiptV1::Pardon {
                tasks: Some(scope), ..
            } => {
                for task_id in scope {
                    diagnoses.remove(task_id);
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
        .filter(|task_id| current_tasks.contains(task_id))
        .collect())
}

fn active_local_escalated_tasks(
    state_dir: &Path,
    graph: &CampaignGraph,
) -> Result<BTreeSet<String>> {
    let current_tasks = graph
        .canonical
        .manifest
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<BTreeSet<_>>();
    let records = read_local_attempt_receipts(
        state_dir,
        &graph.canonical.manifest.name,
        LOCAL_CAMPAIGN_ISSUE_NUMBER,
    )?;
    active_escalated_tasks_from_receipts(&records, &current_tasks)
}

fn append_local_campaign_pardon(
    state_dir: &Path,
    graph: &CampaignGraph,
    actor: &str,
    reason: &str,
    scope: &PardonScope,
) -> Result<String> {
    let campaign = &graph.canonical.manifest.name;
    let issue_number = LOCAL_CAMPAIGN_ISSUE_NUMBER;
    let path = local_attempt_receipts_path(state_dir, campaign)?;
    let directory = path
        .parent()
        .expect("attempt-receipts path always has a parent");
    fs::create_dir_all(directory).with_context(|| {
        format!(
            "cannot create attempt-receipts directory {}",
            directory.display()
        )
    })?;
    let directory_metadata = fs::symlink_metadata(directory)?;
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        bail!(
            "attempt-receipts parent must be a real directory: {}",
            directory.display()
        );
    }
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;

    let normalized_reason = reason
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned();
    validate_attempt_receipt_text(
        &Value::String(normalized_reason.clone()),
        "campaign pardon reason",
        4_000,
    )?;
    validate_attempt_receipt_string(
        &Value::String(actor.to_owned()),
        "campaign pardon actor",
        128,
    )?;
    let nonce = uuid::Uuid::now_v7().to_string();
    let tasks = match scope {
        PardonScope::All => Value::Null,
        PardonScope::Tasks(tasks) => {
            if tasks.is_empty() || tasks.iter().any(|task_id| !safe_task_id(task_id)) {
                return Err(invalid("campaign pardon scope must name safe task ids"));
            }
            json!(tasks)
        }
    };

    let create = fs::OpenOptions::new()
        .create(true)
        .create_new(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path);
    let (mut file, created) = match create {
        Ok(file) => (file, true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = fs::OpenOptions::new()
                .read(true)
                .append(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)
                .with_context(|| format!("cannot open attempt-receipts log {}", path.display()))?;
            (file, false)
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot create attempt-receipts log {}", path.display()))
        }
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        bail!(
            "attempt-receipts log is not a private regular file: {}",
            path.display()
        );
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    FileExt::lock_exclusive(&file)
        .with_context(|| format!("cannot lock attempt-receipts log {}", path.display()))?;
    if created {
        file.sync_all()?;
        fs::File::open(directory)?.sync_all()?;
    }
    let append = (|| -> Result<String> {
        let records =
            read_local_attempt_receipts_locked(&mut file, &path, campaign, issue_number, true)?;
        let sequence = u64::try_from(records.len())?
            .checked_add(1)
            .ok_or_else(|| invalid("attempt-receipts sequence is exhausted"))?;
        let candidate = json!({
            "schemaVersion": ATTEMPT_RECEIPTS_SCHEMA_VERSION,
            "sequence": sequence,
            "kind": "pardon",
            "campaign": campaign,
            "issueNumber": issue_number.to_string(),
            "tasks": tasks,
            "reason": normalized_reason,
            "actor": actor,
            "nonce": nonce,
        });
        validate_local_attempt_receipt(candidate.clone(), &path, sequence, campaign, issue_number)?;
        let mut encoded = serde_json::to_vec(&candidate)?;
        encoded.push(b'\n');
        let final_size = file
            .metadata()?
            .len()
            .checked_add(u64::try_from(encoded.len())?)
            .ok_or_else(|| invalid("attempt-receipts log size is exhausted"))?;
        if final_size > MAX_ATTEMPT_RECEIPTS_LOG_BYTES {
            bail!("attempt-receipts log exceeds 128 MiB");
        }
        file.write_all(&encoded)?;
        file.sync_all()?;
        Ok(local_attempt_receipt_url(campaign, sequence))
    })();
    let unlock = FileExt::unlock(&file)
        .with_context(|| format!("cannot unlock attempt-receipts log {}", path.display()));
    match (append, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(receipt), Ok(())) => Ok(receipt),
    }
}

fn amendment_pardon_plan(
    prior: Option<&CanonicalCampaignGraphV1>,
    current: &CanonicalCampaignGraphV1,
    escalated: &BTreeSet<String>,
) -> (Vec<PlannedAutoPardon>, Vec<String>) {
    let prior_dependencies = prior
        .map(|graph| {
            graph
                .manifest
                .tasks
                .iter()
                .map(|task| {
                    (
                        task.id.as_str(),
                        task.dependencies
                            .iter()
                            .map(String::as_str)
                            .collect::<BTreeSet<_>>(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut pardons = Vec::new();
    let mut addressed = BTreeSet::new();
    for task in &current.manifest.tasks {
        if !escalated.contains(&task.id) {
            continue;
        }
        let Some(previous) = prior_dependencies.get(task.id.as_str()) else {
            continue;
        };
        let added_dependencies = task
            .dependencies
            .iter()
            .filter(|dependency| !previous.contains(dependency.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !added_dependencies.is_empty() {
            addressed.insert(task.id.clone());
            pardons.push(PlannedAutoPardon {
                task_id: task.id.clone(),
                added_dependencies,
            });
        }
    }
    let warnings = escalated
        .difference(&addressed)
        .map(|task_id| {
            format!("task {task_id} remains escalated; run tally campaign resume to unblock")
        })
        .collect();
    (pardons, warnings)
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
) -> Result<Option<String>> {
    let state = status["state"]
        .as_str()
        .ok_or_else(|| invalid("daemon returned campaign status without a state"))?;
    let current_nodes = status["currentNodes"]
        .as_array()
        .ok_or_else(|| invalid("daemon returned campaign status without a current-node table"))?;
    let running = status["counts"]["running"]
        .as_u64()
        .ok_or_else(|| invalid("daemon returned campaign status without a running-task count"))?;
    // `currentNodes` excludes the flow root; the campaign state includes it.
    if state == "running" || running != 0 || !current_nodes.is_empty() {
        return Ok(None);
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
            return Ok(Some(flow_run_id.to_owned()));
        }
    }
    Ok(None)
}

async fn campaign_poll_liveness_arm(
    host: CampaignHost<'_>,
    graph: &CampaignGraph,
    registration: &CampaignRegistration,
    observation: &str,
) -> Result<Option<String>> {
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
    let escalated = active_local_escalated_tasks(host.state_dir, graph)?;
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
    validate_gates(&policy.gates)
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
    committed: &CommittedLocalWorklist,
    repository: &CampaignRepository,
    code_repository: &str,
    adapters: &BTreeMap<String, AdapterConfig>,
) -> Result<Value> {
    let policy = parse_worklist_campaign_policy(&committed.document, &committed.source_path)?;
    if !adapters.contains_key(&policy.agent.adapter) {
        return Err(invalid(format!(
            "worklist campaign references unknown agent adapter {:?}",
            policy.agent.adapter
        )));
    }
    let pool = format!("campaign/{code_repository}");
    debug_assert!(is_campaign_pool_name(&pool));
    let steward = resolve_worklist_steward(&policy, adapters)?;
    Ok(json!({
        "schemaVersion": CAMPAIGN_SCHEMA_VERSION,
        "name": policy.name.expect("campaign name was defaulted above"),
        "repository": repository,
        "maxTasks": policy.max_tasks,
        "maxParallel": policy.max_parallel,
        "driverRuntimeMaxSec": policy.driver_runtime_max_sec,
        "runtimeMaxSec": policy.runtime_max_sec,
        "pool": pool,
        "mergeMethod": policy.merge_method,
        "agent": policy.agent,
        "steward": steward,
        "gates": policy.gates,
        "tasks": [],
    }))
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
    })
}

fn local_campaign_graph_from_worklist(
    repository: CampaignRepository,
    code_repository: &str,
    worklist_pattern: &str,
    adapters: &BTreeMap<String, AdapterConfig>,
) -> Result<CampaignGraph> {
    let committed = committed_local_worklist(&repository, worklist_pattern)?;
    let manifest_config =
        manifest_config_from_worklist(&committed, &repository, code_repository, adapters)?;
    let validated = validate_local_worklist_document(&committed.document, &manifest_config)?;
    if validated.manifest.repository.forge != "local" {
        return Err(invalid(
            "the local worklist arm path requires campaign.repository.forge=local",
        ));
    }
    local_campaign_graph(validated)
}

fn local_campaign_graph(validated: ValidatedWorklist) -> Result<CampaignGraph> {
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
    Ok(CampaignGraph {
        canonical,
        ownership_preflight_warnings,
    })
}

fn approved_graph_directory(state_dir: &Path, registration_id: &str) -> PathBuf {
    let scope = format!("{:x}", Sha256::digest(registration_id.as_bytes()));
    state_dir
        .join("campaigns/approved-graphs")
        .join(&scope[..32])
}

fn approved_graph_path(state_dir: &Path, registration: &CampaignRegistration) -> PathBuf {
    approved_graph_directory(state_dir, &registration.registration_id)
        .join(format!("{}.graph-v1.json", registration.arm_serial))
}

fn validated_graph_snapshot(
    snapshot: ApprovedGraphSnapshotV1,
    registration: &CampaignRegistration,
    path: &Path,
) -> Result<CanonicalCampaignGraphV1> {
    if snapshot.schema_version != APPROVED_GRAPH_SNAPSHOT_SCHEMA_VERSION
        || snapshot.registration_id != registration.registration_id
        || snapshot.arm_serial != registration.arm_serial
        || snapshot.graph.executable_digest != registration.approved_graph_digest
    {
        bail!(
            "campaign approved-graph snapshot {} disagrees with registration {} arm {}",
            path.display(),
            registration.registration_id,
            registration.arm_serial
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
    let path = approved_graph_path(state_dir, registration);
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
    validated_graph_snapshot(snapshot, registration, &path).map(Some)
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

fn prune_approved_graph_snapshots(
    state_dir: &Path,
    registration: &CampaignRegistration,
) -> Result<()> {
    let directory = approved_graph_directory(state_dir, &registration.registration_id);
    let expected = approved_graph_path(state_dir, registration);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path != expected && path.is_file() {
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

async fn dispatch_campaign(
    host: CampaignHost<'_>,
    graph: &CampaignGraph,
    repository_progress: &Value,
    registration: &mut CampaignRegistration,
    wait: bool,
    liveness_arm: Option<&str>,
) -> Result<Value> {
    let CampaignHost {
        socket,
        config_path,
        rpc_timeout,
        ..
    } = host;
    let manifest = &graph.canonical.manifest;
    if graph.canonical.executable_digest != registration.approved_graph_digest {
        bail!(
            "campaign executable graph changed from admitted {} to {}; inspect the worklist and run `tally campaign arm {} {}` to approve it",
            registration.approved_graph_digest,
            graph.canonical.executable_digest,
            registration.code_repository,
            registration.worklist_pattern,
        );
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
        repository,
        &code_repository,
        &worklist_pattern,
        &adapters,
    )?;
    let allowed_actors = normalize_allowed_actors(&args.allowed_actors, LOCAL_ALLOWED_ACTOR)?;
    let prior_graph = prior
        .as_ref()
        .map(|registration| read_approved_graph_snapshot(&state_dir, registration))
        .transpose()?
        .flatten();
    let escalated = if prior.is_none() {
        BTreeSet::new()
    } else {
        active_local_escalated_tasks(&state_dir, &graph)?
    };
    let (pardon_plan, mut arm_warnings) =
        amendment_pardon_plan(prior_graph.as_ref(), &graph.canonical, &escalated);
    // Arm-time conflictDomains warnings are advisory: they share the receipt
    // surface with host warnings but never participate in graph admission.
    arm_warnings.extend(graph.ownership_preflight_warnings.iter().cloned());
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
    let mut auto_pardons = Vec::with_capacity(pardon_plan.len());
    for pardon in &pardon_plan {
        let receipt =
            post_campaign_auto_pardon(&state_dir, &graph, &registration.local_actor, pardon)?;
        auto_pardons.push(AutoPardonReceipt {
            task_id: pardon.task_id.clone(),
            added_dependencies: pardon.added_dependencies.clone(),
            resume_receipt: receipt,
        });
    }
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
            serde_json::to_string(&arm_receipt(&receipt, &auto_pardons, &arm_warnings,))?
        );
        return Ok(());
    }
    let repository_progress = repository_progress_value(&graph)?;
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
    )
    .await?;
    registry.write(&mut registration)?;
    outln!(
        "{}",
        serde_json::to_string(&arm_receipt(&result, &auto_pardons, &arm_warnings,))?
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

fn auto_pardon_reason(pardon: &PlannedAutoPardon) -> String {
    let shown = pardon
        .added_dependencies
        .iter()
        .take(12)
        .map(|dependency| format!("`{dependency}`"))
        .collect::<Vec<_>>();
    let remainder = pardon.added_dependencies.len().saturating_sub(shown.len());
    let dependencies = if remainder == 0 {
        shown.join(", ")
    } else {
        format!("{}, and {remainder} more", shown.join(", "))
    };
    format!(
        "Re-armed graph added dependency {} to escalated task `{}`; the amendment is the operator's structural steering response.",
        dependencies, pardon.task_id
    )
}

fn post_campaign_auto_pardon(
    state_dir: &Path,
    graph: &CampaignGraph,
    actor: &str,
    pardon: &PlannedAutoPardon,
) -> Result<String> {
    let reason = auto_pardon_reason(pardon);
    let scope = PardonScope::Tasks(BTreeSet::from([pardon.task_id.clone()]));
    append_local_campaign_pardon(state_dir, graph, actor, &reason, &scope)
}

async fn run_campaign_resume(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: CampaignResumeArgs,
) -> Result<()> {
    let (code_repository, worklist_pattern) =
        campaign_identity(&args.code_repository, &args.worklist_pattern)?;
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let mut registration = registry
        .read_campaign(&code_repository, &worklist_pattern)?
        .ok_or_else(|| {
        invalid(format!(
            "campaign {code_repository}/{worklist_pattern} is not armed; arm it before attempting resume"
        ))
    })?;
    require_local_actor(&registration)?;
    let repository = campaign_repository_from_registration(&registration);
    let adapters = load_client_config(config_path)?.adapters;
    let graph = local_campaign_graph_from_worklist(
        repository,
        &code_repository,
        &worklist_pattern,
        &adapters,
    )?;
    let _ = validate_host(
        &graph,
        config_path,
        &registration.flow,
        &registration.driver,
    )?;

    let next_arm_serial = registration
        .arm_serial
        .checked_add(1)
        .ok_or_else(|| invalid("campaign resume counter is exhausted"))?;
    let prior_digest = registration.approved_graph_digest.clone();
    let receipt = append_local_campaign_pardon(
        &state_dir,
        &graph,
        &registration.local_actor,
        &args.reason,
        &PardonScope::All,
    )?;
    registration.arm_serial = next_arm_serial;
    registration.armed_at = Utc::now().to_rfc3339();
    registration.approved_graph_digest = graph.canonical.executable_digest.clone();
    // Publish the new authority before dispatch. Once this write succeeds, the
    // timer can recover an interrupted dispatch without another manual state
    // edit.
    write_approved_graph_snapshot(&state_dir, &registration, &graph.canonical)?;
    registry.write(&mut registration)?;
    prune_approved_graph_snapshots(&state_dir, &registration)?;

    let repository_progress = repository_progress_value(&graph)?;
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
    )
    .await?;
    registry.write(&mut registration)?;

    let mut output = if result.is_object() {
        result.clone()
    } else {
        json!({"result": result})
    };
    if let Some(object) = output.as_object_mut() {
        object.insert("status".to_owned(), json!("resumed"));
        object.insert("resumeReceipt".to_owned(), json!(receipt));
        object.insert("reason".to_owned(), json!(args.reason.trim()));
        object.insert("priorGraphDigest".to_owned(), json!(prior_digest));
        object.insert(
            "graphDigest".to_owned(),
            json!(registration.approved_graph_digest),
        );
    }
    outln!("{}", serde_json::to_string(&output)?);
    if args.wait {
        let code = waited_exit_code(&result);
        if code != 0 {
            return Err(anyhow::Error::new(ExitFailure {
                code,
                message: "campaign resumed, but its reconcile pass returned a non-zero verdict"
                    .to_owned(),
            }));
        }
    }
    Ok(())
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
        let attempt = async {
            require_local_actor(&registration)?;
            let repository = campaign_repository_from_registration(&registration);
            let graph = local_campaign_graph_from_worklist(
                repository,
                &registration.code_repository,
                &registration.worklist_pattern,
                &adapters,
            )?;
            if graph.canonical.executable_digest != registration.approved_graph_digest {
                return Ok(CampaignPollAttempt::RearmRequired {
                    approved_graph_digest: registration.approved_graph_digest.clone(),
                    live_graph_digest: graph.canonical.executable_digest.clone(),
                });
            }
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
                let Some(flow_run_id) =
                    campaign_poll_liveness_arm(host, &graph, &registration, &observation).await?
                else {
                    return Ok(CampaignPollAttempt::Unchanged);
                };
                Some(flow_run_id)
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
        let registration_path = path.display().to_string();
        let event = match attempt {
            Ok(CampaignPollAttempt::Dispatched) => CampaignPollEvent::new(
                &registration.registration_id,
                &event_issue,
                &registration_path,
                CampaignPollStatus::Dispatched,
            ),
            Ok(CampaignPollAttempt::Unchanged) => CampaignPollEvent::new(
                &registration.registration_id,
                &event_issue,
                &registration_path,
                CampaignPollStatus::Unchanged,
            ),
            Ok(CampaignPollAttempt::RearmRequired {
                approved_graph_digest,
                live_graph_digest,
            }) => CampaignPollEvent::graph_change(
                &registration.registration_id,
                &event_issue,
                &registration_path,
                CampaignPollStatus::RearmRequired,
                approved_graph_digest,
                live_graph_digest,
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
        };
        outln!("{}", serde_json::to_string(&event)?);
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
    let status = reconciled_campaign_status(
        &client,
        rpc_timeout,
        latest,
        registration.as_ref(),
        approved_graph.as_ref(),
        &code_repository,
        &worklist_pattern,
    )
    .await?;
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

fn run_campaign_quiescent(args: CampaignQuiescentArgs) -> Result<()> {
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let values = campaign_list_values(&registry)?;
    if values.is_empty() {
        return Ok(());
    }

    errln!("{}", serde_json::to_string(&values)?);
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

#[cfg(test)]
mod tests {
    use super::*;
    use tally_core::campaign_contract::{
        validate_manifest, BRIEF_SENTINEL, DEFAULT_AGENT_SANDBOX_POLICY,
    };

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
printf '%s' "$1" >> '{calls}'
shift
for argument in "$@"; do printf '\t%s' "$argument" >> '{calls}'; done
printf '\n' >> '{calls}'
if test -f '{fail_on}' && test "$(cat '{fail_on}' | tr -d '\n')" = "$count"; then
  printf 'injected failure %s\n' "$count" >&2
  exit 23
fi
"#,
                calls = calls.display(),
                count = count.display(),
                fail_on = fail_on.display(),
            ),
        )
        .unwrap();
        std::os::unix::fs::symlink(SHELL_COMMAND_PROVIDER, &program).unwrap();
        program
    }

    #[test]
    fn completed_fixture_campaign_renders_a_read_only_release_plan() {
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

        let campaign = "fixture-release";
        let registration_id = "0198a62b-41ee-7000-8000-000000000777";
        let worklist = "specs/release.json";
        let repository = "acme/widgets";
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
        let complete_ref = format!("{state_prefix}/summary/complete");
        release_fixture_git(&checkout, &["update-ref", &complete_ref, &complete_object]);
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
                    "schemaVersion": ATTEMPT_RECEIPTS_SCHEMA_VERSION,
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
    fn amendment_pardons_only_escalated_tasks_that_gained_a_dependency() {
        let prior = canonical_graph_for_pardon(&[]);
        let amended = canonical_graph_for_pardon(&["prerequisite"]);
        let escalated = BTreeSet::from(["task-a".to_owned(), "task-b".to_owned()]);

        let (pardons, warnings) = amendment_pardon_plan(Some(&prior), &amended, &escalated);
        assert_eq!(
            pardons,
            [PlannedAutoPardon {
                task_id: "task-a".to_owned(),
                added_dependencies: vec!["prerequisite".to_owned()],
            }]
        );
        assert_eq!(
            warnings,
            ["task task-b remains escalated; run tally campaign resume to unblock"]
        );

        let receipt = arm_receipt(
            &json!({"status": "armed"}),
            &[AutoPardonReceipt {
                task_id: pardons[0].task_id.clone(),
                added_dependencies: pardons[0].added_dependencies.clone(),
                resume_receipt: "local://campaign/night-build/attempt-receipts/7".to_owned(),
            }],
            &warnings,
        );
        assert_eq!(receipt["autoPardons"][0]["taskId"], json!("task-a"));
        assert_eq!(
            receipt["autoPardons"][0]["addedDependencies"],
            json!(["prerequisite"])
        );
        assert_eq!(
            receipt["autoPardons"][0]["resumeReceipt"],
            json!("local://campaign/night-build/attempt-receipts/7")
        );
        assert_eq!(receipt["warnings"], json!(warnings));
    }

    #[test]
    fn local_attempt_receipts_drive_escalation_and_scoped_pardons() {
        let temporary = tempfile::tempdir().unwrap();
        let campaign = "night-build";
        let issue_number = LOCAL_CAMPAIGN_ISSUE_NUMBER;
        let path = local_attempt_receipts_path(temporary.path(), campaign).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let diagnosis = |sequence: u64, task_id: &str, attempt: u8| {
            json!({
                "schemaVersion": ATTEMPT_RECEIPTS_SCHEMA_VERSION,
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
                "schemaVersion": ATTEMPT_RECEIPTS_SCHEMA_VERSION,
                "sequence": 5,
                "kind": "escalation",
                "campaign": campaign,
                "issueNumber": issue_number.to_string(),
                "body": "The local frontier is quiescent.",
            }),
            json!({
                "schemaVersion": ATTEMPT_RECEIPTS_SCHEMA_VERSION,
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
        let current = BTreeSet::from(["finish".to_owned(), "foundation".to_owned()]);
        assert_eq!(
            active_escalated_tasks_from_receipts(&loaded, &current).unwrap(),
            BTreeSet::from(["finish".to_owned()])
        );

        let graph = local_graph_for_test();
        let receipt = append_local_campaign_pardon(
            temporary.path(),
            &graph,
            "uid:1000",
            "The amended graph now addresses the failed task.",
            &PardonScope::Tasks(BTreeSet::from(["finish".to_owned()])),
        )
        .unwrap();
        assert_eq!(receipt, "local://campaign/night-build/attempt-receipts/7");
        let repaired = fs::read_to_string(&path).unwrap();
        assert!(!repaired.contains("interrupted"));
        let loaded = read_local_attempt_receipts(temporary.path(), campaign, issue_number).unwrap();
        assert_eq!(loaded.len(), 7);
        assert!(
            active_escalated_tasks_from_receipts(&loaded, &current)
                .unwrap()
                .is_empty(),
            "the two scoped pardons jointly cover the escalation contributors"
        );

        let removed_task_history = [
            LocalAttemptReceiptV1::Diagnosis {
                task_id: "removed".to_owned(),
                attempt: 1,
            },
            LocalAttemptReceiptV1::Diagnosis {
                task_id: "removed".to_owned(),
                attempt: 2,
            },
            LocalAttemptReceiptV1::Escalation,
            LocalAttemptReceiptV1::Pardon {
                tasks: Some(BTreeSet::from(["removed".to_owned()])),
            },
            LocalAttemptReceiptV1::Escalation,
        ];
        assert!(
            active_escalated_tasks_from_receipts(&removed_task_history, &BTreeSet::new()).is_err(),
            "removed tasks are dropped before escalation causality is folded"
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
                .unwrap()
                .as_deref(),
            Some(flow_run_id),
            "an unchanged resting campaign must wake when work is dispatchable"
        );

        let mut retryable = resting.clone();
        retryable["counts"]["blocked"] = json!(1);
        retryable["counts"]["pending"] = json!(0);
        retryable["tasks"][0]["status"] = json!("blocked");
        assert_eq!(
            dispatchable_poll_liveness_arm(&graph, registration_id, &BTreeSet::new(), &retryable,)
                .unwrap()
                .as_deref(),
            Some(flow_run_id),
            "a direct failure remains dispatchable until its escalation is active"
        );

        let mut live = resting.clone();
        live["state"] = json!("running");
        live["counts"]["running"] = json!(1);
        live["currentNodes"] = json!([{"state": "pending"}]);
        assert_eq!(
            dispatchable_poll_liveness_arm(&graph, registration_id, &BTreeSet::new(), &live,)
                .unwrap(),
            None,
            "a live pass already owns campaign progress"
        );

        let escalated = BTreeSet::from(["foundation".to_owned()]);
        assert_eq!(
            dispatchable_poll_liveness_arm(&graph, registration_id, &escalated, &resting).unwrap(),
            None,
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
            None,
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
            None,
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

    fn local_graph_for_test() -> CampaignGraph {
        let manifest = serde_json::from_value(manifest_value_for_test(json!([{
            "id": "foundation",
            "kind": "implementation",
            "issue": 43,
            "dependencies": [],
            "conflictDomains": []
        }])))
        .unwrap();
        CampaignGraph {
            canonical: CanonicalCampaignGraphV1::new(
                manifest,
                vec![CanonicalCampaignTaskV1 {
                    number: 43,
                    title: "Foundation".to_owned(),
                    body: "Build the foundation.".to_owned(),
                }],
            )
            .unwrap(),
            ownership_preflight_warnings: Vec::new(),
        }
    }

    fn canonical_graph_for_pardon(task_a_dependencies: &[&str]) -> CanonicalCampaignGraphV1 {
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
        let config =
            manifest_config_from_worklist(&committed, &repository, "acme/widgets", &adapters)
                .unwrap();
        assert_eq!(config["steward"]["argv"], json!(["narrator", "--json"]));
        assert_eq!(
            config["steward"]["env"]["NARRATOR_ENDPOINT"],
            "https://narrator.invalid/v1"
        );
        assert_eq!(config["steward"]["finalMessagePattern"], "^NARRATION=(.*)$");
        assert_eq!(config["steward"]["runtimeMaxSec"], 120);

        adapters.get_mut("narrator").unwrap().hardening = AdapterHardening::Strict;
        let failure =
            manifest_config_from_worklist(&committed, &repository, "acme/widgets", &adapters)
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
        let failure =
            manifest_config_from_worklist(&committed, &repository, "acme/widgets", &adapters)
                .unwrap_err()
                .to_string();
        assert!(failure.contains("non-empty stdout regex"), "{failure}");
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
        let manifest_config =
            manifest_config_from_worklist(&committed, &repository, "acme/widgets", &adapters)
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
        let graph = local_campaign_graph(validated).unwrap();
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
            &[],
            &graph.ownership_preflight_warnings,
        );
        assert_eq!(
            receipt["warnings"],
            json!(graph.ownership_preflight_warnings)
        );

        let mut forge_field = document.clone();
        forge_field["campaign"]["label"] = json!("must-not-be-accepted");
        let failure = manifest_config_from_worklist(
            &CommittedLocalWorklist {
                document: forge_field,
                source_path: committed.source_path.clone(),
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
            &CommittedLocalWorklist {
                document: unknown_agent,
                source_path: committed.source_path,
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
    fn arm_argv_validation_matches_the_driver() {
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
        let rust_acceptance = cases
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

        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let driver = repo_root.join("drivers/spec_build_driver.py");
        let input_dir = tempfile::tempdir().unwrap();
        let input = input_dir.path().join("campaign-policy-cases.json");
        fs::write(&input, serde_json::to_vec(&cases).unwrap()).unwrap();
        let script = r#"
import importlib.util
import json
import sys

spec = importlib.util.spec_from_file_location("spec_build_driver", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with open(sys.argv[2], encoding="utf-8") as handle:
    cases = json.load(handle)
accepted = []
for case in cases:
    try:
        module.normalize_worklist_campaign(case, "specs/night/epsilon.json")
    except module.DriverError:
        accepted.append(False)
    else:
        accepted.append(True)
print(json.dumps(accepted))
"#;
        let output = ProcessCommand::new("python3")
            .args(["-c", script])
            .arg(&driver)
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "driver policy parity probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let driver_acceptance: Vec<bool> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(rust_acceptance, driver_acceptance);
    }

    #[test]
    fn canonical_digest_matches_the_driver_contract() {
        let value = json!({"z": [1, "é"], "a": {"b": true, "a": null}});
        assert_eq!(
            sha256_json(&value).unwrap(),
            "sha256:356741b14061aca3cb3e9abc01fe332af042dfcd59d81c56ee9fb57832dc6429"
        );
    }

    /// Rust admits, normalizes, and hashes the graph once. The packaged Python
    /// driver must consume those exact bytes even when the operator spelled a
    /// checkout through a symlink or `..`, and a minimal explicit steward must
    /// already contain every default before it crosses the boundary.

    #[test]
    fn digest_mismatch_receipt_names_both_digests_and_the_first_divergent_path() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let driver = repo_root.join("drivers/spec_build_driver.py");
        assert!(
            driver.is_file(),
            "packaged driver missing: {}",
            driver.display()
        );

        // One agent, shared shape; the live side carries exactly one extra
        // nested key, exactly the #429 skew shape.
        let agent = json!({
            "adapter": "codex",
            "argv": [BRIEF_SENTINEL],
            "priority": "low",
            "runtimeMaxSec": 14_400,
            "approvalPolicy": "never",
            "sandboxPolicy": "danger-full-access",
            "model": null
        });
        let armed_agent = agent.clone();
        let mut live_agent = agent.clone();
        live_agent["diagnosisSandboxPolicy"] = json!("read-only");
        let manifest = |agent: Value| {
            json!({
                "schemaVersion": 1,
                "name": "parity",
                "agent": agent,
                "tasks": []
            })
        };
        let armed = manifest(armed_agent);
        let live = manifest(live_agent);
        let tasks = json!([]);

        let input_dir = tempfile::tempdir().unwrap();
        let input_path = input_dir.path().join("divergence.json");
        fs::write(
            &input_path,
            serde_json::to_string(&json!({"armed": armed, "live": live, "tasks": tasks})).unwrap(),
        )
        .unwrap();
        let script = r#"
import importlib.util
import json
import sys

spec = importlib.util.spec_from_file_location("spec_build_driver", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with open(sys.argv[2], encoding="utf-8") as handle:
    data = json.load(handle)
tasks = data["tasks"]
armed_digest = module.canonical_sha256({"manifest": data["armed"], "tasks": tasks})
live_digest = module.canonical_sha256({"manifest": data["live"], "tasks": tasks})
receipt = module.graph_digest_mismatch_receipt(
    data["armed"], data["live"], armed_digest, live_digest
)
path = module.first_divergent_canonical_path(data["armed"], data["live"])
print(json.dumps({
    "armedDigest": armed_digest,
    "liveDigest": live_digest,
    "receipt": receipt,
    "path": path,
}))
"#;
        let output = std::process::Command::new("python3")
            .args(["-c", script])
            .arg(&driver)
            .arg(&input_path)
            .output()
            .expect("python3 must run the packaged driver for the divergence test");
        assert!(
            output.status.success(),
            "packaged driver divergence probe failed (status {:?}):\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let probe: Value = serde_json::from_str(
            &String::from_utf8(output.stdout).expect("divergence probe must print UTF-8"),
        )
        .expect("divergence probe must print JSON");

        let armed_digest = probe["armedDigest"].as_str().unwrap();
        let live_digest = probe["liveDigest"].as_str().unwrap();
        let receipt = probe["receipt"].as_str().unwrap();
        assert_ne!(armed_digest, live_digest);
        assert_eq!(
            probe["path"].as_str().unwrap(),
            "agent.diagnosisSandboxPolicy: absent-in-armed / present-in-live"
        );
        // Both digests, in the arm CLI's `sha256:` form.
        assert!(receipt.contains(armed_digest), "{receipt}");
        assert!(receipt.contains(live_digest), "{receipt}");
        // The first divergent canonical path, prefixed under the manifest.
        assert!(
            receipt.contains(
                "manifest.agent.diagnosisSandboxPolicy: absent-in-armed / present-in-live"
            ),
            "{receipt}"
        );
        // The existing instruction survives: this adds evidence, it does not
        // change the verdict.
        assert!(
            receipt.contains("inspect it and explicitly re-arm"),
            "{receipt}"
        );
        // The receipt must not widen what it publishes: the withheld value
        // never appears.
        assert!(!receipt.contains("read-only"), "{receipt}");
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
        let prior = canonical_graph_for_pardon(&[]);
        let amended = canonical_graph_for_pardon(&["prerequisite"]);
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
            !old_path.exists(),
            "the superseded graph generation must be pruned"
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
    fn campaign_defaults_are_a_pairing_a_codex_agent_can_commit_under() {
        let adapter = codex_shaped_adapter(&["danger-full-access"]);
        // The shipped module defaults, unmodified.
        let defaults = agent_with(Some(DEFAULT_AGENT_SANDBOX_POLICY));
        assert_eq!(defaults.approval_policy.as_deref(), Some("never"));
        validate_agent_policies(&defaults, &adapter).unwrap();

        // The estate workaround already deployed by the consumer: both values
        // explicit, approval disabled outright.
        let workaround = CampaignAgent {
            approval_policy: None,
            ..agent_with(Some("danger-full-access"))
        };
        validate_agent_policies(&workaround, &adapter).unwrap();
    }

    #[test]
    fn missing_agent_final_message_capture_warns_before_worker_findings_are_lost() {
        let agent = agent_with(Some(DEFAULT_AGENT_SANDBOX_POLICY));
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
