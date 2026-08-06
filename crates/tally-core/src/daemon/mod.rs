#![allow(clippy::disallowed_macros)]
// The daemon's stdout/stderr is its log surface, read by journald rather than
// by an operator's pipeline. `cli::out`'s hang-up mapping is a CLI contract and
// deliberately does not reach here (#315).

mod barriers;
mod completion;
mod notify;
mod rpc;
mod run;
mod startup;
mod supervise;
mod witness_view;

pub use barriers::BarrierTracker;
pub use notify::SystemdNotifier;
pub use supervise::{
    spawn_supervised, SupervisedFactory, SupervisedFuture, SupervisedTask, SupervisionEvent,
};

use crate::wire::{method_class, MethodClass};
#[cfg(test)]
pub(crate) use barriers::WaitRegistration;
pub(crate) use barriers::{await_registration, parse_job_barrier, single_job_barrier_value};
#[cfg(test)]
use completion::execution_request;
use completion::hash_job_token;
use completion::{
    accounting_witness_fields, append_context_witness, append_daemon_witness, canonical_job_model,
    canonical_verdict, completed_event, effective_gate_manifest, enqueued_event,
    execution_fact_for_termination, finalize_forced_locked, forced_witness,
    lock_gcroot_registration, release_child_charge, substituted_witness, GhTerminalWork,
    TerminalWork,
};
use completion::{append_orphan_attestation, append_orphan_retraction, gh_completion_id};
pub(crate) use notify::WatchdogKeepalive;
#[cfg(test)]
use notify::{
    dispatch_stall_horizon, dispatch_stall_notice, keepalive_cadence, keepalive_verdict,
    KeepaliveVerdict,
};
use rpc::control::{find_job, lease_request, lease_wire, state_name};
#[cfg(test)]
use rpc::producer::{pool_loss_intent_directory, read_pool_loss_intent, write_pool_loss_intent};
use rpc::producer::{reconcile_pool_loss_intents, PoolTransitionTask};
use rpc::query::{feed_scraped_usage, query_row};
#[cfg(test)]
use rpc::query::{
    overlay_live_states, read_usage_meter, usage_meter_event_path, write_usage_meter,
    UsageMeterObservation,
};
use run::{
    merge_selected_pool_returns, pool_representations, promoted_jobs, renderable_pool_return_rows,
    resume_paused_jobs_locked,
};
#[cfg(test)]
use run::{DispatchStallHook, LeaseTickHook};
#[cfg(test)]
use startup::{
    acquire_daemon_lock, hydrate_adopted_adapter_metadata, hydrate_completed_adapter_metadata,
    prepare_paths, reconcile_reuse_witnesses, recovered_model_is_advisory,
    verified_adapter_attestation_captures,
};
use startup::{
    hydrate_represent_adapter_metadata, install_recovery_jobs, recovery_adapter_invocation,
    DaemonLockGuard,
};
use witness_view::WitnessView;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex, RwLock};
use tokio::task::{JoinHandle, LocalSet};

use crate::adapters::{
    provisions_gate_manifest, AdapterConfig, AdapterEngine, AdapterError, AdapterInvocation,
    ScrapeResult,
};
use crate::brief::{self, PreparedBrief};
use crate::completion::{
    evaluate_completion, ExecutionFact, ExecutionStatus, GateManifestSpec, GateSummaryStatus,
    SemanticCompletion,
};
use crate::config::{Config, GitAiConfig, PoolPredicate, Priority, ResourceKind};
use crate::evidence::{
    parse_evidence_specs, probe_dedup, probe_full_pass, run_evidence_gate, CheckOutcome,
    DedupMissReason, RetryTrigger, RunOutcome,
};
use crate::exec_attestation::ExecAttestationContext;
use crate::executor::{
    ExecutionIdentity, ExecutionOutcome, ExecutionRequest, ExecutionTermination, Executor,
    ExecutorError, UnitAccounting, UnitLimits, Uuid,
};
use crate::flow_lineage::{
    canonical_flow_run_id, record_supersede, FlowLineage, FlowLineageError, PredecessorPins,
    SupersedeReason,
};
use crate::flow_membership::{
    record_membership, FlowMembership, FlowMembershipError, FlowMembershipRecord,
    MembershipDisposition, MembershipWrite,
};
use crate::git_ai::GitAiExecution;
use crate::history::{HistoryError, LifecycleStore};
use crate::journal::{EmitEvent, JournalEmitter, JournalEntry, TallyEvent};
use crate::lease::{
    bump_epoch, AdmitOutcome, LeaseBackend, LeaseEngine, LeaseError, LeaseEventLog, LeaseGrant,
    LeaseRequest, LeaseSchedulingGroup, LocalLease, SystemdUnitLiveness,
};
use crate::nix_store::{DerivationAvailability, NixStore};
use crate::occupancy;
use crate::pagination::{PageCache, PaginationError};
use crate::producer_query::query_producers;
use crate::producers::{
    acknowledged_ingress_ids, archive_ingress_claim, claim_ingress_files, read_ingress_payload,
    read_orphaned_projections, GhCliMutationSink, GhCompletionProjection, GhProjectionOutcome,
    IngressOutcome, OrphanedProjection, OrphanedProjectionKind, OrphanedProjections,
    ProducerEngine, ProducerError, ReachabilityTransition, ORPHANED_PROJECTION_SCHEMA_VERSION,
};
use crate::provenance::{Orchestration, TaskRef, DEFAULT_FLOW_MAX_NODES};
use crate::query::{
    query_pools, query_render, query_standup, query_status, JobProjection, PoolHeadroomFact,
    RenderScope, RowFact, RowStatus, StandupOptions, WindowConsumptionFact,
};
use crate::query_v2::{
    apply_run_lineage, apply_standup_usage, collapse_lifecycle_echoes, log_position_floor,
    log_position_head, query_flow_proofs, query_job as query_job_v2, query_jobs as query_jobs_v2,
    query_lifecycle_log, query_proof, query_run, snapshot_metadata, JobsFilter, LifecycleLogFilter,
    LiveJobFact, LogPosition, ObservabilityError, PositionGap, RowDetailFact,
};
use crate::recovery::{
    collect_durable_recovery_facts, collect_local_unit_facts, recover, DurableRecoveryFacts,
    RecoveryAction, RecoveryFacts, RecoveryIdentity, RecoveryPolicy, RecoveryRowState,
    RecoveryTriggers,
};
use crate::retention::{
    acquire_registration_lock, gcroots_lock_path, parse_horizon, reconcile_recent_roots,
    register_record_roots, GcRootsLock, RetentionError,
};
use crate::storage::{StorageError, StorageMetrics, StorageMonitor, StorageWarningRecord};
use crate::taskdb::{
    admits_durable_row, migrate_acknowledged_events, read_acknowledged_events,
    update_enqueue_event_atomic, write_enqueue_event_atomic, AdmissionInput, AdmissionOrigin,
    DurableEnqueueEvent, DurableRetry, EnqueueSource, RowSeed, TaskDbError,
};
use crate::trace::{query_trace, trace_availability, TraceError, TraceLane};
use crate::usage::UsageObservation;
use crate::usage_rollup::AttestationEvidence;
use crate::watch::{ChangeError, ChangeKind, ChangeStore};
use crate::wire::{
    canonical_payload_hash, serve_connection_with_limits, EnqueuePayload, GuardrailConfig,
    GuardrailState, ParentInfo, ProducerDefaults, RequestFrame, RpcHandler, SubmissionMode,
    WireError, WireErrorCode, WireIoError,
};
use crate::witness::{
    current_host_id, read_verified_attestations, read_verified_records, AttestationLedger,
    AttestationRecord, AttestationVerifyReport, Charge, Derivation, LaborClass, Verdict,
    WitnessBody, WitnessError, WitnessLedger, WitnessRecord,
};

/// The daemon's one cached handle per advisory attestation chain.
///
/// The ledger opens lazily so that a corrupt or unopenable chain keeps its
/// pre-existing advisory semantics: each caller sees the error and decides
/// whether to log, skip, or fail its own operation, and startup never
/// fail-stops on it. Once open, appends and reads reuse the verified head
/// instead of rescanning the whole chain.
pub(crate) struct SharedAttestations {
    path: PathBuf,
    ledger: Option<AttestationLedger>,
}

impl SharedAttestations {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path, ledger: None }
    }

    pub(crate) fn ledger(&mut self) -> Result<&mut AttestationLedger, WitnessError> {
        if self.ledger.is_none() {
            self.ledger = Some(AttestationLedger::open(&self.path)?);
        }
        Ok(self.ledger.as_mut().expect("ledger opened above"))
    }
}

/// A map that serves cheap Arc-shared value snapshots per mutation epoch.
///
/// Fresh query envelopes previously deep-cloned every row projection per
/// query; this rebuilds the Vec only after a mutation and hands queries a
/// clone of the Arc. Read-only access passes through Deref; the mutating
/// methods invalidate the cached snapshot.
pub(crate) struct SnapshotMap<V> {
    map: BTreeMap<Uuid, V>,
    cached: Option<Arc<Vec<V>>>,
}

impl<V> std::ops::Deref for SnapshotMap<V> {
    type Target = BTreeMap<Uuid, V>;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl<V> From<BTreeMap<Uuid, V>> for SnapshotMap<V> {
    fn from(map: BTreeMap<Uuid, V>) -> Self {
        Self { map, cached: None }
    }
}

impl<V: Clone> SnapshotMap<V> {
    pub(crate) fn insert(&mut self, key: Uuid, value: V) -> Option<V> {
        self.cached = None;
        self.map.insert(key, value)
    }

    pub(crate) fn get_mut(&mut self, key: &Uuid) -> Option<&mut V> {
        self.cached = None;
        self.map.get_mut(key)
    }

    pub(crate) fn snapshot(&mut self) -> Arc<Vec<V>> {
        Arc::clone(
            self.cached
                .get_or_insert_with(|| Arc::new(self.map.values().cloned().collect())),
        )
    }
}

const LEASE_TICK: Duration = Duration::from_millis(100);
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);
const RPC_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_METER_EVENT_BYTES: u64 = 64 * 1024;
const UNCLAIMED_DRAIN_BARRIER_LIMIT: usize = 64;
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;

#[derive(Debug, Clone)]
pub struct DaemonPaths {
    pub socket: PathBuf,
    pub state_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl DaemonPaths {
    pub fn events_dir(&self) -> PathBuf {
        self.state_dir.join("events")
    }

    pub fn witness_path(&self) -> PathBuf {
        self.data_dir.join("witness.jsonl")
    }

    pub fn attestations_path(&self) -> PathBuf {
        self.data_dir.join("attestations.jsonl")
    }

    pub fn gcroots_dir(&self) -> PathBuf {
        self.data_dir.join("gcroots")
    }

    /// Durable predecessor/successor lineage for flow runs. Data, not state:
    /// a rollover recorded here outlives every restart and reinstall of the
    /// supervisor that asked for it.
    pub fn flow_lineage_path(&self) -> PathBuf {
        self.data_dir.join(crate::flow_lineage::FLOW_LINEAGE_FILE)
    }

    pub fn flow_membership_path(&self) -> PathBuf {
        self.data_dir
            .join(crate::flow_membership::FLOW_MEMBERSHIP_FILE)
    }

    pub fn lifecycle_path(&self) -> PathBuf {
        self.data_dir.join(crate::history::LIFECYCLE_FILE)
    }

    pub fn brief_path(&self, hash: &str) -> Result<PathBuf, crate::brief::BriefError> {
        brief::content_path(&self.data_dir, hash)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DaemonSettings {
    pub unit_limits: UnitLimits,
    pub yield_grace: Duration,
    pub recovery_policy: RecoveryPolicy,
    pub max_connections: usize,
}

impl DaemonSettings {
    pub fn validate(self) -> Result<Self, DaemonError> {
        if !(1..=10_000).contains(&self.unit_limits.cpu_weight) {
            return Err(DaemonError::Invalid(
                "CPUWeight must be in 1..=10000".to_owned(),
            ));
        }
        if self.unit_limits.memory_max_bytes == 0 || self.unit_limits.memory_max_bytes == u64::MAX {
            return Err(DaemonError::Invalid(
                "MemoryMax must be positive and finite".to_owned(),
            ));
        }
        if self.yield_grace.is_zero() {
            return Err(DaemonError::Invalid(
                "yield grace must be positive".to_owned(),
            ));
        }
        if self.recovery_policy.max_attempts == 0 {
            return Err(DaemonError::Invalid(
                "recovery maxAttempts must be positive".to_owned(),
            ));
        }
        if self.max_connections == 0 {
            return Err(DaemonError::Invalid(
                "max connections must be positive".to_owned(),
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("invalid daemon configuration: {0}")]
    Invalid(String),
    #[error(
        "state directory {path} is not a real directory; replace it with a real directory and move the state files into it before starting tally"
    )]
    InvalidStateDirectory { path: PathBuf },
    #[error("daemon I/O error at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("wire error: {0}")]
    Wire(#[from] WireIoError),
    #[error("lease error: {0}")]
    Lease(#[from] LeaseError),
    #[error("task database error: {0}")]
    TaskDb(#[from] TaskDbError),
    #[error("witness error: {0}")]
    Witness(#[from] WitnessError),
    #[error("retention error: {0}")]
    Retention(#[from] RetentionError),
    #[error(
        "old-format events directory at {path}; archive it aside before first boot: mv -- {path} {archive}"
    )]
    OldFormatEvents { path: PathBuf, archive: PathBuf },
    #[error("lifecycle history error: {0}")]
    History(#[from] HistoryError),
    #[error("change log error: {0}")]
    Change(#[from] ChangeError),
    #[error("storage monitor error: {0}")]
    Storage(#[from] StorageError),
    #[error("recovery error: {0}")]
    Recovery(#[from] crate::recovery::RecoveryError),
    #[error("executor error: {0}")]
    Executor(#[from] ExecutorError),
    #[error("adapter error: {0}")]
    Adapter(#[from] AdapterError),
    #[error("sd_notify failed: {0}")]
    Notify(String),
}

fn io_error(path: &Path, source: io::Error) -> DaemonError {
    DaemonError::Io {
        path: path.to_owned(),
        source,
    }
}

fn retryable_accept_error(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ECONNABORTED | libc::EINTR)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckStage {
    Admission,
    LeaseGrant,
    VerdictWitness,
}

pub const FSYNC_BEFORE_ACK_STAGES: &[AckStage] = &[
    AckStage::Admission,
    AckStage::LeaseGrant,
    AckStage::VerdictWitness,
];

#[derive(Debug, Clone, PartialEq)]
pub struct JobResult {
    pub task_uuid: Option<String>,
    pub task_ref: Option<TaskRef>,
    pub job_id: String,
    pub verdict: Verdict,
    pub exit_code: i32,
    pub artifact_content_hash: Option<String>,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub witness_seq: u64,
    pub model: Option<String>,
    pub completion: Option<SemanticCompletion>,
    pub stderr_excerpt: Option<crate::executor::CaptureExcerpt>,
    /// The same value just appended to the witness record: `None` unless
    /// this attempt actually measured a declared GPU pool's main-process
    /// runtime. Carried here so the completion lifecycle event can report it
    /// truthfully instead of inventing a zero.
    pub gpu_seconds: Option<f64>,
}

impl JobResult {
    fn value(&self) -> Value {
        let mut value = json!({
            "task_uuid": self.task_uuid,
            "job_id": self.job_id,
            "verdict": self.verdict,
            "exit_code": self.exit_code,
            "artifact_content_hash": self.artifact_content_hash,
            "attempt": self.attempt,
            "lease_epoch": self.lease_epoch,
            "witness_seq": self.witness_seq,
        });
        if let Some(task_ref) = &self.task_ref {
            value["taskRef"] = Value::String(task_ref.to_string());
        }
        // The canonical model is already published to the gh producer's
        // `Assisted-by:` trailer from the same job result. A flow that
        // authors a commit message needs the same pointer, so the terminal
        // fact carries it rather than making the script re-derive it.
        if let Some(model) = &self.model {
            value["model"] = Value::String(model.clone());
        }
        if let Some(completion) = &self.completion {
            value["completion"] =
                serde_json::to_value(completion).expect("semantic completion always serializes");
        }
        if let Some(excerpt) = &self.stderr_excerpt {
            value["stderr_excerpt"] = Value::String(excerpt.text.clone());
            value["stderr_truncated"] = Value::Bool(excerpt.truncated);
        }
        value
    }
}

const fn terminal_lifecycle_event(verdict: Verdict, has_artifact: bool) -> TallyEvent {
    match (verdict, has_artifact) {
        (Verdict::Preempted, _) => TallyEvent::Preempted,
        (Verdict::Pass | Verdict::Reused | Verdict::Substituted, true) => TallyEvent::Completed,
        (Verdict::Pass | Verdict::Reused | Verdict::Substituted, false) => {
            TallyEvent::WitnessEmitted
        }
        _ => TallyEvent::Failed,
    }
}

fn retained_failure_stderr_excerpt(
    record: &WitnessRecord,
    executor: &Executor,
) -> Option<crate::executor::CaptureExcerpt> {
    if terminal_lifecycle_event(record.verdict, record.artifact_content_hash.is_some())
        != TallyEvent::Failed
    {
        return None;
    }
    let uuid = Uuid::parse_str(record.task_uuid.as_deref()?).ok()?;
    let identity = ExecutionIdentity {
        job_id: uuid,
        task_uuid: Some(uuid),
        task_ref: record
            .orchestration
            .as_ref()
            .and_then(Orchestration::task_ref),
    };
    if let Ok(Some(excerpt)) =
        executor.persist_failure_stderr(&identity, record.attempt, record.lease_epoch)
    {
        return Some(excerpt);
    }
    let paths = executor
        .retained_capture_paths(&identity, record.attempt, record.lease_epoch)
        .ok()??;
    let failure = paths.failure_stderr.as_ref().unwrap_or(&paths.stderr);
    crate::executor::read_capture_excerpt(failure).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobState {
    Paused,
    Queued,
    Running,
    Completed,
}

#[derive(Debug, Clone)]
struct Job {
    job_id: Uuid,
    task_uuid: Option<Uuid>,
    row: RowSeed,
    invocation: AdapterInvocation,
    labor_class: LaborClass,
    state: JobState,
    lease_id: Option<String>,
    adopted: bool,
    adopted_invocation_id: Option<String>,
    /// True when `row.model` came from the adapter's unauthenticated scrape.
    /// It may be projected and used for resume argv, but never copied into the
    /// canonical verdict witness.
    model_is_advisory: bool,
}

impl Job {
    fn stable_key(&self) -> String {
        self.task_uuid.unwrap_or(self.job_id).to_string()
    }

    fn identity(&self) -> ExecutionIdentity {
        ExecutionIdentity {
            job_id: self.job_id,
            task_uuid: self.task_uuid,
            task_ref: self.task_ref(),
        }
    }

    fn task_ref(&self) -> Option<TaskRef> {
        self.row
            .orchestration
            .as_ref()
            .and_then(Orchestration::task_ref)
    }
}

/// Retire a job that has reached its terminal disposition (#395).
///
/// `context.jobs` used to keep every job the daemon had ever admitted, for the
/// daemon's whole lifetime, and a `Job` is a `RowSeed` plus a rendered adapter
/// invocation — far larger than the membership record whose growth it
/// dominated. It is not only resident cost: the compaction live set
/// (`rpc::control`) and the dedup, guardrail, and pool sweeps all walk this map
/// on the admission path, and every one of them already discards `Completed`
/// entries on the way past. Retiring those entries is therefore neutral for all
/// of them by construction — the filter they apply is the removal.
///
/// Nothing is lost. `context.rows` keeps the row seed and `context.query_rows`
/// keeps the query fact the post-ack scrape enriched, both for the daemon's
/// lifetime and both restored across a restart; [`rpc::control::find_job`]
/// answers the two verbs that can still be asked about a terminal job from
/// those. The startup path never installed terminal jobs in the first place, so
/// this makes the live map mean the same thing at minute one as at minute
/// ten thousand.
fn retire_job(context: &mut Context, job_id: Uuid) {
    context.jobs.remove(&job_id);
}

pub struct Context {
    config: Config,
    paths: DaemonPaths,
    host_id: String,
    epoch: u64,
    lease: LocalLease<SystemdUnitLiveness>,
    guardrails: GuardrailState,
    witness: WitnessLedger,
    witness_view: WitnessView,
    derivation_store: Arc<dyn DerivationAvailability>,
    jobs: HashMap<Uuid, Job>,
    aliases: HashMap<String, Uuid>,
    lease_jobs: HashMap<String, Uuid>,
    paused_pools: HashSet<String>,
    unreachable_pools: HashSet<String>,
    unreachable_paused_jobs: HashSet<Uuid>,
    applied_pool_transitions: HashSet<(String, u64)>,
    barriers: BarrierTracker,
    rows: BTreeMap<Uuid, RowSeed>,
    guardrail_depths: BTreeMap<Uuid, u32>,
    query_rows: SnapshotMap<RowFact>,
    query_details: SnapshotMap<RowDetailFact>,
}

type SharedContext = Rc<RwLock<Context>>;

#[derive(Debug)]
struct ExecutionFinished {
    job_id: Uuid,
    attempt: u32,
    lease_epoch: u64,
    elapsed: Duration,
    outcome: Option<Result<ExecutionOutcome, ExecutorError>>,
}

#[derive(Clone)]
struct DaemonHandler {
    context: SharedContext,
    job_tokens: Rc<RefCell<HashMap<String, Uuid>>>,
    settings: DaemonSettings,
    executor: Executor,
    completion: mpsc::UnboundedSender<ExecutionFinished>,
    journal: JournalEmitter,
    history: Rc<RefCell<LifecycleStore>>,
    changes: Rc<RefCell<ChangeStore>>,
    storage: Rc<RefCell<StorageRuntime>>,
    storage_refresh: Rc<Mutex<()>>,
    storage_receipts: Rc<RefCell<HashSet<StorageReceiptKey>>>,
    trace_adapters: Rc<BTreeSet<String>>,
    pages: Rc<RefCell<PageCache>>,
    execution_shutdown: watch::Receiver<bool>,
    execution_cancel: broadcast::Sender<Uuid>,
    fatal: mpsc::UnboundedSender<DaemonError>,
    post_ack_tasks: Rc<RefCell<Vec<JoinHandle<()>>>>,
    pool_transition_tasks: Rc<RefCell<Vec<PoolTransitionTask>>>,
    ingress_sweep: Rc<Mutex<()>>,
    pool_transition_sweep: Rc<Mutex<()>>,
    gh_program: PathBuf,
    tally_socket: String,
    brief_root: PathBuf,
    git_ai: GitAiConfig,
    exec_attestations: bool,
    attestations: Arc<std::sync::Mutex<SharedAttestations>>,
    flow_lineage_cache: Rc<RefCell<Option<CachedFlowLineage>>>,
    flow_membership_cache: Rc<RefCell<Option<CachedFlowMembership>>>,
}

/// The parsed lineage index plus the file stamp it was parsed from.
///
/// Every flow start reads this store, so it is cached rather than re-parsed per
/// run. The stamp is revalidated on each read, so an operator repairing the
/// ledger by hand does not have to restart the daemon.
pub(crate) struct CachedFlowLineage {
    stamp: Option<(u64, Option<std::time::SystemTime>)>,
    lineage: Rc<FlowLineage>,
}

/// The parsed membership index plus the file stamp it was parsed from.
///
/// Every run-scoped query reads this store and every flow admission writes to
/// it, so unlike the lineage cache it is refreshed in place after this daemon's
/// own append rather than dropped: re-parsing the whole ledger once per
/// admission would make admission linear in the ledger. The stamp is still
/// revalidated on every read, so an operator repairing the ledger by hand does
/// not have to restart the daemon.
pub(crate) struct CachedFlowMembership {
    stamp: Option<(u64, Option<std::time::SystemTime>)>,
    membership: Rc<FlowMembership>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StorageReceiptKey {
    producer: String,
    source: String,
    item_id: String,
    warning_sequence: u64,
}

struct StorageRuntime {
    monitor: StorageMonitor,
    snapshot: StorageMetrics,
    poll_interval: Duration,
}

impl StorageRuntime {
    fn new(monitor: StorageMonitor) -> Self {
        Self {
            snapshot: monitor.query_snapshot(),
            poll_interval: monitor.poll_interval(),
            monitor,
        }
    }
}

#[derive(Clone, Copy)]
enum DispatchMethod {
    Enqueue,
    Continue,
    Retry,
    AwaitJob,
    AwaitBarrier,
    Drain,
    Pause,
    Resume,
    Cancel,
    Supersede,
    PoolTransition,
    ProducerRuntimeObserved,
    Acquire,
    Release,
    LeaseStatus,
    Query,
}

const DISPATCHER_METHODS: &[(&str, DispatchMethod)] = &[
    ("queue.enqueue", DispatchMethod::Enqueue),
    ("queue.continue", DispatchMethod::Continue),
    ("queue.retry", DispatchMethod::Retry),
    ("queue.await_job", DispatchMethod::AwaitJob),
    ("queue.await_barrier", DispatchMethod::AwaitBarrier),
    ("queue.drain", DispatchMethod::Drain),
    ("queue.pause", DispatchMethod::Pause),
    ("queue.resume", DispatchMethod::Resume),
    ("queue.cancel", DispatchMethod::Cancel),
    ("flow.supersede", DispatchMethod::Supersede),
    ("__producer.pool-transition", DispatchMethod::PoolTransition),
    (
        "__producer.runtime-observed",
        DispatchMethod::ProducerRuntimeObserved,
    ),
    ("lease.acquire", DispatchMethod::Acquire),
    ("lease.release", DispatchMethod::Release),
    ("lease.status", DispatchMethod::LeaseStatus),
    ("query.jobs", DispatchMethod::Query),
    ("query.job", DispatchMethod::Query),
    ("query.run", DispatchMethod::Query),
    ("query.lineage", DispatchMethod::Query),
    ("query.status", DispatchMethod::Query),
    ("query.storage", DispatchMethod::Query),
    ("query.log", DispatchMethod::Query),
    ("query.proof", DispatchMethod::Query),
    ("query.trace", DispatchMethod::Query),
    ("query.producers", DispatchMethod::Query),
    ("query.watch", DispatchMethod::Query),
    ("query.render", DispatchMethod::Query),
    ("query.standup", DispatchMethod::Query),
    ("query.pools", DispatchMethod::Query),
];

fn dispatch_method(method: &str) -> Option<DispatchMethod> {
    DISPATCHER_METHODS
        .iter()
        .find_map(|(candidate, dispatched)| (*candidate == method).then_some(*dispatched))
}

impl RpcHandler for DaemonHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            let caller = self.resolve_caller(&request)?;
            match dispatch_method(&request.method) {
                Some(DispatchMethod::Enqueue) => self.enqueue(request.params, caller).await,
                Some(DispatchMethod::Continue) => self.continue_job(request.params, caller).await,
                Some(DispatchMethod::Retry) => self.retry_job(request.params).await,
                Some(DispatchMethod::AwaitJob) => self.await_job(request.params).await,
                Some(DispatchMethod::AwaitBarrier) => self.await_barrier(request.params).await,
                Some(DispatchMethod::Drain) => self.drain(request.params).await,
                Some(DispatchMethod::Pause) => self.pause(request.params).await,
                Some(DispatchMethod::Resume) => self.resume(request.params).await,
                Some(DispatchMethod::Cancel) => self.cancel(request.params).await,
                Some(DispatchMethod::Supersede) => self.supersede_flow(request.params).await,
                Some(DispatchMethod::PoolTransition) => self.pool_transition(request.params).await,
                Some(DispatchMethod::ProducerRuntimeObserved) => {
                    self.producer_runtime_observed(request.params).await
                }
                Some(DispatchMethod::Acquire) => self.acquire(request.params).await,
                Some(DispatchMethod::Release) => self.release(request.params).await,
                Some(DispatchMethod::LeaseStatus) => self.lease_status(request.params).await,
                Some(DispatchMethod::Query) => self.query(&request.method, request.params).await,
                None => Err(WireError::new(
                    WireErrorCode::UnknownMethod,
                    format!("unknown RPC method {}", request.method),
                )),
            }
        })
    }
}

/// Server-resolved caller identity for one request.
///
/// A request that presents no `callerJobToken` is Client class. Under the
/// ratified tenancy model — one machine, one trusted Unix user — that class is
/// trusted as an operator, so this type carries no "untrusted" variant. The
/// token exists so that a request which *does* identify as a job cannot pick
/// which job it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallerIdentity {
    Client,
    Job(Uuid),
}

impl CallerIdentity {
    fn job(self) -> Option<Uuid> {
        match self {
            Self::Client => None,
            Self::Job(job_id) => Some(job_id),
        }
    }
}

impl DaemonHandler {
    /// Derive the caller's class from the capability token it presented, and
    /// deny the method classes a job may not reach.
    ///
    /// A job that simply omits its token is demoted to Client class rather than
    /// rejected. That is deliberate and is not a hole: same-UID processes are
    /// trusted per the tenancy model, and containment of hostile same-user code
    /// belongs to the hardening presets, not to this token.
    fn resolve_caller(&self, request: &RequestFrame) -> Result<CallerIdentity, WireError> {
        let Some(token) = presented_job_token(request.params.as_ref())? else {
            return Ok(CallerIdentity::Client);
        };
        let job_id = self
            .job_tokens
            .borrow()
            .get(&hash_job_token(token))
            .copied()
            .ok_or_else(|| {
                WireError::invalid(
                    "callerJobToken is not a live job capability; it was never minted or has been revoked",
                )
            })?;
        match method_class(&request.method) {
            Some(MethodClass::Admin | MethodClass::Producer) => Err(WireError::invalid(format!(
                "method {} is not available to a job capability",
                request.method
            ))),
            _ => Ok(CallerIdentity::Job(job_id)),
        }
    }

    fn fail_stop(&self, error: DaemonError) -> WireError {
        let message = error.to_string();
        let _ = self.fatal.send(error);
        internal_wire(message)
    }

    fn append_change(&self, kind: ChangeKind, payload: Value) -> Result<(), WireError> {
        self.changes
            .borrow_mut()
            .append_now(kind, payload)
            .map(|_| ())
            .map_err(|error| self.fail_stop(error.into()))
    }
}

/// Read `callerJobToken` out of any request's params without consuming them.
///
/// Only the enqueue payload declares the field, so presenting it to another
/// method still fails that method's own `deny_unknown_fields` decode. Reading it
/// here first means the class denial reports the actual reason instead of a
/// deserialization error.
fn presented_job_token(params: Option<&Value>) -> Result<Option<&str>, WireError> {
    match params.and_then(|params| params.get("callerJobToken")) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(token)) => Ok(Some(token)),
        Some(_) => Err(WireError::invalid("callerJobToken must be a string")),
    }
}

fn decode_params<T: for<'de> Deserialize<'de>>(params: Option<Value>) -> Result<T, WireError> {
    serde_json::from_value(params.unwrap_or_else(|| json!({})))
        .map_err(|error| WireError::invalid(error.to_string()))
}

fn internal_wire(error: impl ToString) -> WireError {
    WireError::new(WireErrorCode::Internal, error.to_string())
}

fn storage_intake_wire(snapshot: &StorageMetrics) -> WireError {
    let reason = snapshot.intake.reason.clone().unwrap_or_else(|| {
        "storage budget prevents new intake while already-admitted work continues".to_owned()
    });
    WireError {
        code: if snapshot.monitor_error.is_some() {
            WireErrorCode::StorageMonitorUnavailable
        } else {
            WireErrorCode::StorageBudgetExceeded
        },
        message: reason,
        data: Some(json!({
            "schemaVersion": snapshot.schema_version,
            "activeWarnings": snapshot.active_warnings,
            "storage": snapshot,
        })),
    }
}

fn log_storage_warning(warning: &StorageWarningRecord) {
    // The daemon service owns stdout/stderr, so this is a journal entry even
    // when native structured emission is disabled. The same transition was
    // fsynced to storage-warnings.jsonl before it reaches this advisory sink.
    eprintln!(
        "tally: STORAGE BUDGET transition {}: {}",
        warning.sequence, warning.message
    );
}

impl DaemonHandler {
    fn cached_storage(&self) -> StorageMetrics {
        self.storage.borrow().snapshot.clone()
    }

    fn refresh_storage_for_intake(&self) -> StorageMetrics {
        let (snapshot, warnings, notices, refresh_error) = {
            let mut storage = self.storage.borrow_mut();
            let refresh_error = storage
                .monitor
                .refresh_free_space()
                .err()
                .map(|error| error.to_string());
            let snapshot = storage.monitor.query_snapshot();
            let warnings = storage.monitor.take_warnings();
            let notices = storage.monitor.take_notices();
            storage.snapshot = snapshot.clone();
            (snapshot, warnings, notices, refresh_error)
        };
        if let Some(error) = refresh_error {
            eprintln!(
                "tally: live storage free-space check failed; new intake will be refused: {error}"
            );
        }
        for notice in notices {
            eprintln!("tally: {notice}");
        }
        for warning in &warnings {
            log_storage_warning(warning);
        }
        snapshot
    }

    async fn refresh_storage_now(&self) -> StorageMetrics {
        let _refresh = self.storage_refresh.lock().await;
        self.refresh_storage_locked().await
    }

    async fn refresh_storage_locked(&self) -> StorageMetrics {
        let completion_count = self.context.read().await.witness.head().seq;
        let request = {
            self.storage
                .borrow_mut()
                .monitor
                .sample_request(completion_count)
        };
        let sampled = tokio::task::spawn_blocking(move || request.run()).await;
        let (snapshot, warnings, notices, refresh_error) = match sampled {
            Ok(measurement) => {
                let mut storage = self.storage.borrow_mut();
                let refresh_error = storage
                    .monitor
                    .apply_measurement_result(measurement)
                    .err()
                    .map(|error| error.to_string());
                let snapshot = storage.monitor.query_snapshot();
                let warnings = storage.monitor.take_warnings();
                let notices = storage.monitor.take_notices();
                storage.snapshot = snapshot.clone();
                (snapshot, warnings, notices, refresh_error)
            }
            Err(error) => {
                let message = format!("storage sampler blocking worker failed: {error}");
                let mut storage = self.storage.borrow_mut();
                storage.monitor.record_sample_worker_failure(&message);
                let snapshot = storage.monitor.query_snapshot();
                storage.snapshot = snapshot.clone();
                eprintln!("tally: {message}; new intake will be refused");
                return snapshot;
            }
        };
        if let Some(error) = refresh_error {
            eprintln!("tally: storage monitor refresh failed; new intake will be refused: {error}");
        }
        for notice in notices {
            eprintln!("tally: {notice}");
        }
        for warning in &warnings {
            log_storage_warning(warning);
        }
        snapshot
    }

    async fn compact_lifecycle_if_needed(&self) {
        let retention = self.context.read().await.config.retention.clone();
        if !retention.enable {
            return;
        }
        let keep = match parse_horizon(&retention.lifecycle_horizon) {
            Ok(keep) => keep,
            Err(error) => {
                eprintln!("tally: lifecycle compaction policy is invalid: {error}");
                return;
            }
        };
        match self.history.borrow_mut().compact_if_over_limit(
            keep,
            retention.lifecycle_max_bytes,
            Utc::now(),
        ) {
            Ok(Some(report)) => eprintln!(
                "tally: lifecycle retention compacted {} old records; {} remain (boundary {})",
                report.dropped,
                report.kept,
                report.truncation_boundary.as_deref().unwrap_or("none")
            ),
            Ok(None) => {}
            Err(error) => eprintln!("tally: lifecycle retention compaction failed: {error}"),
        }
    }
}

pub struct Daemon {
    _state_lock: DaemonLockGuard,
    listener: UnixListener,
    handler: DaemonHandler,
    completion_rx: mpsc::UnboundedReceiver<ExecutionFinished>,
    fatal_rx: mpsc::UnboundedReceiver<DaemonError>,
    notifier: SystemdNotifier,
    initial_jobs: Vec<Job>,
    initial_gh_completions: Vec<GhTerminalWork>,
    initial_lost_pools: Vec<String>,
    execution_shutdown: watch::Sender<bool>,
    max_frame_bytes: u64,
    #[cfg(test)]
    lease_tick_hook: Option<LeaseTickHook>,
    #[cfg(test)]
    connection_count_hook: Option<mpsc::UnboundedSender<usize>>,
    #[cfg(test)]
    dispatch_stall_hook: Option<DispatchStallHook>,
}

#[cfg(test)]
include!("tests.rs");
