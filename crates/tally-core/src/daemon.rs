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
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use taskchampion::Status;
use thiserror::Error;
use tokio::net::UnixListener;
use tokio::sync::{broadcast, mpsc, oneshot, watch, Mutex, RwLock};
use tokio::task::{JoinHandle, JoinSet, LocalSet};

use crate::adapters::{
    provisions_gate_manifest, AdapterEngine, AdapterError, AdapterInvocation, ScrapeResult,
};
use crate::brief::{self, PreparedBrief};
use crate::completion::{
    evaluate_completion, ExecutionFact, GateManifestSpec, GateSummaryStatus, SemanticCompletion,
};
use crate::config::{Config, PoolPredicate, Priority};
use crate::evidence::{
    parse_evidence_specs, probe_dedup, probe_full_pass, run_evidence_gate, CheckOutcome,
    DedupMissReason, RetryTrigger, RunOutcome,
};
use crate::executor::{
    ExecutionIdentity, ExecutionOutcome, ExecutionRequest, ExecutionTermination, Executor,
    ExecutorError, UnitLimits, Uuid,
};
use crate::history::{HistoryError, LifecycleStore};
use crate::journal::{EmitEvent, JournalEmitter, JournalEntry, TallyEvent};
use crate::lease::{
    bump_epoch, AdmitOutcome, LeaseBackend, LeaseEngine, LeaseError, LeaseEventLog, LeaseGrant,
    LeaseRequest, LeaseSchedulingGroup, LocalLease, SystemdUnitLiveness,
};
use crate::pagination::{PageCache, PaginationError};
use crate::producer_query::query_producers;
use crate::producers::{
    acknowledged_ingress_ids, archive_ingress_claim, claim_ingress_files, read_ingress_payload,
    GhCliMutationSink, IngressOutcome, ProducerEngine, ReachabilityTransition,
};
use crate::provenance::{Orchestration, DEFAULT_FLOW_MAX_NODES};
use crate::query::{
    query_pools, query_render, query_standup, query_status, JobProjection, PoolHeadroomFact,
    RenderScope, RowFact, RowStatus, StandupOptions, WindowConsumptionFact,
};
use crate::query_v2::{
    query_job as query_job_v2, query_jobs as query_jobs_v2, query_lifecycle_log, query_proof,
    snapshot_metadata, JobsFilter, LifecycleLogFilter, LiveJobFact, ObservabilityError,
    RowDetailFact,
};
use crate::recovery::{
    collect_durable_recovery_facts, collect_local_unit_facts, recover, DurableRecoveryFacts,
    RecoveryAction, RecoveryFacts, RecoveryIdentity, RecoveryPolicy, RecoveryRowState,
    RecoveryTriggers,
};
use crate::taskdb::{
    admits_durable_row, read_acknowledged_events, update_enqueue_event_atomic,
    write_enqueue_event_atomic, AdmissionInput, AdmissionOrigin, DurableEnqueueEvent, DurableRetry,
    EnqueueSource, RowSeed, TaskDb, TaskDbError,
};
use crate::trace::{query_trace, trace_availability, TraceError, TraceLane};
use crate::watch::{ChangeError, ChangeKind, ChangeStore};
use crate::wire::{
    canonical_payload_hash, serve_connection_with_max_frame_bytes, EnqueuePayload, GuardrailConfig,
    GuardrailState, ParentInfo, ProducerDefaults, RequestFrame, RpcHandler, SubmissionMode,
    WireError, WireErrorCode, WireIoError,
};
use crate::witness::{
    append_attestation, read_verified_attestations, read_verified_records, repair_attestation_tail,
    verify_attestations, AttestationRecord, LaborClass, Verdict, WitnessBody, WitnessError,
    WitnessLedger, WitnessRecord,
};

const LEASE_TICK: Duration = Duration::from_millis(100);
const MAX_METER_EVENT_BYTES: u64 = 64 * 1024;
const UNCLAIMED_DRAIN_BARRIER_LIMIT: usize = 64;

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
        Ok(self)
    }
}

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("invalid daemon configuration: {0}")]
    Invalid(String),
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
    #[error("lifecycle history error: {0}")]
    History(#[from] HistoryError),
    #[error("change log error: {0}")]
    Change(#[from] ChangeError),
    #[error("recovery error: {0}")]
    Recovery(#[from] crate::recovery::RecoveryError),
    #[error("executor error: {0}")]
    Executor(#[from] ExecutorError),
    #[error("adapter error: {0}")]
    Adapter(#[from] AdapterError),
    #[error("post-ack replica worker stopped")]
    CommitWorkerStopped,
    #[error("sd_notify failed: {0}")]
    Notify(String),
}

fn io_error(path: &Path, source: io::Error) -> DaemonError {
    DaemonError::Io {
        path: path.to_owned(),
        source,
    }
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
    pub job_id: String,
    pub verdict: Verdict,
    pub exit_code: i32,
    pub artifact_content_hash: Option<String>,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub witness_seq: u64,
    pub model: Option<String>,
    pub completion: Option<SemanticCompletion>,
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
        if let Some(completion) = &self.completion {
            value["completion"] =
                serde_json::to_value(completion).expect("semantic completion always serializes");
        }
        value
    }
}

enum WaitRegistration {
    Ready(Value),
    Pending(oneshot::Receiver<Value>),
}

#[derive(Debug, Default)]
struct BarrierEntry {
    pending: BTreeSet<String>,
    results: BTreeMap<String, Value>,
    waiters: Vec<oneshot::Sender<Value>>,
}

#[derive(Debug, Default)]
pub struct BarrierTracker {
    namespace: u64,
    next: u64,
    barriers: HashMap<String, BarrierEntry>,
    job_waiters: HashMap<String, Vec<oneshot::Sender<Value>>>,
}

impl BarrierTracker {
    pub fn with_namespace(namespace: u64) -> Self {
        Self {
            namespace,
            ..Self::default()
        }
    }

    pub fn register_job(&mut self, stable_job_key: &str, attempt: u32) -> String {
        self.prune_closed_waiters();
        format!("barrier:{stable_job_key}:{attempt}")
    }

    pub fn snapshot(&mut self, jobs: impl IntoIterator<Item = String>) -> String {
        self.prune_closed_waiters();
        self.next = self.next.saturating_add(1);
        let barrier = format!("barrier:drain:{}:{}", self.namespace, self.next);
        let mut entry = BarrierEntry::default();
        for job in jobs {
            entry.pending.insert(job);
        }
        self.barriers.insert(barrier.clone(), entry);
        self.prune_unclaimed_barriers();
        barrier
    }

    fn complete_job(&mut self, stable_job_key: &str, value: Value) {
        self.prune_closed_waiters();
        if let Some(waiters) = self.job_waiters.remove(stable_job_key) {
            for waiter in waiters {
                let _ = waiter.send(value.clone());
            }
        }

        let mut completed = Vec::new();
        for (barrier, entry) in &mut self.barriers {
            if entry.pending.remove(stable_job_key) {
                entry
                    .results
                    .insert(stable_job_key.to_owned(), value.clone());
            }
            if entry.pending.is_empty() && !entry.waiters.is_empty() {
                completed.push(barrier.clone());
            }
        }
        for barrier in completed {
            if let Some(mut entry) = self.barriers.remove(&barrier) {
                let result = barrier_value(&barrier, &entry.results);
                if entry.waiters.is_empty() {
                    self.barriers.insert(barrier, entry);
                } else {
                    for waiter in std::mem::take(&mut entry.waiters) {
                        let _ = waiter.send(result.clone());
                    }
                }
            }
        }
        self.prune_unclaimed_barriers();
    }

    fn wait_job(&mut self, stable_job_key: &str) -> WaitRegistration {
        self.prune_closed_waiters();
        let (sender, receiver) = oneshot::channel();
        self.job_waiters
            .entry(stable_job_key.to_owned())
            .or_default()
            .push(sender);
        WaitRegistration::Pending(receiver)
    }

    fn prune_closed_waiters(&mut self) {
        self.job_waiters.retain(|_, waiters| {
            waiters.retain(|waiter| !waiter.is_closed());
            !waiters.is_empty()
        });
        for entry in self.barriers.values_mut() {
            entry.waiters.retain(|waiter| !waiter.is_closed());
        }
    }

    fn prune_unclaimed_barriers(&mut self) {
        let mut unclaimed = self
            .barriers
            .iter()
            .filter(|(_, entry)| entry.waiters.is_empty())
            .map(|(barrier, _)| {
                let sequence = barrier
                    .rsplit(':')
                    .next()
                    .and_then(|sequence| sequence.parse::<u64>().ok())
                    .unwrap_or(0);
                (sequence, barrier.clone())
            })
            .collect::<Vec<_>>();
        unclaimed.sort_by_key(|(sequence, _)| *sequence);
        let remove_count = unclaimed
            .len()
            .saturating_sub(UNCLAIMED_DRAIN_BARRIER_LIMIT);
        for (_, barrier) in unclaimed.into_iter().take(remove_count) {
            self.barriers.remove(&barrier);
        }
    }

    fn wait_barrier(&mut self, barrier: &str) -> Result<WaitRegistration, WireError> {
        self.prune_closed_waiters();
        if self
            .barriers
            .get(barrier)
            .is_some_and(|entry| entry.pending.is_empty())
        {
            let entry = self
                .barriers
                .remove(barrier)
                .expect("the completed barrier was just observed");
            return Ok(WaitRegistration::Ready(barrier_value(
                barrier,
                &entry.results,
            )));
        }
        let entry = self
            .barriers
            .get_mut(barrier)
            .ok_or_else(|| WireError::not_found(format!("unknown barrier {barrier}")))?;
        let (sender, receiver) = oneshot::channel();
        entry.waiters.push(sender);
        Ok(WaitRegistration::Pending(receiver))
    }

    #[cfg(test)]
    fn retained_entry_count(&self) -> usize {
        self.barriers.len() + self.job_waiters.values().map(Vec::len).sum::<usize>()
    }
}

fn barrier_value(barrier: &str, results: &BTreeMap<String, Value>) -> Value {
    json!({
        "barrier": barrier,
        "complete": true,
        "results": results.values().cloned().collect::<Vec<_>>(),
    })
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
        }
    }
}

pub struct Context {
    config: Config,
    paths: DaemonPaths,
    epoch: u64,
    lease: LocalLease<SystemdUnitLiveness>,
    guardrails: GuardrailState,
    witness: WitnessLedger,
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
    query_rows: BTreeMap<Uuid, RowFact>,
    query_details: BTreeMap<Uuid, RowDetailFact>,
}

type SharedContext = Rc<RwLock<Context>>;

#[derive(Debug, Clone)]
enum CommitCommand {
    Upsert {
        row: Box<RowSeed>,
        status: Status,
        labor_class: LaborClass,
    },
    Rebuild,
    Shutdown,
}

trait ReplicaCommitter: Send {
    fn commit<'a>(
        &'a mut self,
        command: CommitCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>>;
}

struct TaskDbCommitter {
    db: TaskDb,
    events_dir: PathBuf,
    witness_path: PathBuf,
    adapter_metadata: BTreeMap<Uuid, (RowSeed, Status, LaborClass)>,
}

impl ReplicaCommitter for TaskDbCommitter {
    fn commit<'a>(
        &'a mut self,
        command: CommitCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>> {
        Box::pin(async move {
            match command {
                CommitCommand::Upsert {
                    row,
                    status,
                    labor_class,
                } => {
                    let row = *row;
                    if row.session_ref.is_some()
                        || row.model.is_some()
                        || row.final_message.is_some()
                    {
                        self.adapter_metadata
                            .insert(row.uuid, (row.clone(), status.clone(), labor_class));
                    } else {
                        self.adapter_metadata.remove(&row.uuid);
                    }
                    let prepared = self
                        .db
                        .prepare_row(row, status, labor_class)
                        .await
                        .map_err(|error| error.to_string())?;
                    self.db
                        .commit_prepared([prepared])
                        .await
                        .map_err(|error| error.to_string())?;
                }
                CommitCommand::Rebuild => {
                    self.db
                        .rebuild_from_sources(&self.events_dir, &self.witness_path)
                        .await
                        .map_err(|error| error.to_string())?;
                    let metadata = self.adapter_metadata.values().cloned().collect::<Vec<_>>();
                    let mut prepared = Vec::with_capacity(metadata.len());
                    for (row, status, labor_class) in metadata {
                        prepared.push(
                            self.db
                                .prepare_row(row, status, labor_class)
                                .await
                                .map_err(|error| error.to_string())?,
                        );
                    }
                    self.db
                        .commit_prepared(prepared)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                CommitCommand::Shutdown => {}
            }
            Ok(())
        })
    }
}

struct CommitWorker {
    thread: std::thread::JoinHandle<()>,
    stopping: Arc<AtomicBool>,
}

fn spawn_commit_worker(
    mut committer: Box<dyn ReplicaCommitter>,
    mut receiver: mpsc::UnboundedReceiver<CommitCommand>,
    state_lock: File,
) -> Result<CommitWorker, DaemonError> {
    let stopping = Arc::new(AtomicBool::new(false));
    let worker_stopping = stopping.clone();
    let thread = std::thread::Builder::new()
        .name("tally-replica-commit".to_owned())
        .spawn(move || {
            // A wedged post-ack writer may outlive the bounded daemon shutdown.
            // Retain the daemon lock in that thread so no replacement writer can
            // open the same state until the old worker has actually stopped.
            let _state_lock = state_lock;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the replica worker runtime must initialize");
            while let Some(command) = receiver.blocking_recv() {
                if worker_stopping.load(Ordering::Acquire)
                    || matches!(command, CommitCommand::Shutdown)
                {
                    break;
                }
                if let Err(error) = runtime.block_on(committer.commit(command)) {
                    eprintln!("tally: post-ack replica commit failed: {error}");
                }
                if worker_stopping.load(Ordering::Acquire) {
                    break;
                }
            }
        })
        .map_err(|error| DaemonError::Invalid(format!("cannot start replica worker: {error}")))?;
    Ok(CommitWorker { thread, stopping })
}

#[derive(Debug)]
struct ExecutionFinished {
    job_id: Uuid,
    attempt: u32,
    lease_epoch: u64,
    elapsed: Duration,
    outcome: Option<Result<ExecutionOutcome, ExecutorError>>,
}

struct TerminalWork {
    job: Job,
    result: JobResult,
    evidence: String,
    evidence_checks: Vec<CheckOutcome>,
    launches: Vec<Job>,
    scrape_capture: bool,
}

#[derive(Debug, Clone)]
struct GhTerminalWork {
    row: RowSeed,
    result: JobResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PoolLossIntent {
    schema_version: u32,
    row: RowSeed,
    labor_class: LaborClass,
    adopted_invocation_id: Option<String>,
    model_is_advisory: bool,
}

type PoolTransitionTask = JoinHandle<Result<(), WireError>>;

#[derive(Clone)]
struct DaemonHandler {
    context: SharedContext,
    settings: DaemonSettings,
    executor: Executor,
    completion: mpsc::UnboundedSender<ExecutionFinished>,
    commits: mpsc::UnboundedSender<CommitCommand>,
    journal: JournalEmitter,
    history: Rc<RefCell<LifecycleStore>>,
    changes: Rc<RefCell<ChangeStore>>,
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
}

impl RpcHandler for DaemonHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            match request.method.as_str() {
                "queue.enqueue" => self.enqueue(request.params).await,
                "queue.continue" => self.continue_job(request.params).await,
                "queue.retry" => self.retry_job(request.params).await,
                "queue.await_job" => self.await_job(request.params).await,
                "queue.await_barrier" => self.await_barrier(request.params).await,
                "queue.drain" => self.drain(request.params).await,
                "queue.pause" => self.pause(request.params).await,
                "queue.resume" => self.resume(request.params).await,
                "queue.cancel" => self.cancel(request.params).await,
                "__producer.pool-transition" => self.pool_transition(request.params).await,
                "__producer.runtime-observed" => {
                    self.producer_runtime_observed(request.params).await
                }
                "lease.acquire" => self.acquire(request.params).await,
                "lease.release" => self.release(request.params).await,
                "lease.status" => self.lease_status(request.params).await,
                "query.jobs" | "query.job" | "query.status" | "query.log" | "query.proof"
                | "query.trace" | "query.producers" | "query.watch" | "query.render"
                | "query.standup" | "query.pools" => {
                    self.query(&request.method, request.params).await
                }
                other => Err(WireError::new(
                    WireErrorCode::UnknownMethod,
                    format!("unknown RPC method {other}"),
                )),
            }
        })
    }
}

impl DaemonHandler {
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

    async fn enqueue(&self, params: Option<Value>) -> Result<Value, WireError> {
        let payload: EnqueuePayload = decode_params(params)?;
        self.enqueue_payload(payload, None).await
    }

    async fn continue_job(&self, params: Option<Value>) -> Result<Value, WireError> {
        let payload: EnqueuePayload = decode_params(params)?;
        if payload.resume_from.is_none() {
            return Err(WireError::invalid(
                "queue.continue requires a resumeFrom task UUID",
            ));
        }
        self.enqueue_payload(payload, None).await
    }

    async fn retry_job(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            #[serde(alias = "taskUuid")]
            task_uuid: String,
        }

        let params: Params = decode_params(params)?;
        let task_uuid = Uuid::parse_str(&params.task_uuid)
            .map_err(|_| WireError::invalid("task_uuid must be a UUID"))?;
        let mut context = self.context.write().await;
        if context.jobs.get(&task_uuid).is_some_and(|job| {
            matches!(
                job.state,
                JobState::Paused | JobState::Queued | JobState::Running
            )
        }) {
            return Err(WireError::invalid(format!(
                "job {task_uuid} is not terminal and cannot be retried"
            )));
        }
        let mut row = context
            .rows
            .get(&task_uuid)
            .cloned()
            .ok_or_else(|| WireError::invalid(format!("job {task_uuid} was not found")))?;
        let (report, records) =
            read_verified_records(&context.paths.witness_path()).map_err(internal_wire)?;
        if !report.ok {
            return Err(internal_wire(
                "witness verification failed while admitting retry",
            ));
        }
        let canonical_task_uuid = task_uuid.to_string();
        let terminal = records
            .iter()
            .filter(|record| record.task_uuid.as_deref() == Some(canonical_task_uuid.as_str()))
            .max_by_key(|record| record.seq)
            .cloned()
            .ok_or_else(|| {
                WireError::invalid(format!(
                    "job {task_uuid} has no terminal witness and cannot be retried"
                ))
            })?;
        if terminal.attempt != row.attempt {
            return Err(internal_wire(format!(
                "job {task_uuid} row attempt {} disagrees with terminal witness attempt {}",
                row.attempt, terminal.attempt
            )));
        }
        if terminal.payload_hash != row.payload_hash {
            return Err(internal_wire(format!(
                "job {task_uuid} durable row and terminal witness payload hashes disagree"
            )));
        }
        if terminal.verdict == Verdict::Pass {
            return Err(WireError::invalid(format!(
                "job {task_uuid} passed and cannot be retried"
            )));
        }
        let next_attempt = row
            .attempt
            .checked_add(1)
            .ok_or_else(|| WireError::invalid(format!("job {task_uuid} attempt overflow")))?;
        row.attempt = next_attempt;
        row.lease_epoch = context.epoch;

        let engine = AdapterEngine::new(&context.config.adapters);
        let invocation = if row.resumed_from.is_some() {
            let session_ref = row.session_ref.clone().ok_or_else(|| {
                WireError::invalid(format!(
                    "continued job {task_uuid} has no durable session reference"
                ))
            })?;
            let mut captures =
                BTreeMap::from([("sessionRef".to_owned(), Value::String(session_ref))]);
            if let Some(model) = &row.model {
                captures.insert("model".to_owned(), Value::String(model.clone()));
            }
            engine.resume_with_options(
                &row.adapter,
                &row.argv,
                &ScrapeResult { captures },
                &row.adapter_options,
                row.cwd.as_deref(),
            )
        } else {
            engine.launch_with_options(
                &row.adapter,
                &row.argv,
                &row.adapter_options,
                row.cwd.as_deref(),
            )
        }
        .map_err(|error| WireError::invalid(error.to_string()))?;
        row.validate()
            .map_err(|error| WireError::invalid(error.to_string()))?;

        let mut job = Job {
            job_id: task_uuid,
            task_uuid: Some(task_uuid),
            row: row.clone(),
            invocation,
            labor_class: LaborClass::Recovered,
            state: JobState::Queued,
            lease_id: None,
            adopted: false,
            adopted_invocation_id: None,
            model_is_advisory: false,
        };
        let unit = self.executor.unit_name(&job.identity());
        let request = lease_request(&job, unit);
        context
            .lease
            .engine()
            .validate_admission(&request)
            .map_err(lease_wire)?;

        let parent_charge = row.parent_uuid.is_some()
            && context
                .guardrail_depths
                .get(&task_uuid)
                .is_some_and(|depth| *depth > 0);
        if let Some(parent_uuid) = row.parent_uuid.filter(|_| parent_charge) {
            ensure_guardrail_parent(&mut context, &parent_uuid.to_string(), true)?;
            context.guardrails.charge_child(&parent_uuid.to_string())?;
        }

        let events_dir = context.paths.events_dir();
        let mut matching_events = read_acknowledged_events(&events_dir)
            .map_err(internal_wire)?
            .into_iter()
            .filter(|event| event.row.uuid == task_uuid)
            .collect::<Vec<_>>();
        if matching_events.len() != 1 {
            if let Some(parent_uuid) = row.parent_uuid.filter(|_| parent_charge) {
                context
                    .guardrails
                    .rollback_child_charge(&parent_uuid.to_string())?;
            }
            return Err(internal_wire(format!(
                "job {task_uuid} has {} acknowledged enqueue events",
                matching_events.len()
            )));
        }
        let mut event = matching_events
            .pop()
            .expect("exactly one matching event was checked");
        event.retries.push(DurableRetry {
            attempt: next_attempt,
            previous_witness_seq: terminal.seq,
        });
        if let Err(error) = update_enqueue_event_atomic(&events_dir, &event) {
            // Atomic replacement can fail after the rename made the retry
            // durable. Stop serving so recovery, rather than this generation,
            // decides whether the new attempt is pending.
            return Err(self.fail_stop(error.into()));
        }

        let stable_key = task_uuid.to_string();
        let barrier = context.barriers.register_job(&stable_key, next_attempt);
        let mut launch = None;
        if row.pools.iter().any(|pool| {
            context.paused_pools.contains(pool) || context.unreachable_pools.contains(pool)
        }) {
            job.state = JobState::Paused;
            if row
                .pools
                .iter()
                .any(|pool| context.unreachable_pools.contains(pool))
            {
                context.unreachable_paused_jobs.insert(task_uuid);
            }
        } else {
            match context.lease.admit(request, Utc::now()) {
                Ok(AdmitOutcome::Granted(grant)) => {
                    job.lease_id = Some(grant.lease_id.clone());
                    job.state = JobState::Running;
                    context.lease_jobs.insert(grant.lease_id, task_uuid);
                    launch = Some(job.clone());
                }
                Ok(AdmitOutcome::Queued { ticket_id, .. }) => {
                    job.lease_id = Some(ticket_id.clone());
                    context.lease_jobs.insert(ticket_id, task_uuid);
                }
                Err(error) => {
                    eprintln!(
                        "tally: retried job {} is waiting for lease retry: {error}",
                        job.stable_key()
                    );
                }
            }
        }
        context.aliases.insert(stable_key.clone(), task_uuid);
        let guardrail_depth = context
            .guardrail_depths
            .get(&task_uuid)
            .copied()
            .unwrap_or(0);
        if context.guardrails.parent(&stable_key).is_none() {
            let child_count = context
                .rows
                .values()
                .filter(|child| child.parent_uuid == Some(task_uuid))
                .filter(|child| {
                    context
                        .jobs
                        .get(&child.uuid)
                        .is_some_and(|job| job.state != JobState::Completed)
                })
                .count();
            let outstanding = u32::try_from(child_count)
                .map_err(|_| internal_wire("retry child guardrail count overflow"))?;
            context.guardrails.register_parent(
                stable_key.clone(),
                ParentInfo {
                    parent_uuid: stable_key.clone(),
                    depth: guardrail_depth,
                    outstanding,
                    no_enqueue: row.no_enqueue,
                    terminal: false,
                },
            );
        }
        context.rows.insert(task_uuid, row.clone());
        context
            .query_rows
            .insert(task_uuid, query_row(&row, RowStatus::Pending));
        context.query_details.insert(
            task_uuid,
            RowDetailFact::from_seed(&row, RowStatus::Pending, LaborClass::Recovered),
        );
        if let Some(parent) = context.guardrails.parent(&stable_key).cloned() {
            context.guardrails.register_parent(
                stable_key.clone(),
                ParentInfo {
                    depth: guardrail_depth,
                    terminal: false,
                    ..parent
                },
            );
        }
        context.jobs.insert(task_uuid, job.clone());
        drop(context);

        if self
            .commits
            .send(CommitCommand::Upsert {
                row: Box::new(row.clone()),
                status: Status::Pending,
                labor_class: LaborClass::Recovered,
            })
            .is_err()
        {
            eprintln!("tally: post-ack replica worker stopped before retry projection");
        }
        self.emit_post_ack(enqueued_event(&job));
        if let Some(job) = launch {
            self.spawn_execution(job);
        }
        let mut response = json!({
            "schemaVersion": 1,
            "retried": true,
            "task_uuid": stable_key,
            "taskUuid": stable_key,
            "job_id": stable_key,
            "barrier": barrier,
            "state": state_name(job.state),
            "status": state_name(job.state),
            "attempt": next_attempt,
        });
        if let Some(payload_hash) = row.payload_hash {
            response["payloadHash"] = Value::String(payload_hash);
        }
        Ok(response)
    }

    async fn enqueue_payload(
        &self,
        mut payload: EnqueuePayload,
        ingress_id: Option<String>,
    ) -> Result<Value, WireError> {
        let inline_brief = payload.brief.take();
        let brief_source_path = payload.brief_path.take();
        let prepared_brief =
            tokio::task::spawn_blocking(move || brief::prepare(inline_brief, brief_source_path))
                .await
                .map_err(|error| internal_wire(format!("brief worker failed: {error}")))?
                .map_err(|error| WireError::invalid(error.to_string()))?;
        let full_mode = payload
            .submission
            .as_ref()
            .is_some_and(|submission| submission.mode == SubmissionMode::Full);
        let caller_job_id = payload.caller_job_id.clone();
        let mut context = self.context.write().await;
        if let Some(caller_job_id) = caller_job_id.as_deref() {
            ensure_guardrail_parent(&mut context, caller_job_id, false)?;
        }
        let resumed_job = if let Some(resume_from) = payload.resume_from.as_deref() {
            let previous = find_job(&context, resume_from)?.clone();
            if previous.state != JobState::Completed {
                return Err(WireError::invalid(format!(
                    "job {resume_from} is not terminal and cannot be continued"
                )));
            }
            if previous.row.session_ref.is_none() {
                return Err(WireError::invalid(format!(
                    "job {resume_from} has no scraped session reference"
                )));
            }
            payload
                .pools
                .get_or_insert_with(|| previous.row.pools.clone());
            if payload.executor.is_none() {
                payload.executor.clone_from(&previous.row.executor);
            }
            payload.priority.get_or_insert(previous.row.priority);
            payload
                .adapter
                .get_or_insert_with(|| previous.row.adapter.clone());
            payload.source.get_or_insert(previous.row.source);
            if payload.origin.is_none() {
                payload.origin.clone_from(&previous.row.origin);
            }
            if payload.cwd.is_none() {
                payload.cwd.clone_from(&previous.row.cwd);
            }
            if payload.workspace.is_none() {
                payload.workspace.clone_from(&previous.row.workspace);
            }
            if payload.adapter_options.is_none() {
                payload.adapter_options = Some(previous.row.adapter_options.clone());
            }
            if previous.row.source == EnqueueSource::Gh {
                payload.gh_origin.clone_from(&previous.row.gh_origin);
                if let Some(origin) = &previous.row.gh_origin {
                    payload.gh_trigger_actor = Some(origin.trigger_actor.clone());
                    payload.gh_self_actor = Some(origin.self_actor.clone());
                }
            }
            Some(previous)
        } else {
            None
        };
        let requested_adapter = payload
            .adapter
            .clone()
            .unwrap_or_else(|| "shell".to_owned());
        if let Some(origin) = &payload.gh_origin {
            ProducerEngine::new(
                &context.config.producers,
                context.paths.events_dir(),
                &context.paths.state_dir,
            )
            .validate_gh_origin(origin)
            .map_err(|error| WireError::invalid(error.to_string()))?;
        }
        let mut requested_pools = payload
            .pools
            .clone()
            .ok_or_else(|| WireError::invalid("pool set is required"))?;
        crate::poolset::canonicalize(&mut requested_pools)
            .map_err(|error| WireError::invalid(error.to_string()))?;
        for requested_pool in &requested_pools {
            if !context.config.pools.contains_key(requested_pool) {
                return Err(WireError::invalid(format!(
                    "unknown pool {requested_pool:?}"
                )));
            }
        }
        if !context.config.adapters.contains_key(&requested_adapter) {
            return Err(WireError::invalid(format!(
                "unknown adapter {requested_adapter:?}"
            )));
        }
        if let Some(executor) = &payload.executor {
            if !context.config.executors.contains_key(executor) {
                return Err(WireError::invalid(format!("unknown executor {executor:?}")));
            }
        }
        let defaults = ProducerDefaults {
            pools: requested_pools,
            executor: payload.executor.clone(),
            priority: payload.priority.unwrap_or(Priority::Medium),
            adapter: requested_adapter,
            source: payload.source.unwrap_or(EnqueueSource::Manual),
            cwd: None,
            workspace: None,
            adapter_options: Default::default(),
        };
        let mut resolved = context.guardrails.validate_enqueue(payload, &defaults)?;
        resolved.brief_hash = prepared_brief.as_ref().map(|brief| brief.hash().to_owned());
        let mut child_charged = caller_job_id.is_some() && !full_mode;
        for pool in &resolved.pools {
            let pool_credentials = context
                .config
                .pools
                .get(pool)
                .expect("the requested pools were validated above")
                .credentials
                .clone();
            for (name, source) in pool_credentials {
                if resolved
                    .credentials
                    .get(&name)
                    .is_some_and(|existing| existing != &source)
                {
                    rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                    return Err(WireError::invalid(format!(
                        "credential {name:?} has conflicting pool and enqueue sources"
                    )));
                }
                resolved.credentials.entry(name).or_insert(source);
            }
        }
        let engine = AdapterEngine::new(&context.config.adapters);
        let rendered = if let Some(previous) = &resumed_job {
            if resolved.adapter != previous.row.adapter {
                Err(AdapterError::InvalidConfig {
                    adapter: resolved.adapter.clone(),
                    detail: "a continuation must use the original adapter".to_owned(),
                })
            } else {
                let mut captures = BTreeMap::from([(
                    "sessionRef".to_owned(),
                    Value::String(
                        previous
                            .row
                            .session_ref
                            .clone()
                            .expect("continued jobs were checked for a session reference"),
                    ),
                )]);
                if let Some(model) = &previous.row.model {
                    captures.insert("model".to_owned(), Value::String(model.clone()));
                }
                engine.resume_with_options(
                    &resolved.adapter,
                    &resolved.argv,
                    &ScrapeResult { captures },
                    &resolved.adapter_options,
                    resolved.cwd.as_deref(),
                )
            }
        } else {
            engine.launch_with_options(
                &resolved.adapter,
                &resolved.argv,
                &resolved.adapter_options,
                resolved.cwd.as_deref(),
            )
        };
        let invocation = match rendered {
            Ok(invocation) => invocation,
            Err(error) => {
                rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                return Err(WireError::invalid(error.to_string()));
            }
        };

        let epoch = context.epoch;
        let durable = admits_durable_row(&AdmissionInput {
            source: resolved.source,
            // An RPC enqueue is an acknowledged, crash-survivable admission.
            // Merely tagging its producer as "orchestrator" does not make it
            // an already-running, live-only orchestrator child.
            live_orchestrator_spawned: false,
            autonomous: resolved.source != EnqueueSource::Orchestrator,
            crash_survivable: true,
            needs_cross_source_urgency: resolved.priority.rank() >= Priority::High.rank(),
        });
        if !durable {
            rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
            return Err(internal_wire(
                "RPC admissions must always have a durable recovery row",
            ));
        }
        let payload_hash = if full_mode {
            match canonical_payload_hash(&resolved) {
                Ok(payload_hash) => Some(payload_hash),
                Err(error) => {
                    rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                    return Err(internal_wire(format!(
                        "cannot serialize canonical enqueue payload: {error}"
                    )));
                }
            }
        } else {
            None
        };
        let job_id = resolved
            .task_uuid
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()
            .map_err(|_| WireError::invalid("taskUuid must be a UUID"))?
            .unwrap_or_else(Uuid::now_v7);
        if !full_mode
            && (context.jobs.contains_key(&job_id) || context.query_rows.contains_key(&job_id))
        {
            rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
            return Err(WireError::invalid(format!(
                "task UUID {job_id} is already admitted"
            )));
        }
        let task_uuid = Some(job_id);
        let row_uuid = job_id;
        let parent_uuid = resolved
            .parent
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()
            .map_err(|_| WireError::invalid("parent must be a UUID"))?;
        let row = RowSeed {
            uuid: row_uuid,
            description: resolved.argv.join(" "),
            priority: resolved.priority,
            source: resolved.source,
            adapter: resolved.adapter.clone(),
            pools: resolved.pools.clone(),
            executor: resolved.executor.clone(),
            model: resumed_job.as_ref().and_then(|job| job.row.model.clone()),
            cwd: resolved.cwd,
            workspace: resolved.workspace,
            adapter_options: resolved.adapter_options,
            gate_manifest: resolved.gate_manifest,
            resumed_from: resolved.resume_from,
            dedup_key: resolved.dedup_key.clone(),
            payload_hash,
            brief_hash: resolved.brief_hash.clone(),
            orchestration: resolved.orchestration.clone(),
            session_ref: resumed_job
                .as_ref()
                .and_then(|job| job.row.session_ref.clone()),
            final_message: None,
            lease_epoch: epoch,
            attempt: 1,
            argv: resolved.argv,
            evidence: resolved.evidence,
            parent_uuid,
            consumption_estimate: resolved.consumption_estimate,
            runtime_max_sec: resolved.runtime_max_sec,
            no_enqueue: resolved.no_enqueue,
            credentials: resolved.credentials,
            origin: Some(resolved.origin),
            gh_origin: resolved.gh_origin,
            related_trigger: resolved.related_trigger,
            evidence_class: resolved.evidence_class,
            manifest_hash: resolved.manifest_hash.map(Value::String),
        };
        if let Err(error) = row.validate() {
            rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
            return Err(WireError::invalid(error.to_string()));
        }

        let mut reused_rejected = None;
        let mut reuse_error_detail = None;
        if full_mode {
            if let (Some(dedup_key), Some(payload_hash)) = (
                row.dedup_key
                    .as_deref()
                    .filter(|key| !key.trim().is_empty()),
                row.payload_hash.as_deref(),
            ) {
                loop {
                    if let Some(response) =
                        full_live_disposition(&context, dedup_key, payload_hash)?
                    {
                        return Ok(response);
                    }
                    let witness_path = context.paths.witness_path();
                    let probe_dedup_key = dedup_key.to_owned();
                    let probe_payload_hash = payload_hash.to_owned();
                    let evidence_specs = row.evidence.clone();
                    drop(context);
                    let probe = tokio::task::spawn_blocking(move || {
                        let (report, witness) = read_verified_records(&witness_path)?;
                        if !report.ok {
                            return Err(WitnessError::Corrupt(
                                "witness verification failed during full dedup probe".to_owned(),
                            ));
                        }
                        let governing = witness
                            .iter()
                            .filter(|record| {
                                record.dedup_key.as_deref() == Some(probe_dedup_key.as_str())
                            })
                            .max_by_key(|record| record.seq)
                            .cloned();
                        let pass_probe = governing.as_ref().and_then(|record| {
                            (record.payload_hash.as_deref() == Some(probe_payload_hash.as_str())
                                && record.verdict == Verdict::Pass)
                                .then(|| {
                                    let evidence = parse_evidence_specs(&evidence_specs)
                                        .expect("validated row evidence remains canonical");
                                    probe_full_pass(&evidence, record)
                                })
                        });
                        Ok((report.last_seq.unwrap_or(0), governing, pass_probe))
                    })
                    .await;
                    context = self.context.write().await;
                    let (loaded_head, governing, pass_probe) = match probe {
                        Ok(Ok(probe)) => probe,
                        Ok(Err(error)) => return Err(internal_wire(error)),
                        Err(error) => {
                            return Err(internal_wire(format!(
                                "full dedup worker failed: {error}"
                            )));
                        }
                    };
                    if let Some(response) =
                        full_live_disposition(&context, dedup_key, payload_hash)?
                    {
                        return Ok(response);
                    }
                    if context.witness.head().seq != loaded_head {
                        continue;
                    }
                    let Some(governing) = governing else {
                        break;
                    };
                    let Some(existing_payload_hash) = governing.payload_hash.as_deref() else {
                        reused_rejected = Some("payload-hash-unrecorded");
                        break;
                    };
                    if existing_payload_hash != payload_hash {
                        let task_uuid = governing
                            .task_uuid
                            .clone()
                            .unwrap_or_else(|| format!("witness:{}", governing.seq));
                        return Err(dedup_conflict(
                            dedup_key,
                            payload_hash,
                            vec![DedupConflictCandidate {
                                task_uuid,
                                payload_hash: Some(existing_payload_hash.to_owned()),
                                orchestration: governing.orchestration.clone(),
                            }],
                        ));
                    }
                    if governing.verdict != Verdict::Pass {
                        return full_terminal_response(&governing, payload_hash, "terminal");
                    }
                    let pass_probe = pass_probe.expect(
                        "matching pass governing records are artifact-probed in the worker",
                    );
                    if pass_probe.hit {
                        return full_terminal_response(&governing, payload_hash, "reused");
                    }
                    match pass_probe.miss_reason {
                        Some(DedupMissReason::WitnessHashMismatch) => {
                            reused_rejected = Some("artifact-drift");
                        }
                        Some(DedupMissReason::DeclaredHashMismatch) => {
                            reused_rejected = Some("declared-hash-mismatch");
                        }
                        Some(DedupMissReason::ArtifactUnavailable(path)) => {
                            reused_rejected = Some("artifact-unavailable");
                            reuse_error_detail = Some(path.to_string_lossy().into_owned());
                        }
                        Some(reason) => {
                            return Err(internal_wire(format!(
                                "unexpected full dedup miss: {reason:?}"
                            )));
                        }
                        None => {
                            return Err(internal_wire(
                                "full dedup miss omitted its rejection reason",
                            ));
                        }
                    }
                    break;
                }
            }
        }

        if !full_mode {
            if let Some(dedup_key) = row
                .dedup_key
                .clone()
                .filter(|_| row.gate_manifest.is_none())
            {
                let evidence = parse_evidence_specs(&row.evidence)
                    .expect("guardrail validation canonicalized evidence before charging fanout");
                let witness_path = context.paths.witness_path();
                drop(context);
                let probe = tokio::task::spawn_blocking(move || {
                    let (report, witness) = read_verified_records(&witness_path)?;
                    if !report.ok {
                        return Err(WitnessError::Corrupt(
                            "witness verification failed during dedup probe".to_owned(),
                        ));
                    }
                    Ok(probe_dedup(Some(&dedup_key), &evidence, &witness))
                })
                .await;
                context = self.context.write().await;
                let dedup = match probe {
                    Ok(Ok(dedup)) => dedup,
                    Ok(Err(error)) => {
                        rollback_child_charge(
                            &mut context,
                            caller_job_id.as_deref(),
                            child_charged,
                        )?;
                        return Err(internal_wire(error));
                    }
                    Err(error) => {
                        rollback_child_charge(
                            &mut context,
                            caller_job_id.as_deref(),
                            child_charged,
                        )?;
                        return Err(internal_wire(format!("dedup worker failed: {error}")));
                    }
                };
                if dedup.hit {
                    if let Err(error) =
                        store_admitted_brief(&context.paths, &row, prepared_brief.as_ref())
                    {
                        rollback_child_charge(
                            &mut context,
                            caller_job_id.as_deref(),
                            child_charged,
                        )?;
                        return Err(error);
                    }
                    let artifact_hash = dedup
                        .artifact_hash
                        .clone()
                        .expect("a dedup hit always carries an artifact hash");
                    let matched_witness_seq = dedup
                        .matched_witness_seq
                        .expect("a dedup hit always carries a matched witness");
                    let event = match DurableEnqueueEvent::new_reuse_with_depth(
                        row.clone(),
                        resolved.depth,
                        matched_witness_seq,
                        artifact_hash.clone(),
                    )
                    .and_then(|event| event.with_ingress_id(ingress_id.clone()))
                    {
                        Ok(event) => event,
                        Err(error) => {
                            rollback_child_charge(
                                &mut context,
                                caller_job_id.as_deref(),
                                child_charged,
                            )?;
                            return Err(WireError::invalid(error.to_string()));
                        }
                    };
                    let job = Job {
                        job_id,
                        task_uuid,
                        row: row.clone(),
                        invocation: invocation.clone(),
                        labor_class: LaborClass::Reused,
                        state: JobState::Completed,
                        lease_id: None,
                        adopted: false,
                        adopted_invocation_id: None,
                        model_is_advisory: false,
                    };
                    let stable_key = job.stable_key();
                    let barrier = context.barriers.register_job(&stable_key, row.attempt);
                    // The durable reuse disposition is the crash-repair marker for
                    // the following canonical verdict append. Recovery completes
                    // exactly this witness and can never execute the row as Fresh.
                    let events_dir = context.paths.events_dir();
                    if let Err(error) = write_enqueue_event_atomic(&events_dir, &event) {
                        let renamed = events_dir.join(format!("{}.enqueue.json", event.event_id));
                        if renamed.exists() {
                            return Err(self.fail_stop(error.into()));
                        }
                        rollback_child_charge(
                            &mut context,
                            caller_job_id.as_deref(),
                            child_charged,
                        )?;
                        if matches!(&error, TaskDbError::InvalidEvent { .. }) {
                            return Err(WireError::invalid(error.to_string()));
                        }
                        return Err(internal_wire(error));
                    }
                    let record = match context.witness.append(WitnessBody {
                        task_uuid: task_uuid.map(|uuid| uuid.to_string()),
                        transition_timestamp: Utc::now()
                            .to_rfc3339_opts(SecondsFormat::Millis, true),
                        verdict: Verdict::Reused,
                        exit_code: 0,
                        artifact_content_hash: Some(artifact_hash.clone()),
                        gpu_seconds: None,
                        wall_clock: 0.0,
                        attempt: row.attempt,
                        lease_epoch: row.lease_epoch,
                        dedup_key: row.dedup_key.clone(),
                        payload_hash: row.payload_hash.clone(),
                        brief_hash: row.brief_hash.clone(),
                        orchestration: row.orchestration.clone(),
                        labor_class: LaborClass::Reused,
                        trace_ref: None,
                        pools: Some(row.pools.clone()),
                        executor: row.executor.clone(),
                        charge: None,
                        model: row.model.clone(),
                        evidence_class: row.evidence_class.clone(),
                        manifest_hash: row.manifest_hash.clone(),
                        completion: None,
                    }) {
                        Ok(record) => record,
                        Err(error) => return Err(self.fail_stop(error.into())),
                    };
                    let result = JobResult {
                        task_uuid: task_uuid.map(|uuid| uuid.to_string()),
                        job_id: job_id.to_string(),
                        verdict: Verdict::Reused,
                        exit_code: 0,
                        artifact_content_hash: Some(artifact_hash.clone()),
                        attempt: row.attempt,
                        lease_epoch: row.lease_epoch,
                        witness_seq: record.seq,
                        model: record.model.clone(),
                        completion: None,
                    };
                    context.barriers.complete_job(&stable_key, result.value());
                    rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                    context.aliases.insert(job_id.to_string(), job_id);
                    context.aliases.insert(stable_key.clone(), job_id);
                    context
                        .query_rows
                        .insert(row_uuid, query_row(&row, RowStatus::Completed));
                    context.rows.insert(row_uuid, row.clone());
                    context.guardrail_depths.insert(row_uuid, resolved.depth);
                    context.query_details.insert(
                        row_uuid,
                        RowDetailFact::from_seed(&row, RowStatus::Completed, LaborClass::Reused),
                    );
                    context.jobs.insert(job_id, job.clone());
                    let evidence = serde_json::to_string(&row.evidence).map_err(internal_wire)?;
                    drop(context);
                    if self.commits.send(CommitCommand::Rebuild).is_err() {
                        eprintln!("tally: post-ack replica worker stopped before reuse projection");
                    }
                    self.complete_gh_post_ack(job.row.clone(), result.clone());
                    self.emit_post_ack(enqueued_event(&job));
                    self.emit_post_ack(completed_event(&job, &result, evidence));
                    return Ok(json!({
                        "schemaVersion": 1,
                        "disposition": "reused",
                        "task_uuid": task_uuid.map(|uuid| uuid.to_string()),
                        "job_id": job_id.to_string(),
                        "barrier": barrier,
                        "state": "reused",
                        "status": "reused",
                        "verdict": Verdict::Reused,
                        "dedup_key": dedup.dedup_key,
                        "artifact_content_hash": dedup.artifact_hash,
                        "witness_lsn": dedup.matched_witness_seq,
                    }));
                }
            }
        }

        if full_mode
            && (context.jobs.contains_key(&job_id) || context.query_rows.contains_key(&job_id))
        {
            return Err(WireError::invalid(format!(
                "task UUID {job_id} is already admitted"
            )));
        }

        if let Err(error) = enforce_flow_node_cap(&context, &row) {
            rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
            return Err(error);
        }

        if full_mode {
            if let Some(caller_job_id) = caller_job_id.as_deref() {
                context.guardrails.charge_child(caller_job_id)?;
                child_charged = true;
            }
        }

        let stable_key = row_uuid.to_string();
        let mut job = Job {
            job_id,
            task_uuid,
            row: row.clone(),
            invocation,
            labor_class: LaborClass::Fresh,
            state: JobState::Queued,
            lease_id: None,
            adopted: false,
            adopted_invocation_id: None,
            model_is_advisory: false,
        };
        let unit = self.executor.unit_name(&job.identity());
        let request = lease_request(&job, unit);
        if let Err(error) = context.lease.engine().validate_admission(&request) {
            rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
            return Err(lease_wire(error));
        }
        if let Err(error) = store_admitted_brief(&context.paths, &row, prepared_brief.as_ref()) {
            rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
            return Err(error);
        }

        if task_uuid.is_some() {
            let event = match DurableEnqueueEvent::new_with_depth(row.clone(), resolved.depth)
                .and_then(|event| event.with_ingress_id(ingress_id))
            {
                Ok(event) => event,
                Err(error) => {
                    rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                    return Err(WireError::invalid(error.to_string()));
                }
            };
            let events_dir = context.paths.events_dir();
            if let Err(error) = write_enqueue_event_atomic(&events_dir, &event) {
                let renamed = events_dir.join(format!("{}.enqueue.json", event.event_id));
                if renamed.exists() {
                    return Err(self.fail_stop(error.into()));
                }
                rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                if matches!(&error, TaskDbError::InvalidEvent { .. }) {
                    return Err(WireError::invalid(error.to_string()));
                }
                return Err(internal_wire(error));
            }
        }

        let barrier = context.barriers.register_job(&stable_key, row.attempt);
        let mut launch = None;
        if row.pools.iter().any(|pool| {
            context.paused_pools.contains(pool) || context.unreachable_pools.contains(pool)
        }) {
            job.state = JobState::Paused;
            if row
                .pools
                .iter()
                .any(|pool| context.unreachable_pools.contains(pool))
            {
                context.unreachable_paused_jobs.insert(job_id);
            }
        } else {
            match context.lease.admit(request, Utc::now()) {
                Ok(AdmitOutcome::Granted(grant)) => {
                    job.lease_id = Some(grant.lease_id.clone());
                    job.state = JobState::Running;
                    context.lease_jobs.insert(grant.lease_id, job_id);
                    launch = Some(job.clone());
                }
                Ok(AdmitOutcome::Queued {
                    ticket_id,
                    position: _,
                }) => {
                    job.lease_id = Some(ticket_id.clone());
                    context.lease_jobs.insert(ticket_id, job_id);
                }
                Err(error) => {
                    eprintln!(
                        "tally: admitted job {} is waiting for lease retry: {error}",
                        job.stable_key()
                    );
                }
            }
        }
        context.aliases.insert(job_id.to_string(), job_id);
        context.aliases.insert(stable_key.clone(), job_id);
        context.guardrails.register_parent(
            job_id.to_string(),
            ParentInfo {
                parent_uuid: stable_key.clone(),
                depth: resolved.depth,
                outstanding: 0,
                no_enqueue: row.no_enqueue,
                terminal: false,
            },
        );
        if task_uuid.is_some() {
            context
                .query_rows
                .insert(row_uuid, query_row(&row, RowStatus::Pending));
            context.rows.insert(row_uuid, row.clone());
            context.guardrail_depths.insert(row_uuid, resolved.depth);
            context.query_details.insert(
                row_uuid,
                RowDetailFact::from_seed(&row, RowStatus::Pending, LaborClass::Fresh),
            );
        }
        context.jobs.insert(job_id, job.clone());
        drop(context);

        if task_uuid.is_some()
            && self
                .commits
                .send(CommitCommand::Upsert {
                    row: Box::new(row.clone()),
                    status: Status::Pending,
                    labor_class: LaborClass::Fresh,
                })
                .is_err()
        {
            eprintln!("tally: post-ack replica worker stopped before enqueue projection");
        }
        self.emit_post_ack(enqueued_event(&job));
        if let Some(job) = launch {
            self.spawn_execution(job);
        }
        let mut response = json!({
            "schemaVersion": 1,
            "disposition": "created",
            "task_uuid": task_uuid.map(|uuid| uuid.to_string()),
            "job_id": job_id.to_string(),
            "barrier": barrier,
            "state": state_name(job.state),
        });
        if full_mode {
            response["payloadHash"] = Value::String(
                row.payload_hash
                    .clone()
                    .expect("full-mode rows always carry a payload hash"),
            );
            response["attempt"] = json!(row.attempt);
            if let Some(reason) = reused_rejected {
                response["reusedRejected"] = Value::String(reason.to_owned());
            }
            if let Some(detail) = reuse_error_detail {
                response["errorDetail"] = Value::String(detail);
            }
        }
        Ok(response)
    }

    async fn await_job(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            #[serde(default)]
            task_uuid: Option<String>,
            #[serde(default)]
            job_id: Option<String>,
            #[serde(default)]
            attempt: Option<u32>,
        }
        let params: Params = decode_params(params)?;
        if params.attempt == Some(0) {
            return Err(WireError::invalid("attempt must be positive"));
        }
        let requested_attempt = params.attempt;
        let presented = match (params.task_uuid, params.job_id) {
            (Some(task_uuid), None) => task_uuid,
            (None, Some(job_id)) => job_id,
            _ => {
                return Err(WireError::invalid(
                    "provide exactly one of task_uuid or job_id",
                ));
            }
        };
        let (registration, witness_lookup) = {
            let mut context = self.context.write().await;
            let job_id = context
                .aliases
                .get(&presented)
                .copied()
                .or_else(|| {
                    Uuid::parse_str(&presented).ok().filter(|uuid| {
                        context.jobs.contains_key(uuid) || context.rows.contains_key(uuid)
                    })
                })
                .ok_or_else(|| WireError::not_found(format!("job {presented} was not found")))?;
            let stable = context
                .jobs
                .get(&job_id)
                .map(Job::stable_key)
                .unwrap_or_else(|| job_id.to_string());
            let current = context.jobs.get(&job_id);
            if current.is_some_and(|job| {
                job.state != JobState::Completed
                    && requested_attempt.is_none_or(|attempt| job.row.attempt == attempt)
            }) {
                (Some(context.barriers.wait_job(&stable)), None)
            } else {
                (
                    None,
                    Some((
                        context.paths.witness_path(),
                        stable,
                        requested_attempt
                            .or_else(|| context.rows.get(&job_id).map(|row| row.attempt)),
                    )),
                )
            }
        };
        if let Some(registration) = registration {
            return await_registration(registration).await;
        }
        let (path, stable, attempt) =
            witness_lookup.expect("terminal witness lookup was selected above");
        tokio::task::spawn_blocking(move || reconstruct_job_result(&path, &stable, attempt))
            .await
            .map_err(|error| internal_wire(format!("witness await worker failed: {error}")))?
    }

    async fn await_barrier(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            barrier: String,
        }
        let params: Params = decode_params(params)?;
        if params.barrier.starts_with("barrier:drain:") {
            let registration = self
                .context
                .write()
                .await
                .barriers
                .wait_barrier(&params.barrier)?;
            return await_registration(registration).await;
        }
        let (presented, attempt) = parse_job_barrier(&params.barrier)?;
        let (registration, witness_lookup, stable) = {
            let mut context = self.context.write().await;
            let job_id = context
                .aliases
                .get(presented)
                .copied()
                .or_else(|| {
                    Uuid::parse_str(presented).ok().filter(|uuid| {
                        context.jobs.contains_key(uuid) || context.rows.contains_key(uuid)
                    })
                })
                .ok_or_else(|| WireError::not_found(format!("job {presented} was not found")))?;
            let stable = context
                .jobs
                .get(&job_id)
                .map(Job::stable_key)
                .unwrap_or_else(|| job_id.to_string());
            if stable != presented {
                return Err(WireError::not_found(format!(
                    "barrier {} does not identify job {stable}",
                    params.barrier
                )));
            }
            if context
                .jobs
                .get(&job_id)
                .is_some_and(|job| job.state != JobState::Completed && job.row.attempt == attempt)
            {
                (Some(context.barriers.wait_job(&stable)), None, stable)
            } else {
                (
                    None,
                    Some((context.paths.witness_path(), stable.clone(), attempt)),
                    stable,
                )
            }
        };
        let result = if let Some(registration) = registration {
            await_registration(registration).await?
        } else {
            let (path, stable, attempt) =
                witness_lookup.expect("terminal barrier lookup was selected above");
            tokio::task::spawn_blocking(move || {
                reconstruct_job_result(&path, &stable, Some(attempt))
            })
            .await
            .map_err(|error| internal_wire(format!("witness barrier worker failed: {error}")))??
        };
        Ok(single_job_barrier_value(&params.barrier, &stable, result))
    }

    async fn drain(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            #[serde(default)]
            producer: Option<String>,
        }
        let params: Params = decode_params(params)?;
        if let Some(producer) = &params.producer {
            let context = self.context.read().await;
            let configured = context
                .config
                .producers
                .get(producer)
                .ok_or_else(|| WireError::invalid(format!("unknown producer {producer:?}")))?;
            if configured.kind() != "events-dir" {
                return Err(WireError::invalid(format!(
                    "producer {producer:?} is not an events-dir producer"
                )));
            }
        }
        let _sweep = self.ingress_sweep.lock().await;
        let events_dir = self.context.read().await.paths.events_dir();
        let claims = claim_ingress_files(&events_dir).map_err(internal_wire)?;
        let acknowledged = acknowledged_ingress_ids(&events_dir).map_err(internal_wire)?;
        let mut outcomes = Vec::with_capacity(claims.len());
        let mut enqueued = 0_u64;
        let mut rejected = 0_u64;
        let mut repaired = 0_u64;
        for claim in claims {
            if acknowledged.contains(&claim.ingress_id) {
                let archived_to =
                    archive_ingress_claim(&events_dir, &claim, true).map_err(internal_wire)?;
                repaired = repaired.saturating_add(1);
                outcomes.push(IngressOutcome {
                    file: claim.original_name,
                    status: "accepted".to_owned(),
                    archived_to: Some(archived_to),
                    reason: Some("repaired acknowledged archive after interruption".to_owned()),
                });
                continue;
            }
            let mut payload = match read_ingress_payload(&claim) {
                Ok(payload) => payload,
                Err(error @ crate::producers::ProducerError::Io { .. }) => {
                    return Err(internal_wire(error));
                }
                Err(error) => {
                    let reason = format!("invalid enqueue params: {error}");
                    let archived_to =
                        archive_ingress_claim(&events_dir, &claim, false).map_err(internal_wire)?;
                    eprintln!(
                        "tally: rejected producer ingress {}: {reason}",
                        claim.original_name
                    );
                    rejected = rejected.saturating_add(1);
                    outcomes.push(IngressOutcome {
                        file: claim.original_name,
                        status: "rejected".to_owned(),
                        archived_to: Some(archived_to),
                        reason: Some(reason),
                    });
                    continue;
                }
            };
            if payload.origin.is_none() {
                if payload.source.is_none() && params.producer.is_some() {
                    payload.source = Some(EnqueueSource::EventsDir);
                }
                if payload.source == Some(EnqueueSource::EventsDir) {
                    payload.origin = Some(params.producer.as_ref().map_or_else(
                        || AdmissionOrigin::direct(EnqueueSource::EventsDir),
                        |producer| AdmissionOrigin::producer(producer, EnqueueSource::EventsDir),
                    ));
                }
            }
            match self
                .enqueue_payload(payload, Some(claim.ingress_id.clone()))
                .await
            {
                Ok(_) => {
                    let archived_to =
                        archive_ingress_claim(&events_dir, &claim, true).map_err(internal_wire)?;
                    enqueued = enqueued.saturating_add(1);
                    outcomes.push(IngressOutcome {
                        file: claim.original_name,
                        status: "accepted".to_owned(),
                        archived_to: Some(archived_to),
                        reason: None,
                    });
                }
                Err(error)
                    if matches!(
                        error.code,
                        WireErrorCode::InvalidParams | WireErrorCode::NotFound
                    ) =>
                {
                    let reason = format!("enqueue failed: {}", error.message);
                    let archived_to =
                        archive_ingress_claim(&events_dir, &claim, false).map_err(internal_wire)?;
                    eprintln!(
                        "tally: rejected producer ingress {}: {reason}",
                        claim.original_name
                    );
                    rejected = rejected.saturating_add(1);
                    outcomes.push(IngressOutcome {
                        file: claim.original_name,
                        status: "rejected".to_owned(),
                        archived_to: Some(archived_to),
                        reason: Some(reason),
                    });
                }
                Err(error) => return Err(error),
            }
        }
        let mut context = self.context.write().await;
        let active = context
            .jobs
            .values()
            .filter(|job| job.state != JobState::Completed)
            .map(Job::stable_key)
            .collect::<Vec<_>>();
        let barrier = context.barriers.snapshot(active);
        Ok(json!({
            "barrier": barrier,
            "enqueued": enqueued,
            "rejected": rejected,
            "repaired": repaired,
            "represented": 0,
            "outcomes": outcomes,
        }))
    }

    async fn pause(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        struct Params {
            #[serde(default)]
            pool: Option<String>,
            #[serde(default)]
            all: bool,
        }
        let params: Params = decode_params(params)?;
        let mut context = self.context.write().await;
        let pools = selected_pools(&context.config, params.pool, params.all)?;
        for pool in &pools {
            context.paused_pools.insert(pool.clone());
        }
        let queued = context
            .jobs
            .values()
            .filter(|job| {
                job.state == JobState::Queued
                    && job.row.pools.iter().any(|pool| pools.contains(pool))
            })
            .map(|job| (job.job_id, job.lease_id.clone()))
            .collect::<Vec<_>>();
        for (job_id, lease_id) in &queued {
            if let Some(lease_id) = lease_id {
                let epoch = context.epoch;
                context
                    .lease
                    .engine_mut()
                    .cancel_pending_at(lease_id, epoch, Utc::now())
                    .map_err(lease_wire)?;
                context.lease_jobs.remove(lease_id);
            }
            let job = context.jobs.get_mut(job_id).expect("queued job exists");
            job.lease_id = None;
            job.state = JobState::Paused;
        }
        let affected = queued.len();
        drop(context);
        for pool in &pools {
            self.append_change(
                ChangeKind::Pool,
                json!({"pool": pool, "update": "paused", "affected": affected}),
            )?;
        }
        Ok(json!({"paused": pools, "affected": affected}))
    }

    async fn resume(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        struct Params {
            #[serde(default)]
            pool: Option<String>,
            #[serde(default)]
            all: bool,
        }
        let params: Params = decode_params(params)?;
        let mut context = self.context.write().await;
        let pools = selected_pools(&context.config, params.pool, params.all)?;
        for pool in &pools {
            context.paused_pools.remove(pool);
        }
        let paused_jobs = context
            .jobs
            .values()
            .filter(|job| {
                job.state == JobState::Paused
                    && job.row.pools.iter().any(|pool| pools.contains(pool))
                    && !job.row.pools.iter().any(|pool| {
                        context.paused_pools.contains(pool)
                            || context.unreachable_pools.contains(pool)
                    })
            })
            .map(|job| job.job_id)
            .collect::<Vec<_>>();
        for job_id in &paused_jobs {
            context.unreachable_paused_jobs.remove(job_id);
        }
        let launches = resume_paused_jobs_locked(&mut context, &self.executor, paused_jobs);
        drop(context);
        for job in launches {
            self.spawn_execution(job);
        }
        for pool in &pools {
            self.append_change(ChangeKind::Pool, json!({"pool": pool, "update": "resumed"}))?;
        }
        Ok(json!({"resumed": pools}))
    }

    async fn producer_runtime_observed(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            producer: String,
        }
        let params: Params = decode_params(params)?;
        if !self
            .context
            .read()
            .await
            .config
            .producers
            .contains_key(&params.producer)
        {
            return Err(WireError::invalid(format!(
                "unknown producer {:?}",
                params.producer
            )));
        }
        self.append_change(
            ChangeKind::Producer,
            json!({
                "name": params.producer,
                "update": "runtime-observation-recorded",
            }),
        )?;
        Ok(json!({"observed": true}))
    }

    async fn pool_transition(&self, params: Option<Value>) -> Result<Value, WireError> {
        let handler = self.clone();
        let (result_tx, result_rx) = oneshot::channel();
        let task = tokio::task::spawn_local(async move {
            let result = handler.pool_transition_inner(params).await;
            let task_result = result.clone().map(|_| ());
            let _ = result_tx.send(result);
            task_result
        });
        {
            let mut tasks = self.pool_transition_tasks.borrow_mut();
            tasks.retain(|task| !task.is_finished());
            tasks.push(task);
        }
        result_rx
            .await
            .map_err(|_| internal_wire("pool transition task stopped before replying"))?
    }

    async fn pool_transition_inner(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct Params {
            producer: String,
            transition: ReachabilityTransition,
            generation: u64,
        }
        let params: Params = decode_params(params)?;
        let _sweep = self.pool_transition_sweep.lock().await;
        let (pool, state_dir) = {
            let context = self.context.read().await;
            let engine = ProducerEngine::new(
                &context.config.producers,
                context.paths.events_dir(),
                &context.paths.state_dir,
            );
            let pool = engine
                .validate_reachability_transition(
                    &params.producer,
                    params.transition,
                    params.generation,
                )
                .map_err(|error| WireError::invalid(error.to_string()))?;
            (pool, context.paths.state_dir.clone())
        };
        let key = (params.producer.clone(), params.generation);
        let marker = pool_transition_marker(&state_dir, &params.producer, params.generation);
        if pool_transition_marker_exists(&marker).map_err(internal_wire)?
            || self
                .context
                .read()
                .await
                .applied_pool_transitions
                .contains(&key)
        {
            return Ok(json!({
                "applied": false,
                "alreadyApplied": true,
                "pool": pool,
                "transition": params.transition,
                "generation": params.generation,
            }));
        }

        let affected = match params.transition {
            ReachabilityTransition::Lost => self.apply_pool_loss(&pool).await?,
            ReachabilityTransition::Returned => self.apply_pool_return(&pool).await?,
        };
        write_pool_transition_marker(
            &marker,
            &params.producer,
            params.transition,
            params.generation,
        )
        .map_err(|error| self.fail_stop(error))?;
        self.context
            .write()
            .await
            .applied_pool_transitions
            .insert(key);
        self.append_change(
            ChangeKind::Pool,
            json!({
                "pool": pool,
                "producer": params.producer,
                "update": params.transition,
                "generation": params.generation,
                "affected": affected,
            }),
        )?;
        Ok(json!({
            "applied": true,
            "alreadyApplied": false,
            "pool": pool,
            "transition": params.transition,
            "generation": params.generation,
            "affected": affected,
        }))
    }

    async fn apply_pool_loss(&self, pool: &str) -> Result<usize, WireError> {
        let mut context = self.context.write().await;
        context.unreachable_pools.insert(pool.to_owned());
        let queued = context
            .jobs
            .values()
            .filter(|job| {
                job.state == JobState::Queued && job.row.pools.iter().any(|name| name == pool)
            })
            .map(|job| (job.job_id, job.lease_id.clone()))
            .collect::<Vec<_>>();
        for (job_id, lease_id) in queued {
            if let Some(lease_id) = lease_id {
                let epoch = context.epoch;
                if let Err(error) =
                    context
                        .lease
                        .engine_mut()
                        .cancel_pending_at(&lease_id, epoch, Utc::now())
                {
                    return Err(self.fail_stop(error.into()));
                }
                context.lease_jobs.remove(&lease_id);
            }
            let job = context.jobs.get_mut(&job_id).expect("queued job exists");
            job.lease_id = None;
            job.state = JobState::Paused;
            context.unreachable_paused_jobs.insert(job_id);
        }
        let targets = context
            .jobs
            .values()
            .filter(|job| {
                job.state == JobState::Running
                    && job.row.pools.iter().any(|name| name == pool)
                    && job.lease_id.is_some()
            })
            .map(|job| job.job_id)
            .collect::<Vec<_>>();
        let mut terminal = Vec::new();
        for job_id in &targets {
            let job = context
                .jobs
                .get(job_id)
                .cloned()
                .expect("pool-loss target exists");
            let intent_path = write_pool_loss_intent(&context.paths.state_dir, &job)
                .map_err(|error| self.fail_stop(error))?;
            if let Err(error) = self
                .executor
                .reclaim_identity_exact_on(
                    job.row.executor.as_deref(),
                    &job.identity(),
                    job.adopted_invocation_id.as_deref(),
                    job.row.attempt,
                    job.row.lease_epoch,
                )
                .await
            {
                return Err(self.fail_stop(error.into()));
            }
            let _ = self.execution_cancel.send(job.job_id);
            let scrape_capture = match self.executor.capture_generation_matches(
                &job.identity(),
                job.row.attempt,
                job.row.lease_epoch,
            ) {
                Ok(matches) => matches,
                Err(error) => {
                    eprintln!(
                        "tally: pool-vanished job {} capture generation is unavailable: {error}",
                        job.stable_key()
                    );
                    false
                }
            };
            match finalize_forced_locked(
                &mut context,
                *job_id,
                Verdict::PoolVanished,
                true,
                scrape_capture,
            ) {
                Ok(Some(work)) => terminal.push(work),
                Ok(None) => {}
                Err(error) => return Err(self.fail_stop(error)),
            }
            clear_pool_loss_intent(&intent_path).map_err(|error| self.fail_stop(error))?;
        }
        drop(context);
        for work in terminal {
            self.complete_terminal_post_ack(work);
        }
        Ok(targets.len())
    }

    async fn apply_pool_return(&self, pool: &str) -> Result<usize, WireError> {
        let (config, paths, epoch, auto_resume) = {
            let context = self.context.read().await;
            let pool_config = context
                .config
                .pools
                .get(pool)
                .ok_or_else(|| WireError::invalid(format!("unknown pool {pool:?}")))?;
            (
                context.config.clone(),
                context.paths.clone(),
                context.epoch,
                pool_config.auto_resume_enabled(),
            )
        };

        let mut plan = if auto_resume {
            let durable =
                collect_durable_recovery_facts(&paths.events_dir(), &paths.witness_path())
                    .map_err(|error| self.fail_stop(error.into()))?;
            let units = collect_local_unit_facts(&self.executor, &durable)
                .await
                .map_err(|error| self.fail_stop(error.into()))?;
            let facts = RecoveryFacts {
                durable,
                current_lease_epoch: epoch,
                units,
                rowless_units: BTreeMap::new(),
                triggers: RecoveryTriggers {
                    confirmed_pool_returns: BTreeSet::from([pool.to_owned()]),
                    resource_returns: BTreeSet::new(),
                    bounded_requeues: BTreeSet::new(),
                },
                advisory_return_attestations: Vec::new(),
            };
            let mut policy = self.settings.recovery_policy;
            policy.retry.auto_pool_return = true;
            let plan = recover(&facts, policy).map_err(|error| self.fail_stop(error.into()))?;
            let selected = renderable_pool_return_rows(
                &plan,
                &config,
                &self.executor,
                &paths.attestations_path(),
            );
            pool_representations(plan, pool, &selected)
        } else {
            crate::recovery::RecoveryPlan {
                witness_lsn: 0,
                rows: Vec::new(),
                actions: Vec::new(),
                lease_epoch_fences: Vec::new(),
                advisory_return_attestations: Vec::new(),
            }
        };
        hydrate_represent_adapter_metadata(
            &mut plan,
            &config,
            &self.executor,
            &paths.attestations_path(),
        )
        .map_err(|error| self.fail_stop(error))?;
        let represented_rows = plan
            .rows
            .iter()
            .map(|row| row.row.clone())
            .collect::<Vec<_>>();

        let mut context = self.context.write().await;
        context.unreachable_pools.remove(pool);
        for recovery in &plan.rows {
            let row = &recovery.row;
            context.rows.insert(row.uuid, row.clone());
            context
                .guardrail_depths
                .insert(row.uuid, recovery.guardrail_depth);
            context
                .query_rows
                .insert(row.uuid, query_row(row, RowStatus::Pending));
            context.query_details.insert(
                row.uuid,
                RowDetailFact::from_seed(row, RowStatus::Pending, recovery.labor_class),
            );
        }
        let mut launches = install_recovery_jobs(&mut context, &plan, &self.executor)
            .map_err(|error| self.fail_stop(error))?;
        let paused = context
            .unreachable_paused_jobs
            .iter()
            .filter_map(|job_id| {
                context
                    .jobs
                    .get(job_id)
                    .filter(|job| {
                        job.row.pools.iter().any(|name| name == pool)
                            && !job
                                .row
                                .pools
                                .iter()
                                .any(|name| context.unreachable_pools.contains(name))
                    })
                    .map(|_| *job_id)
            })
            .collect::<Vec<_>>();
        for job_id in &paused {
            context.unreachable_paused_jobs.remove(job_id);
        }
        launches.extend(resume_paused_jobs_locked(
            &mut context,
            &self.executor,
            paused,
        ));
        drop(context);

        for row in &represented_rows {
            if self
                .commits
                .send(CommitCommand::Upsert {
                    row: Box::new(row.clone()),
                    status: Status::Pending,
                    labor_class: LaborClass::Recovered,
                })
                .is_err()
            {
                eprintln!("tally: post-ack replica worker stopped before pool-return projection");
            }
        }
        for job in launches {
            self.spawn_execution(job);
        }
        Ok(represented_rows.len())
    }

    async fn cancel(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        struct Params {
            task_uuid: String,
            #[serde(default)]
            force: bool,
        }
        let params: Params = decode_params(params)?;
        let mut context = self.context.write().await;
        let job = find_job(&context, &params.task_uuid)?.clone();
        let was = state_name(job.state);
        if job.state == JobState::Completed {
            return Ok(json!({
                "ok": true,
                "affected": 0,
                "task_uuid": job.task_uuid.map(|uuid| uuid.to_string()),
                "was": was,
                "lease_epoch": job.row.lease_epoch,
                "already_terminal": true,
            }));
        }
        if job.state == JobState::Running && !params.force {
            return Ok(json!({
                "ok": true,
                "affected": 0,
                "task_uuid": job.task_uuid.map(|uuid| uuid.to_string()),
                "was": was,
                "lease_epoch": job.row.lease_epoch,
            }));
        }
        let scrape_capture = if job.state == JobState::Running {
            let identity = job.identity();
            if let Err(error) = self
                .executor
                .reclaim_identity_exact_on(
                    job.row.executor.as_deref(),
                    &identity,
                    job.adopted_invocation_id.as_deref(),
                    job.row.attempt,
                    job.row.lease_epoch,
                )
                .await
            {
                return Err(internal_wire(error.to_string()));
            }
            let _ = self.execution_cancel.send(job.job_id);
            match self.executor.capture_generation_matches(
                &identity,
                job.row.attempt,
                job.row.lease_epoch,
            ) {
                Ok(matches) => matches,
                Err(error) => {
                    eprintln!(
                        "tally: cancelled job {} capture generation is unavailable: {error}",
                        job.stable_key()
                    );
                    false
                }
            }
        } else {
            false
        };
        let work = match finalize_forced_locked(
            &mut context,
            job.job_id,
            Verdict::Cancelled,
            true,
            scrape_capture,
        ) {
            Ok(work) => work,
            Err(error) => return Err(self.fail_stop(error)),
        };
        drop(context);
        if let Some(work) = work {
            self.complete_terminal_post_ack(work);
        }
        Ok(json!({
            "ok": true,
            "affected": 1,
            "task_uuid": job.task_uuid.map(|uuid| uuid.to_string()),
            "was": was,
            "lease_epoch": job.row.lease_epoch,
        }))
    }

    fn complete_terminal_post_ack(&self, work: TerminalWork) {
        if self.commits.send(CommitCommand::Rebuild).is_err() {
            eprintln!("tally: post-ack replica worker stopped before terminal projection");
        }
        let status = if work.result.verdict == Verdict::Cancelled {
            Status::Deleted
        } else {
            Status::Completed
        };
        if work.job.task_uuid.is_some()
            && self
                .commits
                .send(CommitCommand::Upsert {
                    row: Box::new(work.job.row.clone()),
                    status,
                    labor_class: work.job.labor_class,
                })
                .is_err()
        {
            eprintln!("tally: post-ack replica worker stopped before terminal row projection");
        }
        self.complete_gh_post_ack(work.job.row.clone(), work.result.clone());
        for check in &work.evidence_checks {
            self.emit_post_ack(evidence_event(&work.job, check));
        }
        self.emit_scraped_completion(work.job, work.result, work.evidence, work.scrape_capture);
        for job in work.launches {
            self.spawn_execution(job);
        }
    }

    fn complete_gh_post_ack(&self, row: RowSeed, result: JobResult) {
        let Some(origin) = row.gh_origin.clone() else {
            return;
        };
        let handler = self.clone();
        let task = tokio::task::spawn_local(async move {
            let (registry, events_dir, state_dir, gh_program, mut shutdown) = {
                let context = handler.context.read().await;
                (
                    context.config.producers.clone(),
                    context.paths.events_dir(),
                    context.paths.state_dir.clone(),
                    handler.gh_program.clone(),
                    handler.execution_shutdown.clone(),
                )
            };
            let completion_id = format!("{}:{}:{}", row.uuid, result.attempt, result.witness_seq);
            let mut evidence = json!({
                "taskUuid": row.uuid.to_string(),
                "witnessSeq": result.witness_seq,
                "verdict": result.verdict,
                "exitCode": result.exit_code,
                "artifactContentHash": result.artifact_content_hash,
                "adapter": row.adapter,
                "model": result.model,
            });
            if let Some(completion) = &result.completion {
                evidence["completion"] = serde_json::to_value(completion)
                    .expect("semantic completion always serializes");
            }
            let mut retry_delay = Duration::from_secs(1);
            loop {
                let registry = registry.clone();
                let events_dir = events_dir.clone();
                let state_dir = state_dir.clone();
                let gh_program = gh_program.clone();
                let origin = origin.clone();
                let completion_id = completion_id.clone();
                let evidence = evidence.clone();
                let semantic_completion = result.completion.clone();
                let verdict = result.verdict;
                let completed = tokio::task::spawn_blocking(move || {
                    let engine = ProducerEngine::new(&registry, events_dir, state_dir);
                    let mut sink = GhCliMutationSink::with_program(gh_program);
                    engine.complete_gh_once_with_completion(
                        &origin,
                        &completion_id,
                        verdict,
                        Some(evidence),
                        semantic_completion,
                        &mut sink,
                    )
                })
                .await;
                match completed {
                    Ok(Ok(_)) => break,
                    Ok(Err(error)) => eprintln!(
                        "tally: post-ack GitHub COMPLETED mutation failed for {} and will retry: {error}",
                        row.uuid
                    ),
                    Err(error) => eprintln!(
                        "tally: post-ack GitHub mutation worker failed for {} and will retry: {error}",
                        row.uuid
                    ),
                }
                if *shutdown.borrow() {
                    break;
                }
                tokio::select! {
                    _ = tokio::time::sleep(retry_delay) => {}
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            break;
                        }
                    }
                }
                retry_delay = retry_delay.saturating_mul(2).min(Duration::from_secs(60));
            }
        });
        let mut tasks = self.post_ack_tasks.borrow_mut();
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }

    fn emit_scraped_completion(
        &self,
        job: Job,
        result: JobResult,
        evidence: String,
        scrape_capture: bool,
    ) {
        if !scrape_capture {
            self.emit_post_ack(completed_event(&job, &result, evidence));
            return;
        }
        let handler = self.clone();
        let task = tokio::task::spawn_local(async move {
            let (adapters, attestation_path, state_dir, pools) = {
                let context = handler.context.read().await;
                (
                    context.config.adapters.clone(),
                    context.paths.attestations_path(),
                    context.paths.state_dir.clone(),
                    context.config.pools.clone(),
                )
            };
            let scrape_configured = adapters
                .get(&job.row.adapter)
                .is_some_and(|adapter| !adapter.scrape.is_empty());
            if !scrape_configured {
                handler.emit_post_ack(completed_event(&job, &result, evidence));
                return;
            }

            let paths = handler.executor.paths(&job.identity());
            let adapter = job.row.adapter.clone();
            let stable_key = job.stable_key();
            let job_id = job.job_id.to_string();
            let attempt = job.row.attempt;
            let lease_epoch = job.row.lease_epoch;
            let leased_pools = job.row.pools.clone();
            let scraped = tokio::task::spawn_blocking(move || {
                let captures = AdapterEngine::new(&adapters)
                    .scrape_paths(&adapter, &paths)
                    .map_err(|error| error.to_string())?;
                let attestation_error = if captures.captures.is_empty() {
                    None
                } else {
                    append_attestation(
                        &attestation_path,
                        json!({
                            "kind": "adapter-scrape",
                            "taskUuid": stable_key,
                            "jobId": job_id,
                            "adapter": adapter,
                            "attempt": attempt,
                            "leaseEpoch": lease_epoch,
                            "captures": captures.captures.clone(),
                            "usageAuthority": "advisory-only",
                        }),
                    )
                    .err()
                    .map(|error| error.to_string())
                };
                let meter_errors = feed_scraped_usage(&state_dir, &pools, &leased_pools, &captures);
                Ok::<_, String>((captures, attestation_error, meter_errors))
            })
            .await;

            let (captures, attestation_error, meter_errors) = match scraped {
                Ok(Ok(scraped)) => scraped,
                Ok(Err(error)) => {
                    eprintln!(
                        "tally: post-ack adapter scrape failed for {}: {error}",
                        job.stable_key()
                    );
                    handler.emit_post_ack(completed_event(&job, &result, evidence));
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "tally: post-ack adapter scrape worker failed for {}: {error}",
                        job.stable_key()
                    );
                    handler.emit_post_ack(completed_event(&job, &result, evidence));
                    return;
                }
            };
            for error in meter_errors {
                eprintln!(
                    "tally: built-in usage meter feeder failed for {}: {error}",
                    job.stable_key()
                );
            }
            if let Some(error) = attestation_error {
                eprintln!(
                    "tally: post-ack adapter attestation failed for {}: {error}",
                    job.stable_key()
                );
                handler.emit_post_ack(completed_event(&job, &result, evidence));
                return;
            }

            let mut enriched = job;
            if let Ok(Some(session_ref)) = captures.session_ref() {
                enriched.row.session_ref = Some(session_ref.to_owned());
            }
            if let Ok(Some(model)) = captures.model() {
                enriched.row.model = Some(model.to_owned());
            }
            if let Ok(Some(final_message)) = captures.final_message() {
                enriched.row.final_message = Some(final_message.to_owned());
            }
            {
                let mut context = handler.context.write().await;
                if let Some(stored) = context.jobs.get_mut(&enriched.job_id) {
                    stored.row.session_ref.clone_from(&enriched.row.session_ref);
                    stored.row.model.clone_from(&enriched.row.model);
                    stored
                        .row
                        .final_message
                        .clone_from(&enriched.row.final_message);
                }
                if let Some(task_uuid) = enriched.task_uuid {
                    if let Some(row) = context.query_rows.get_mut(&task_uuid) {
                        row.session_ref.clone_from(&enriched.row.session_ref);
                        row.model.clone_from(&enriched.row.model);
                        row.final_message.clone_from(&enriched.row.final_message);
                    }
                    if let Some(detail) = context.query_details.get_mut(&task_uuid) {
                        detail.session_ref.clone_from(&enriched.row.session_ref);
                        detail.observed_model.clone_from(&enriched.row.model);
                        detail.final_message.clone_from(&enriched.row.final_message);
                    }
                }
            }
            if enriched.task_uuid.is_some()
                && handler
                    .commits
                    .send(CommitCommand::Upsert {
                        row: Box::new(enriched.row.clone()),
                        status: if result.verdict == Verdict::Cancelled {
                            Status::Deleted
                        } else {
                            Status::Completed
                        },
                        labor_class: enriched.labor_class,
                    })
                    .is_err()
            {
                eprintln!("tally: post-ack replica worker stopped before scrape projection");
            }
            handler.emit_post_ack(completed_event(&enriched, &result, evidence));
        });
        let mut tasks = self.post_ack_tasks.borrow_mut();
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }

    async fn drain_post_ack_tasks(&self) {
        loop {
            let tasks = std::mem::take(&mut *self.post_ack_tasks.borrow_mut());
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                if let Err(error) = task.await {
                    eprintln!("tally: post-ack task failed during shutdown: {error}");
                }
            }
        }
    }

    async fn drain_pool_transition_tasks(&self) -> Result<(), DaemonError> {
        let mut first_error = None;
        loop {
            let tasks = std::mem::take(&mut *self.pool_transition_tasks.borrow_mut());
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                let error = match task.await {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(DaemonError::Invalid(format!(
                        "pool transition failed during shutdown: {}",
                        error.message
                    ))),
                    Err(error) => Some(DaemonError::Invalid(format!(
                        "pool transition task failed during shutdown: {error}"
                    ))),
                };
                if first_error.is_none() {
                    first_error = error;
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn acquire(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        struct Params {
            #[serde(deserialize_with = "crate::poolset::deserialize")]
            pool: Vec<String>,
        }
        let mut params: Params = decode_params(params)?;
        crate::poolset::canonicalize(&mut params.pool)
            .map_err(|error| WireError::invalid(error.to_string()))?;
        let id = Uuid::new_v4();
        let mut context = self.context.write().await;
        let epoch = context.epoch;
        let outcome = match context.lease.admit(
            LeaseRequest {
                job_id: id.to_string(),
                unit: format!("tally-job-{id}.service"),
                pools: params.pool,
                // The additive acquire/release surface is an explicit
                // reservation token, not a daemon-owned execution. Keep
                // it outside managed hard-preemption; only daemon jobs
                // have a unit identity that tally is authorized to stop.
                priority: Priority::Interrupt,
                admission_key: None,
                consumption_estimate: None,
                scheduling_group: LeaseSchedulingGroup::Standalone,
            },
            Utc::now(),
        ) {
            Ok(outcome) => outcome,
            Err(
                error @ (LeaseError::UnknownPool(_)
                | LeaseError::InvalidRequest(_)
                | LeaseError::StaleEpoch { .. }
                | LeaseError::NotFound(_)),
            ) => return Err(lease_wire(error)),
            Err(error) => return Err(self.fail_stop(error.into())),
        };
        Ok(json!({"epoch": epoch, "outcome": outcome}))
    }

    async fn release(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        struct Params {
            lease: String,
        }
        let params: Params = decode_params(params)?;
        let mut context = self.context.write().await;
        let epoch = context.epoch;
        let outcome = match context.lease.release(&params.lease, epoch, Utc::now()) {
            Ok(outcome) => outcome,
            Err(
                error @ (LeaseError::UnknownPool(_)
                | LeaseError::InvalidRequest(_)
                | LeaseError::StaleEpoch { .. }
                | LeaseError::NotFound(_)),
            ) => return Err(lease_wire(error)),
            Err(error) => return Err(self.fail_stop(error.into())),
        };
        let promoted = outcome.promoted.clone();
        let launches = promoted_jobs(&mut context, outcome.promoted);
        drop(context);
        for job in launches {
            self.spawn_execution(job);
        }
        Ok(json!({"released": outcome.released, "promoted": promoted}))
    }

    async fn lease_status(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct Params {
            #[serde(default)]
            lease: Option<String>,
            #[serde(default)]
            job_id: Option<String>,
        }
        let params: Params = decode_params(params)?;
        let context = self.context.read().await;
        let lease = match (params.lease, params.job_id) {
            (Some(lease), None) if !lease.trim().is_empty() => lease,
            (None, Some(job_id)) if !job_id.trim().is_empty() => {
                let job = find_job(&context, &job_id)?;
                job.lease_id.clone().ok_or_else(|| {
                    WireError::not_found(format!("job {job_id} has no active lease"))
                })?
            }
            _ => {
                return Err(WireError::invalid(
                    "lease status requires exactly one non-empty lease or jobId",
                ))
            }
        };
        let status = context
            .lease
            .engine()
            .status(&lease, context.epoch)
            .map_err(lease_wire)?;
        serde_json::to_value(status).map_err(|error| internal_wire(error.to_string()))
    }

    async fn query(&self, method: &str, params: Option<Value>) -> Result<Value, WireError> {
        if method == "query.watch" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Params {
                #[serde(default)]
                after: Option<String>,
                #[serde(default)]
                limit: Option<usize>,
            }
            let params: Params = decode_params(params)?;
            return serde_json::to_value(
                self.changes
                    .borrow()
                    .watch(params.after.as_deref(), params.limit)
                    .map_err(change_wire)?,
            )
            .map_err(internal_wire);
        }
        if method == "query.producers" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Params {
                #[serde(default)]
                name: Option<String>,
                #[serde(default)]
                kind: Option<String>,
            }
            let params: Params = decode_params(params)?;
            let (registry, state_dir) = {
                let context = self.context.read().await;
                (
                    context.config.producers.clone(),
                    context.paths.state_dir.clone(),
                )
            };
            return serde_json::to_value(query_producers(
                &registry,
                &state_dir,
                params.name.as_deref(),
                params.kind.as_deref(),
            ))
            .map_err(internal_wire);
        }

        let history = self.history.borrow().snapshot();
        let journal = history
            .records
            .iter()
            .map(|record| JournalEntry {
                fields: record.fields.clone(),
                realtime_us: Some(record.realtime_us),
            })
            .collect::<Vec<_>>();
        let (rows, details, witness_path, attestations_path, live_states, live) = {
            let context = self.context.read().await;
            (
                context.query_rows.values().cloned().collect::<Vec<_>>(),
                context.query_details.values().cloned().collect::<Vec<_>>(),
                context.paths.witness_path(),
                context.paths.attestations_path(),
                context
                    .jobs
                    .values()
                    .filter(|job| job.state != JobState::Completed)
                    .map(|job| (job.stable_key(), state_name(job.state).to_owned()))
                    .collect::<HashMap<_, _>>(),
                context
                    .jobs
                    .values()
                    .filter(|job| job.state != JobState::Completed)
                    .map(|job| LiveJobFact {
                        anchor: job.stable_key(),
                        job_id: job.job_id.to_string(),
                        live_state: state_name(job.state).to_owned(),
                        attempt: job.row.attempt,
                        lease_epoch: job.row.lease_epoch,
                        unit: format!("tally-job-{}.service", job.stable_key()),
                        labor_class: job.labor_class,
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let (report, witness) = tokio::task::spawn_blocking(move || {
            crate::witness::read_verified_records(&witness_path)
        })
        .await
        .map_err(|error| internal_wire(format!("witness query worker failed: {error}")))?
        .map_err(internal_wire)?;
        if !report.ok {
            return Err(internal_wire("witness verification failed during query"));
        }

        match method {
            "query.jobs" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields, rename_all = "camelCase")]
                struct Params {
                    #[serde(default, alias = "state")]
                    live_state: Option<String>,
                    #[serde(default, alias = "verdict")]
                    terminal_verdict: Option<Verdict>,
                    #[serde(default)]
                    pool: Option<String>,
                    #[serde(default)]
                    executor: Option<String>,
                    #[serde(default)]
                    adapter: Option<String>,
                    #[serde(default)]
                    source: Option<String>,
                    #[serde(default)]
                    origin: Option<String>,
                    #[serde(default)]
                    parent: Option<String>,
                    #[serde(default)]
                    flow_run: Option<String>,
                    #[serde(default)]
                    session: Option<String>,
                    #[serde(default)]
                    since: Option<String>,
                    #[serde(default)]
                    until: Option<String>,
                    #[serde(default)]
                    limit: Option<usize>,
                    #[serde(default)]
                    cursor: Option<String>,
                }
                let params: Params = decode_params(params)?;
                let fingerprint = serde_json::to_string(&json!({
                    "liveState": params.live_state.clone(),
                    "terminalVerdict": params.terminal_verdict,
                    "pool": params.pool.clone(),
                    "executor": params.executor.clone(),
                    "adapter": params.adapter.clone(),
                    "source": params.source.clone(),
                    "origin": params.origin.clone(),
                    "parent": params.parent.clone(),
                    "flowRun": params.flow_run.clone(),
                    "session": params.session.clone(),
                    "since": params.since.clone(),
                    "until": params.until.clone(),
                }))
                .map_err(internal_wire)?;
                let envelope = if params.cursor.is_none() {
                    let pool_signals = {
                        let mut context = self.context.write().await;
                        query_pools(&pool_headroom_facts(&mut context)?)
                            .map_err(query_wire)?
                            .pools
                            .into_iter()
                            .map(|pool| (pool.pool, pool.signal))
                            .collect::<BTreeMap<_, _>>()
                    };
                    let lanes = trace_lanes(&details, &live, &history);
                    let adapters = {
                        let context = self.context.read().await;
                        context.config.adapters.clone()
                    };
                    let mut result = query_jobs_v2(
                        &details,
                        &live,
                        &history,
                        &witness,
                        &pool_signals,
                        &JobsFilter {
                            live_state: params.live_state,
                            terminal_verdict: params.terminal_verdict,
                            pool: params.pool,
                            executor: params.executor,
                            adapter: params.adapter,
                            source: params.source,
                            origin: params.origin,
                            parent: params.parent,
                            flow_run: params.flow_run,
                            session: params.session,
                            since: params.since,
                            until: params.until,
                        },
                    )
                    .map_err(observability_wire)?;
                    for item in &mut result.items {
                        item.trace =
                            trace_availability(&item.anchor, &lanes, &adapters, &self.executor);
                    }
                    Some(serde_json::to_value(result).map_err(internal_wire)?)
                } else {
                    None
                };
                self.pages
                    .borrow_mut()
                    .page(
                        method,
                        &fingerprint,
                        params.limit,
                        params.cursor.as_deref(),
                        envelope,
                    )
                    .map_err(pagination_wire)
            }
            "query.job" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    id: String,
                }
                let params: Params = decode_params(params)?;
                if params.id.trim().is_empty() {
                    return Err(WireError::invalid("query job ID must not be empty"));
                }
                let pool_signals = {
                    let mut context = self.context.write().await;
                    query_pools(&pool_headroom_facts(&mut context)?)
                        .map_err(query_wire)?
                        .pools
                        .into_iter()
                        .map(|pool| (pool.pool, pool.signal))
                        .collect::<BTreeMap<_, _>>()
                };
                let lanes = trace_lanes(&details, &live, &history);
                let adapters = {
                    let context = self.context.read().await;
                    context.config.adapters.clone()
                };
                let mut result = query_job_v2(
                    &params.id,
                    &details,
                    &live,
                    &history,
                    &witness,
                    &pool_signals,
                )
                .map_err(observability_wire)?;
                result.job.trace =
                    trace_availability(&result.job.anchor, &lanes, &adapters, &self.executor);
                serde_json::to_value(result).map_err(internal_wire)
            }
            "query.status" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    #[serde(default)]
                    pool: Option<String>,
                }
                let params: Params = decode_params(params)?;
                let pools = {
                    let mut context = self.context.write().await;
                    pool_headroom_facts(&mut context)?
                };
                let mut view =
                    query_status(&pools, params.pool.as_deref(), &rows, &journal, &witness)
                        .map_err(query_wire)?;
                overlay_live_states(&mut view.jobs, &live_states);
                serde_json::to_value(view).map_err(internal_wire)
            }
            "query.log" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields, rename_all = "camelCase")]
                struct Params {
                    #[serde(default)]
                    task: Option<String>,
                    #[serde(default)]
                    attempt: Option<u32>,
                    #[serde(default)]
                    session: Option<String>,
                    #[serde(default)]
                    event: Option<TallyEvent>,
                    #[serde(default)]
                    source: Option<String>,
                    #[serde(default)]
                    since: Option<String>,
                    #[serde(default)]
                    until: Option<String>,
                    #[serde(default)]
                    limit: Option<usize>,
                    #[serde(default)]
                    cursor: Option<String>,
                }
                let params: Params = decode_params(params)?;
                let fingerprint = serde_json::to_string(&json!({
                    "task": params.task.clone(),
                    "attempt": params.attempt,
                    "session": params.session.clone(),
                    "event": params.event,
                    "source": params.source.clone(),
                    "since": params.since.clone(),
                    "until": params.until.clone(),
                }))
                .map_err(internal_wire)?;
                let envelope = if params.cursor.is_none() {
                    Some(
                        serde_json::to_value(
                            query_lifecycle_log(
                                &history,
                                &witness,
                                &LifecycleLogFilter {
                                    task: params.task,
                                    attempt: params.attempt,
                                    session: params.session,
                                    event: params.event,
                                    source: params.source,
                                    since: params.since,
                                    until: params.until,
                                },
                            )
                            .map_err(observability_wire)?,
                        )
                        .map_err(internal_wire)?,
                    )
                } else {
                    None
                };
                self.pages
                    .borrow_mut()
                    .page(
                        method,
                        &fingerprint,
                        params.limit,
                        params.cursor.as_deref(),
                        envelope,
                    )
                    .map_err(pagination_wire)
            }
            "query.proof" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    task: String,
                    #[serde(default)]
                    attempt: Option<u32>,
                }
                let params: Params = decode_params(params)?;
                if params.task.trim().is_empty() {
                    return Err(WireError::invalid("query proof task must not be empty"));
                }
                let (attestation_report, attestations) = tokio::task::spawn_blocking(move || {
                    read_verified_attestations(&attestations_path)
                })
                .await
                .map_err(|error| {
                    internal_wire(format!("attestation query worker failed: {error}"))
                })?
                .map_err(internal_wire)?;
                if !attestation_report.ok {
                    return Err(internal_wire(
                        "attestation verification failed during proof query",
                    ));
                }
                serde_json::to_value(
                    query_proof(
                        &params.task,
                        params.attempt,
                        &details,
                        &history,
                        &report,
                        &witness,
                        &attestations,
                    )
                    .map_err(observability_wire)?,
                )
                .map_err(internal_wire)
            }
            "query.trace" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    task: String,
                    #[serde(default)]
                    attempt: Option<u32>,
                    #[serde(default)]
                    limit: Option<usize>,
                    #[serde(default)]
                    cursor: Option<String>,
                }
                let params: Params = decode_params(params)?;
                if params.task.trim().is_empty() {
                    return Err(WireError::invalid("query trace task must not be empty"));
                }
                let fingerprint = serde_json::to_string(&json!({
                    "task": params.task.clone(),
                    "attempt": params.attempt,
                }))
                .map_err(internal_wire)?;
                let envelope = if params.cursor.is_none() {
                    let lanes = trace_lanes(&details, &live, &history);
                    let adapters = {
                        let context = self.context.read().await;
                        context.config.adapters.clone()
                    };
                    Some(
                        serde_json::to_value(
                            query_trace(
                                &params.task,
                                params.attempt,
                                &lanes,
                                &adapters,
                                &self.executor,
                                snapshot_metadata(&history, &witness),
                            )
                            .map_err(trace_wire)?,
                        )
                        .map_err(internal_wire)?,
                    )
                } else {
                    None
                };
                self.pages
                    .borrow_mut()
                    .page(
                        method,
                        &fingerprint,
                        params.limit,
                        params.cursor.as_deref(),
                        envelope,
                    )
                    .map_err(pagination_wire)
            }
            "query.producers" | "query.watch" => {
                unreachable!("early read-only query paths return before projection setup")
            }
            "query.render" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    #[serde(default)]
                    format: Option<String>,
                    #[serde(default)]
                    scope: RenderScope,
                }
                let params: Params = decode_params(params)?;
                if params
                    .format
                    .as_deref()
                    .is_some_and(|format| !matches!(format, "text" | "json"))
                {
                    return Err(WireError::invalid("format must be text or json"));
                }
                let mut view = query_render(params.scope, &rows, &journal, &witness);
                overlay_live_states(&mut view.jobs, &live_states);
                if params.format.as_deref() == Some("text") {
                    serde_json::to_string_pretty(&view)
                        .map(Value::String)
                        .map_err(internal_wire)
                } else {
                    serde_json::to_value(view).map_err(internal_wire)
                }
            }
            "query.standup" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    #[serde(default)]
                    since: Option<String>,
                    #[serde(default)]
                    source: Option<String>,
                }
                let params: Params = decode_params(params)?;
                let since_realtime_us = params
                    .since
                    .as_deref()
                    .map(|since| {
                        chrono::DateTime::parse_from_rfc3339(since)
                            .map_err(|_| {
                                WireError::invalid(format!("invalid since timestamp {since:?}"))
                            })
                            .and_then(|timestamp| {
                                u64::try_from(timestamp.timestamp_micros()).map_err(|_| {
                                    WireError::invalid("since timestamp predates the Unix epoch")
                                })
                            })
                    })
                    .transpose()?;
                let until = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
                let mut digest = query_standup(
                    &rows,
                    &journal,
                    &witness,
                    &StandupOptions {
                        since: params.since,
                        since_realtime_us,
                        until,
                        source: params.source,
                    },
                );
                for entry in &mut digest.in_flight {
                    if let Some(state) = entry
                        .task_uuid
                        .as_ref()
                        .and_then(|task_uuid| live_states.get(task_uuid))
                    {
                        entry.state.clone_from(state);
                    }
                }
                serde_json::to_value(digest).map_err(internal_wire)
            }
            "query.pools" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {}
                let _: Params = decode_params(params)?;
                let pools = {
                    let mut context = self.context.write().await;
                    pool_headroom_facts(&mut context)?
                };
                serde_json::to_value(query_pools(&pools).map_err(query_wire)?)
                    .map_err(internal_wire)
            }
            _ => unreachable!("query methods are filtered by the RPC dispatcher"),
        }
    }

    fn spawn_execution(&self, job: Job) {
        if job.labor_class == LaborClass::Recovered {
            self.emit_post_ack(execution_event(&job, TallyEvent::Resumed));
        }
        self.emit_post_ack(execution_event(&job, TallyEvent::Dispatched));
        self.emit_post_ack(execution_event(&job, TallyEvent::Started));
        let executor = self.executor.clone();
        let completion = self.completion.clone();
        let request = execution_request(
            &executor,
            &job,
            self.settings.unit_limits,
            &self.tally_socket,
            &self.brief_root,
        );
        let execution_target = job.row.executor.clone();
        let evidence = job.row.evidence.clone();
        let mut shutdown = self.execution_shutdown.clone();
        let mut cancellation = self.execution_cancel.subscribe();
        tokio::task::spawn_local(async move {
            let started = Instant::now();
            let execution = async {
                let request = request?;
                if job.adopted {
                    executor
                        .adopt_on(
                            execution_target.as_deref(),
                            request,
                            job.adopted_invocation_id
                                .as_deref()
                                .expect("adopted recovery jobs retain their invocation ID"),
                            evidence,
                        )
                        .await
                } else {
                    executor
                        .execute_on(execution_target.as_deref(), request, evidence)
                        .await
                }
            };
            tokio::pin!(execution);
            let outcome = tokio::select! {
                outcome = &mut execution => Some(outcome),
                () = wait_for_cancellation(&mut cancellation, job.job_id) => None,
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                    Some(execution.await)
                }
            };
            let _ = completion.send(ExecutionFinished {
                job_id: job.job_id,
                attempt: job.row.attempt,
                lease_epoch: job.row.lease_epoch,
                elapsed: started.elapsed(),
                outcome,
            });
        });
    }

    fn emit_post_ack(&self, event: EmitEvent) {
        let fields = match event.into_fields() {
            Ok(fields) => fields,
            Err(error) => {
                let error =
                    DaemonError::Invalid(format!("invalid lifecycle event after ack: {error}"));
                eprintln!("tally: {error}");
                let _ = self.fatal.send(error);
                return;
            }
        };
        let lifecycle = match self.history.borrow_mut().append_now(fields.clone()) {
            Ok(record) => record,
            Err(error) => {
                eprintln!("tally: lifecycle history append failed after ack: {error}");
                let _ = self.fatal.send(error.into());
                return;
            }
        };
        let change_payload = json!({
            "taskUuid": fields.task_uuid,
            "attempt": fields.attempt,
            "leaseEpoch": fields.lease_epoch,
            "event": fields.event,
            "lifecycleCursor": lifecycle.cursor,
        });
        let mut changes = self.changes.borrow_mut();
        let mut append_change = |kind, payload| changes.append_now(kind, payload);
        let changed = append_change(ChangeKind::Lifecycle, change_payload.clone())
            .and_then(|_| {
                append_change(
                    ChangeKind::Job,
                    json!({
                        "taskUuid": fields.task_uuid,
                        "attempt": fields.attempt,
                        "leaseEpoch": fields.lease_epoch,
                        "reason": fields.event,
                    }),
                )
            })
            .and_then(|_| {
                if matches!(
                    fields.event,
                    TallyEvent::Completed
                        | TallyEvent::Failed
                        | TallyEvent::Preempted
                        | TallyEvent::WitnessEmitted
                ) {
                    append_change(ChangeKind::Proof, change_payload.clone())?;
                }
                Ok(())
            })
            .and_then(|_| {
                if fields
                    .agent
                    .as_ref()
                    .is_some_and(|adapter| self.trace_adapters.contains(adapter))
                    && matches!(
                        fields.event,
                        TallyEvent::Started
                            | TallyEvent::Completed
                            | TallyEvent::Failed
                            | TallyEvent::Preempted
                    )
                {
                    append_change(ChangeKind::Trace, change_payload)?;
                }
                Ok(())
            });
        drop(changes);
        if let Err(error) = changed {
            eprintln!("tally: change log append failed after ack: {error}");
            let _ = self.fatal.send(error.into());
            return;
        }
        let journal = self.journal.clone();
        tokio::task::spawn_local(async move {
            tokio::task::yield_now().await;
            if let Err(error) = journal.emit_fields(&fields) {
                eprintln!("tally: journald emission failed outside ack barrier: {error}");
            }
        });
    }
}

fn decode_params<T: for<'de> Deserialize<'de>>(params: Option<Value>) -> Result<T, WireError> {
    serde_json::from_value(params.unwrap_or_else(|| json!({})))
        .map_err(|error| WireError::invalid(error.to_string()))
}

fn internal_wire(error: impl ToString) -> WireError {
    WireError::new(WireErrorCode::Internal, error.to_string())
}

fn ensure_guardrail_parent(
    context: &mut Context,
    presented: &str,
    allow_terminal: bool,
) -> Result<(), WireError> {
    if let Some(info) = context.guardrails.parent(presented) {
        if info.terminal && !allow_terminal {
            return Err(WireError::not_found(format!(
                "parent job {presented} is terminal"
            )));
        }
        return Ok(());
    }
    let job_id = context
        .aliases
        .get(presented)
        .copied()
        .or_else(|| Uuid::parse_str(presented).ok())
        .filter(|uuid| context.jobs.contains_key(uuid) || context.rows.contains_key(uuid))
        .ok_or_else(|| WireError::not_found(format!("unknown parent job {presented}")))?;
    let active = context
        .jobs
        .get(&job_id)
        .is_some_and(|job| job.state != JobState::Completed);
    if !active && !allow_terminal {
        return Err(WireError::not_found(format!(
            "parent job {presented} is terminal"
        )));
    }
    let row = context
        .jobs
        .get(&job_id)
        .map(|job| &job.row)
        .or_else(|| context.rows.get(&job_id))
        .ok_or_else(|| WireError::not_found(format!("unknown parent job {presented}")))?;
    let stable = row.uuid.to_string();
    let outstanding = context
        .jobs
        .values()
        .filter(|job| job.state != JobState::Completed && job.row.parent_uuid == Some(row.uuid))
        .count();
    let outstanding = u32::try_from(outstanding)
        .map_err(|_| internal_wire("parent outstanding child count overflow"))?;
    let info = ParentInfo {
        parent_uuid: stable.clone(),
        depth: context
            .guardrail_depths
            .get(&row.uuid)
            .copied()
            .unwrap_or(0),
        outstanding,
        no_enqueue: row.no_enqueue,
        terminal: !active,
    };
    context
        .guardrails
        .register_parent(stable.clone(), info.clone());
    if presented != stable {
        context
            .guardrails
            .register_parent(presented.to_owned(), info);
    }
    Ok(())
}

fn enforce_flow_node_cap(context: &Context, row: &RowSeed) -> Result<(), WireError> {
    let Some(orchestration) = &row.orchestration else {
        return Ok(());
    };
    let flow_run_id = orchestration.flow_run_id();
    let existing_nodes = context
        .rows
        .values()
        .filter(|existing| {
            existing
                .orchestration
                .as_ref()
                .is_some_and(|capsule| capsule.flow_run_id() == flow_run_id)
        })
        .count();
    let existing_nodes =
        u64::try_from(existing_nodes).map_err(|_| internal_wire("flow node count overflow"))?;
    let max_nodes = orchestration.max_nodes().unwrap_or(DEFAULT_FLOW_MAX_NODES);
    if existing_nodes >= max_nodes {
        return Err(WireError {
            code: WireErrorCode::FlowNodeCap,
            message: format!(
                "flow run {flow_run_id} already has {existing_nodes} nodes; maxNodes is {max_nodes}"
            ),
            data: Some(json!({
                "flowRunId": flow_run_id,
                "maxNodes": max_nodes,
                "existingNodes": existing_nodes,
            })),
        });
    }
    Ok(())
}

fn store_admitted_brief(
    paths: &DaemonPaths,
    row: &RowSeed,
    prepared: Option<&PreparedBrief>,
) -> Result<(), WireError> {
    match (row.brief_hash.as_deref(), prepared) {
        (None, None) => Ok(()),
        (Some(expected), Some(prepared)) if prepared.hash() == expected => {
            let stored = brief::store(&paths.data_dir, prepared).map_err(internal_wire)?;
            let expected_path = paths.brief_path(expected).map_err(internal_wire)?;
            if stored != expected_path {
                return Err(internal_wire(
                    "content-addressed brief store returned an unexpected path",
                ));
            }
            Ok(())
        }
        _ => Err(internal_wire(
            "prepared brief and durable row briefHash disagree",
        )),
    }
}

fn rollback_child_charge(
    context: &mut Context,
    caller_job_id: Option<&str>,
    charged: bool,
) -> Result<(), WireError> {
    if charged {
        let caller_job_id = caller_job_id.ok_or_else(|| {
            WireError::new(
                WireErrorCode::Internal,
                "child charge is set without a caller job",
            )
        })?;
        context.guardrails.rollback_child_charge(caller_job_id)?;
    }
    Ok(())
}

fn lease_wire(error: LeaseError) -> WireError {
    match error {
        LeaseError::UnknownPool(_)
        | LeaseError::InvalidRequest(_)
        | LeaseError::StaleEpoch { .. } => WireError::invalid(error.to_string()),
        LeaseError::NotFound(_) => WireError::not_found(error.to_string()),
        other => internal_wire(other),
    }
}

fn query_wire(error: crate::query::QueryError) -> WireError {
    match error {
        crate::query::QueryError::UnknownPool(_) => WireError::not_found(error.to_string()),
        crate::query::QueryError::InvalidPool(_)
        | crate::query::QueryError::InvalidTimestamp(_) => WireError::invalid(error.to_string()),
    }
}

fn observability_wire(error: ObservabilityError) -> WireError {
    match error {
        ObservabilityError::InvalidTimestamp(_) => WireError::invalid(error.to_string()),
        ObservabilityError::UnknownJob(_) | ObservabilityError::UnknownAttempt { .. } => {
            WireError::not_found(error.to_string())
        }
    }
}

fn pagination_wire(error: PaginationError) -> WireError {
    match error {
        PaginationError::InvalidLimit
        | PaginationError::InvalidCursor
        | PaginationError::CursorMismatch => WireError::invalid(error.to_string()),
        PaginationError::CursorExpired => WireError::not_found(error.to_string()),
        PaginationError::InvalidEnvelope | PaginationError::ItemTooLarge => internal_wire(error),
    }
}

fn trace_wire(error: TraceError) -> WireError {
    match error {
        TraceError::UnknownJob(_) | TraceError::UnknownAttempt { .. } => {
            WireError::not_found(error.to_string())
        }
        TraceError::Io { .. } => internal_wire(error),
    }
}

fn change_wire(error: ChangeError) -> WireError {
    match error {
        ChangeError::Invalid(_) => WireError::invalid(error.to_string()),
        ChangeError::Io { .. } | ChangeError::Json(_) => internal_wire(error),
    }
}

fn trace_lanes(
    details: &[RowDetailFact],
    live: &[LiveJobFact],
    history: &crate::history::LifecycleSnapshot,
) -> Vec<TraceLane> {
    let mut lanes = BTreeMap::<(String, u32, u64), TraceLane>::new();
    for detail in details {
        lanes.insert(
            (detail.task_uuid.clone(), detail.attempt, detail.lease_epoch),
            TraceLane {
                task_uuid: detail.task_uuid.clone(),
                job_id: None,
                attempt: detail.attempt,
                lease_epoch: detail.lease_epoch,
                adapter: detail.adapter.clone(),
                session_ref: detail.session_ref.clone(),
                running: false,
                remote: detail.executor.is_some(),
            },
        );
    }
    for record in &history.records {
        let (Some(attempt), Some(lease_epoch)) = (record.fields.attempt, record.fields.lease_epoch)
        else {
            continue;
        };
        let key = (record.fields.task_uuid.clone(), attempt, lease_epoch);
        let lane = lanes.entry(key).or_insert_with(|| TraceLane {
            task_uuid: record.fields.task_uuid.clone(),
            job_id: record.fields.job_id.clone(),
            attempt,
            lease_epoch,
            adapter: record
                .fields
                .agent
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
            session_ref: record.fields.session_ref.clone(),
            running: false,
            remote: record.fields.executor.is_some(),
        });
        if record.fields.job_id.is_some() {
            lane.job_id.clone_from(&record.fields.job_id);
        }
        if record.fields.agent.is_some() {
            lane.adapter = record.fields.agent.clone().unwrap();
        }
        if record.fields.session_ref.is_some() {
            lane.session_ref.clone_from(&record.fields.session_ref);
        }
        lane.remote |= record.fields.executor.is_some();
    }
    for live in live {
        let key = (live.anchor.clone(), live.attempt, live.lease_epoch);
        if let Some(lane) = lanes.get_mut(&key) {
            lane.running = live.live_state == "running";
            lane.job_id = Some(live.job_id.clone());
        }
    }
    lanes.into_values().collect()
}

fn pool_headroom_facts(context: &mut Context) -> Result<Vec<PoolHeadroomFact>, WireError> {
    let now = Utc::now();
    let pools = context.config.pools.clone();
    let unleased_by_pool = context
        .jobs
        .values()
        .filter(|job| job.state == JobState::Queued && job.lease_id.is_none())
        .fold(HashMap::<String, usize>::new(), |mut counts, job| {
            for pool in &job.row.pools {
                *counts.entry(pool.clone()).or_default() += 1;
            }
            counts
        });
    pools
        .into_iter()
        .map(|(name, pool)| {
            let held = context
                .lease
                .engine()
                .held_in_pool(&name)
                .map_err(lease_wire)?;
            let queued = context
                .lease
                .engine()
                .queued_in_pool(&name)
                .map_err(lease_wire)?
                + unleased_by_pool.get(&name).copied().unwrap_or(0);
            let consumption = match pool.predicate {
                PoolPredicate::CoResidency(_) => None,
                PoolPredicate::WindowedConsumption(ref window) => {
                    let used = context
                        .lease
                        .engine_mut()
                        .budget_used_at(&name, now)
                        .map_err(lease_wire)?;
                    let reset_at = context
                        .lease
                        .engine_mut()
                        .window_reset_at(&name, now)
                        .map_err(lease_wire)?;
                    Some(WindowConsumptionFact {
                        used,
                        cap: window.consumption_cap,
                        reset_at,
                    })
                }
            };
            let meter = match (&pool.usage_meter, &pool.predicate) {
                (Some(meter), _) => read_usage_meter(
                    &context.paths.state_dir,
                    &name,
                    meter.poll_interval_sec.saturating_mul(2),
                    now,
                ),
                (None, PoolPredicate::WindowedConsumption(window))
                    if pool.resource == crate::config::ResourceKind::Budget =>
                {
                    read_usage_meter(&context.paths.state_dir, &name, window.window_sec, now)
                }
                _ => None,
            };
            Ok(PoolHeadroomFact {
                pool: name,
                capacity: u64::from(pool.capacity),
                held: u64::try_from(held).unwrap_or(u64::MAX),
                queued,
                consumption,
                meter_utilization_pct: meter.as_ref().map(|meter| meter.utilization_pct),
                weekly_utilization_pct: meter.and_then(|meter| meter.weekly_utilization_pct),
            })
        })
        .collect()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageMeterObservation {
    pool: String,
    budget_class: crate::config::MeterBudgetClass,
    utilization_pct: f64,
    #[serde(default)]
    weekly_utilization_pct: Option<f64>,
    reset_at: String,
    observed_at: String,
}

fn usage_meter_event_path(state_dir: &Path, pool: &str) -> PathBuf {
    let digest = Sha256::digest(pool.as_bytes());
    state_dir.join("meters").join(format!("{digest:x}.json"))
}

fn read_usage_meter(
    state_dir: &Path,
    pool: &str,
    freshness_sec: u64,
    now: chrono::DateTime<Utc>,
) -> Option<UsageMeterObservation> {
    let path = usage_meter_event_path(state_dir, pool);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_METER_EVENT_BYTES {
        return None;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .ok()?;
    let opened = file.metadata().ok()?;
    if !opened.file_type().is_file() || opened.len() > MAX_METER_EVENT_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_METER_EVENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_METER_EVENT_BYTES {
        return None;
    }
    let event: UsageMeterObservation = serde_json::from_slice(&bytes).ok()?;
    if event.pool != pool
        || event.budget_class != crate::config::MeterBudgetClass::Programmatic
        || !event.utilization_pct.is_finite()
        || !(0.0..=100.0).contains(&event.utilization_pct)
        || event
            .weekly_utilization_pct
            .is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value))
    {
        return None;
    }
    let observed_at = chrono::DateTime::parse_from_rfc3339(&event.observed_at)
        .ok()?
        .with_timezone(&Utc);
    let reset_at = chrono::DateTime::parse_from_rfc3339(&event.reset_at)
        .ok()?
        .with_timezone(&Utc);
    let freshness_sec = i64::try_from(freshness_sec).ok()?;
    if observed_at > now
        || now.signed_duration_since(observed_at) > chrono::Duration::seconds(freshness_sec)
        || reset_at <= now
        || reset_at < observed_at
    {
        return None;
    }
    Some(event)
}

fn feed_scraped_usage(
    state_dir: &Path,
    pools: &BTreeMap<String, crate::config::PoolConfig>,
    leased_pools: &[String],
    captures: &ScrapeResult,
) -> Vec<String> {
    let Some(amount) = scraped_token_amount(captures) else {
        return Vec::new();
    };
    let observed_at = Utc::now();
    leased_pools
        .iter()
        .filter_map(|name| {
            let pool = pools.get(name)?;
            let PoolPredicate::WindowedConsumption(window) = &pool.predicate else {
                return None;
            };
            if pool.resource != crate::config::ResourceKind::Budget || pool.usage_meter.is_some() {
                return None;
            }
            let reset_at = observed_at.checked_add_signed(chrono::Duration::seconds(
                i64::try_from(window.window_sec).ok()?,
            ))?;
            let event = UsageMeterObservation {
                pool: name.clone(),
                budget_class: crate::config::MeterBudgetClass::Programmatic,
                utilization_pct: ((amount as f64 / window.consumption_cap as f64) * 100.0)
                    .min(100.0),
                weekly_utilization_pct: None,
                reset_at: reset_at.to_rfc3339_opts(SecondsFormat::Millis, true),
                observed_at: observed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            };
            write_usage_meter(state_dir, &event)
                .err()
                .map(|error| format!("pool {name:?}: {error}"))
        })
        .collect()
}

fn scraped_token_amount(captures: &ScrapeResult) -> Option<u64> {
    let usage = captures.captures.get("usage")?.as_object()?;
    let amount = if let Some(total) = usage.get("total_tokens") {
        total.as_u64()?
    } else {
        let input = match usage.get("input_tokens") {
            Some(value) => value.as_u64()?,
            None => 0,
        };
        let output = match usage.get("output_tokens") {
            Some(value) => value.as_u64()?,
            None => 0,
        };
        input.checked_add(output)?
    };
    (amount > 0).then_some(amount)
}

fn write_usage_meter(state_dir: &Path, event: &UsageMeterObservation) -> io::Result<()> {
    let directory = state_dir.join("meters");
    std::fs::create_dir_all(&directory)?;
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "meter directory is not a regular directory",
        ));
    }
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let path = usage_meter_event_path(state_dir, &event.pool);
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, event).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        File::open(&directory)?.sync_all()
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

async fn await_registration(registration: WaitRegistration) -> Result<Value, WireError> {
    match registration {
        WaitRegistration::Ready(value) => Ok(value),
        WaitRegistration::Pending(receiver) => receiver
            .await
            .map_err(|_| internal_wire("daemon stopped while waiting")),
    }
}

fn parse_job_barrier(barrier: &str) -> Result<(&str, u32), WireError> {
    let body = barrier
        .strip_prefix("barrier:")
        .ok_or_else(|| WireError::not_found(format!("unknown barrier {barrier}")))?;
    let (stable, attempt) = body
        .rsplit_once(':')
        .ok_or_else(|| WireError::not_found(format!("unknown barrier {barrier}")))?;
    if stable.is_empty() || stable.starts_with("drain:") {
        return Err(WireError::not_found(format!("unknown barrier {barrier}")));
    }
    let attempt = attempt
        .parse::<u32>()
        .ok()
        .filter(|attempt| *attempt > 0)
        .ok_or_else(|| WireError::not_found(format!("unknown barrier {barrier}")))?;
    Ok((stable, attempt))
}

fn reconstruct_job_result(
    path: &Path,
    stable: &str,
    attempt: Option<u32>,
) -> Result<Value, WireError> {
    let (report, records) = read_verified_records(path).map_err(internal_wire)?;
    if !report.ok {
        return Err(internal_wire(
            "witness verification failed while reconstructing a completed wait",
        ));
    }
    let record = records
        .into_iter()
        .filter(|record| record.task_uuid.as_deref() == Some(stable))
        .filter(|record| attempt.is_none_or(|attempt| record.attempt == attempt))
        .max_by_key(|record| record.seq)
        .ok_or_else(|| {
            let suffix = attempt.map_or_else(String::new, |attempt| format!(" attempt {attempt}"));
            WireError::not_found(format!("job {stable}{suffix} has no terminal witness"))
        })?;
    Ok(job_result_from_witness(&record).value())
}

fn job_result_from_witness(record: &WitnessRecord) -> JobResult {
    let stable = record
        .task_uuid
        .clone()
        .expect("durable wait reconstruction selected a task witness");
    JobResult {
        task_uuid: Some(stable.clone()),
        job_id: stable,
        verdict: record.verdict,
        exit_code: record.exit_code,
        artifact_content_hash: record.artifact_content_hash.clone(),
        attempt: record.attempt,
        lease_epoch: record.lease_epoch,
        witness_seq: record.seq,
        model: record.model.clone(),
        completion: record.completion.clone(),
    }
}

fn single_job_barrier_value(barrier: &str, stable: &str, result: Value) -> Value {
    barrier_value(barrier, &BTreeMap::from([(stable.to_owned(), result)]))
}

async fn wait_for_cancellation(receiver: &mut broadcast::Receiver<Uuid>, job_id: Uuid) {
    loop {
        match receiver.recv().await {
            Ok(cancelled) if cancelled == job_id => return,
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => {
                std::future::pending::<()>().await;
            }
        }
    }
}

fn selected_pools(
    config: &Config,
    pool: Option<String>,
    all: bool,
) -> Result<Vec<String>, WireError> {
    if all == pool.is_some() {
        return Err(WireError::invalid(
            "provide exactly one of pool or all=true",
        ));
    }
    if all {
        return Ok(config.pools.keys().cloned().collect());
    }
    let pool = pool.expect("checked above");
    if !config.pools.contains_key(&pool) {
        return Err(WireError::invalid(format!("unknown pool {pool:?}")));
    }
    Ok(vec![pool])
}

fn find_job<'a>(context: &'a Context, presented: &str) -> Result<&'a Job, WireError> {
    context
        .aliases
        .get(presented)
        .and_then(|job_id| context.jobs.get(job_id))
        .ok_or_else(|| WireError::not_found(format!("job {presented} was not found")))
}

fn lease_request(job: &Job, unit: String) -> LeaseRequest {
    let scheduling_group = if let Some(orchestration) = &job.row.orchestration {
        LeaseSchedulingGroup::Flow(orchestration.flow_run_id().to_owned())
    } else if let Some(parent) = job.row.parent_uuid {
        LeaseSchedulingGroup::Parent(parent.to_string())
    } else {
        LeaseSchedulingGroup::Standalone
    };
    LeaseRequest {
        job_id: job.job_id.to_string(),
        unit,
        pools: job.row.pools.clone(),
        priority: job.row.priority,
        admission_key: Some(format!("{}:{}", job.stable_key(), job.row.attempt)),
        consumption_estimate: job.row.consumption_estimate,
        scheduling_group,
    }
}

fn execution_request(
    executor: &Executor,
    job: &Job,
    limits: UnitLimits,
    tally_socket: &str,
    brief_root: &Path,
) -> Result<ExecutionRequest, ExecutorError> {
    let brief_path = job.row.brief_hash.as_deref().map(|hash| {
        brief::content_path(brief_root, hash)
            .expect("validated durable briefHash always derives a content path")
    });
    let gate_manifest = effective_gate_manifest(executor, job)?;
    Ok(ExecutionRequest {
        identity: job.identity(),
        parent: job.row.parent_uuid,
        pools: job.row.pools.clone(),
        lease_epoch: job.row.lease_epoch,
        attempt: job.row.attempt,
        priority: job.row.priority,
        no_enqueue: job.row.no_enqueue,
        argv: job.invocation.argv.clone(),
        yield_hook: job.invocation.yield_hook.clone(),
        // A remote worker has no tally daemon and cannot use the coordinator's
        // Unix socket. The SSH transport itself never forwards ambient sockets.
        tally_socket: job.row.executor.is_none().then(|| tally_socket.to_owned()),
        environment: job.invocation.env.clone(),
        gh_origin: job.row.gh_origin.clone(),
        brief_hash: job.row.brief_hash.clone(),
        brief_path,
        brief_document: None,
        cwd: job.row.cwd.clone(),
        workspace: job.row.workspace.clone(),
        gate_manifest,
        hardening: job.invocation.hardening,
        credentials: job.row.credentials.clone(),
        limits,
        runtime_max_sec: job.row.runtime_max_sec,
    })
}

fn effective_gate_manifest(
    executor: &Executor,
    job: &Job,
) -> Result<Option<GateManifestSpec>, ExecutorError> {
    if let Some(spec) = &job.row.gate_manifest {
        return Ok(Some(spec.clone()));
    }
    provisions_gate_manifest(&job.row.adapter)
        .then(|| {
            executor.default_gate_manifest_on(
                job.row.executor.as_deref(),
                &job.identity(),
                job.row.attempt,
            )
        })
        .transpose()
}

fn execution_fact_for_termination(termination: &ExecutionTermination) -> ExecutionFact {
    match termination {
        ExecutionTermination::Exited(exit_code) => ExecutionFact::exited(*exit_code),
        ExecutionTermination::RuntimeExceeded => {
            ExecutionFact::failed("process exceeded RuntimeMaxSec")
        }
        ExecutionTermination::Signaled { code, status } => {
            ExecutionFact::failed(format!("process ended by {code} {status}"))
        }
        ExecutionTermination::ServiceFailed { service_result, .. } => {
            ExecutionFact::failed(format!("systemd service failed with {service_result}"))
        }
    }
}

fn state_name(state: JobState) -> &'static str {
    match state {
        JobState::Paused => "paused",
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Completed => "completed",
    }
}

struct DedupConflictCandidate {
    task_uuid: String,
    payload_hash: Option<String>,
    orchestration: Option<Orchestration>,
}

fn orchestration_node_label(orchestration: Option<&Orchestration>) -> Option<&str> {
    orchestration?
        .as_value()
        .get("nodeLabel")
        .and_then(Value::as_str)
}

fn dedup_conflict(
    dedup_key: &str,
    payload_hash: &str,
    mut existing: Vec<DedupConflictCandidate>,
) -> WireError {
    existing.sort_by(|left, right| left.task_uuid.cmp(&right.task_uuid));
    let existing_values = existing
        .iter()
        .map(|candidate| {
            let mut value = json!({
                "taskUuid": candidate.task_uuid,
                "payloadHash": candidate.payload_hash,
                "orchestration": candidate.orchestration,
            });
            if let Some(label) = orchestration_node_label(candidate.orchestration.as_ref()) {
                value["nodeLabel"] = Value::String(label.to_owned());
            }
            value
        })
        .collect::<Vec<_>>();
    let mut data = json!({
        "dedupKey": dedup_key,
        "payloadHash": payload_hash,
        "existing": existing_values,
        "liveTaskUuids": existing
            .iter()
            .map(|candidate| &candidate.task_uuid)
            .collect::<Vec<_>>(),
    });
    if let [candidate] = existing.as_slice() {
        data["existingTaskUuid"] = Value::String(candidate.task_uuid.clone());
        data["existingPayloadHash"] = candidate
            .payload_hash
            .as_ref()
            .map_or(Value::Null, |hash| Value::String(hash.clone()));
        data["existingOrchestration"] = candidate
            .orchestration
            .as_ref()
            .map_or(Value::Null, |orchestration| {
                orchestration.as_value().clone()
            });
        if let Some(label) = orchestration_node_label(candidate.orchestration.as_ref()) {
            data["existingLabel"] = Value::String(label.to_owned());
        }
    }
    WireError {
        code: WireErrorCode::DedupKeyConflict,
        message: format!("dedup-key-conflict for key {dedup_key:?}"),
        data: Some(data),
    }
}

fn full_live_disposition(
    context: &Context,
    dedup_key: &str,
    payload_hash: &str,
) -> Result<Option<Value>, WireError> {
    let live = context
        .jobs
        .values()
        .filter(|job| {
            job.state != JobState::Completed && job.row.dedup_key.as_deref() == Some(dedup_key)
        })
        .collect::<Vec<_>>();
    if live.is_empty() {
        return Ok(None);
    }
    if live.len() != 1 {
        return Err(dedup_conflict(
            dedup_key,
            payload_hash,
            live.into_iter()
                .map(|job| DedupConflictCandidate {
                    task_uuid: job.stable_key(),
                    payload_hash: job.row.payload_hash.as_ref().map(ToOwned::to_owned),
                    orchestration: job.row.orchestration.clone(),
                })
                .collect(),
        ));
    }
    let job = live[0];
    if job.row.payload_hash.as_deref() != Some(payload_hash) {
        return Err(dedup_conflict(
            dedup_key,
            payload_hash,
            vec![DedupConflictCandidate {
                task_uuid: job.stable_key(),
                payload_hash: job.row.payload_hash.clone(),
                orchestration: job.row.orchestration.clone(),
            }],
        ));
    }
    let task_uuid = job.stable_key();
    let state = state_name(job.state);
    let mut response = json!({
        "schemaVersion": 1,
        "disposition": "attached",
        "task_uuid": task_uuid,
        "taskUuid": task_uuid,
        "job_id": job.job_id.to_string(),
        "barrier": format!("barrier:{task_uuid}:{}", job.row.attempt),
        "state": state,
        "status": state,
        "dedup_key": dedup_key,
        "payloadHash": payload_hash,
        "attempt": job.row.attempt,
    });
    if let Some(label) = orchestration_node_label(job.row.orchestration.as_ref()) {
        response["recordedLabel"] = Value::String(label.to_owned());
    }
    if let Some(orchestration) = &job.row.orchestration {
        response["recordedOrchestration"] = orchestration.as_value().clone();
    }
    Ok(Some(response))
}

fn full_terminal_response(
    record: &WitnessRecord,
    payload_hash: &str,
    disposition: &str,
) -> Result<Value, WireError> {
    let task_uuid = record.task_uuid.clone().ok_or_else(|| {
        WireError::new(
            WireErrorCode::Internal,
            format!(
                "governing witness seq {} has no durable task UUID",
                record.seq
            ),
        )
    })?;
    let mut response = json!({
        "schemaVersion": 1,
        "disposition": disposition,
        "task_uuid": task_uuid,
        "taskUuid": task_uuid,
        "job_id": task_uuid,
        "barrier": format!("barrier:{task_uuid}:{}", record.attempt),
        "state": disposition,
        "status": disposition,
        "verdict": record.verdict,
        "exit_code": record.exit_code,
        "dedup_key": record.dedup_key,
        "artifact_content_hash": record.artifact_content_hash,
        "witness_lsn": record.seq,
        "witnessSeq": record.seq,
        "payloadHash": payload_hash,
        "attempt": record.attempt,
        "lease_epoch": record.lease_epoch,
    });
    if let Some(completion) = &record.completion {
        response["completion"] = serde_json::to_value(completion).map_err(internal_wire)?;
    }
    if let Some(label) = orchestration_node_label(record.orchestration.as_ref()) {
        response["recordedLabel"] = Value::String(label.to_owned());
    }
    if let Some(orchestration) = &record.orchestration {
        response["recordedOrchestration"] = orchestration.as_value().clone();
    }
    Ok(response)
}

fn overlay_live_states(jobs: &mut [JobProjection], live_states: &HashMap<String, String>) {
    for job in jobs {
        // A witness read happens after the live snapshot. If the job completed
        // in between, the newer terminal witness must win over stale live state.
        if job.witness_seq.is_none() {
            if let Some(state) = live_states.get(&job.anchor) {
                job.state.clone_from(state);
            }
        }
    }
}

fn enqueued_event(job: &Job) -> EmitEvent {
    let mut event = EmitEvent::enqueued(job.stable_key(), job.row.priority, job.row.source);
    event.agent = Some(job.row.adapter.clone());
    event.session_ref.clone_from(&job.row.session_ref);
    event.unit = Some(format!("tally-job-{}.service", job.stable_key()));
    event.attempt = Some(job.row.attempt);
    event.lease_epoch = Some(job.row.lease_epoch);
    event.labor_class = Some(job.labor_class);
    event.job_id = Some(job.job_id.to_string());
    event.parent = job.row.parent_uuid.map(|uuid| uuid.to_string());
    event.pools = Some(job.row.pools.clone());
    event.executor = job.row.executor.clone();
    event
}

fn execution_event(job: &Job, event: TallyEvent) -> EmitEvent {
    EmitEvent {
        event,
        task_uuid: job.stable_key(),
        class: job.row.priority,
        source: job.row.source,
        message: None,
        agent: Some(job.row.adapter.clone()),
        session_ref: job.row.session_ref.clone(),
        unit: Some(format!("tally-job-{}.service", job.stable_key())),
        exit_code: None,
        gpu_seconds: None,
        artifact_hash: None,
        evidence: None,
        attempt: Some(job.row.attempt),
        lease_epoch: Some(job.row.lease_epoch),
        labor_class: Some(job.labor_class),
        job_id: Some(job.job_id.to_string()),
        parent: job.row.parent_uuid.map(|uuid| uuid.to_string()),
        pools: Some(job.row.pools.clone()),
        executor: job.row.executor.clone(),
    }
}

fn canonical_job_model(job: &Job) -> Option<String> {
    job.row.adapter_options.model.clone().or_else(|| {
        if job.model_is_advisory {
            None
        } else {
            job.row.model.clone()
        }
    })
}

fn forced_witness(job: &Job, verdict: Verdict) -> WitnessBody {
    WitnessBody {
        task_uuid: job.task_uuid.map(|uuid| uuid.to_string()),
        transition_timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        verdict,
        exit_code: if verdict == Verdict::Cancelled { 0 } else { 1 },
        artifact_content_hash: None,
        gpu_seconds: None,
        wall_clock: 0.0,
        attempt: job.row.attempt,
        lease_epoch: job.row.lease_epoch,
        dedup_key: job.row.dedup_key.clone(),
        payload_hash: job.row.payload_hash.clone(),
        brief_hash: job.row.brief_hash.clone(),
        orchestration: job.row.orchestration.clone(),
        labor_class: job.labor_class,
        trace_ref: None,
        pools: Some(job.row.pools.clone()),
        executor: job.row.executor.clone(),
        charge: None,
        model: canonical_job_model(job),
        evidence_class: job.row.evidence_class.clone(),
        manifest_hash: job.row.manifest_hash.clone(),
        completion: None,
    }
}

fn release_child_charge(context: &mut Context, job: &Job) -> Result<(), DaemonError> {
    if context
        .guardrail_depths
        .get(&job.row.uuid)
        .is_some_and(|depth| *depth > 0)
    {
        if let Some(parent_uuid) = job.row.parent_uuid {
            context
                .guardrails
                .rollback_child_charge(&parent_uuid.to_string())
                .map_err(|error| DaemonError::Invalid(error.message))?;
        }
    }
    Ok(())
}

fn finalize_forced_locked(
    context: &mut Context,
    job_id: Uuid,
    verdict: Verdict,
    release_lease: bool,
    scrape_capture: bool,
) -> Result<Option<TerminalWork>, DaemonError> {
    let job = context
        .jobs
        .get(&job_id)
        .cloned()
        .ok_or_else(|| DaemonError::Invalid(format!("unknown forced-terminal job {job_id}")))?;
    if job.state == JobState::Completed {
        return Ok(None);
    }
    let record = context.witness.append(forced_witness(&job, verdict))?;
    let result = JobResult {
        task_uuid: job.task_uuid.map(|uuid| uuid.to_string()),
        job_id: job.job_id.to_string(),
        verdict,
        exit_code: if verdict == Verdict::Cancelled { 0 } else { 1 },
        artifact_content_hash: None,
        attempt: job.row.attempt,
        lease_epoch: job.row.lease_epoch,
        witness_seq: record.seq,
        model: record.model.clone(),
        completion: None,
    };
    context
        .barriers
        .complete_job(&job.stable_key(), result.value());
    let stored = context.jobs.get_mut(&job_id).expect("job exists");
    stored.state = JobState::Completed;
    if release_lease {
        stored.lease_id = None;
    }
    release_child_charge(context, &job)?;
    context.guardrails.retire_parent(&job.stable_key());
    if let Some(task_uuid) = job.task_uuid {
        if let Some(row) = context.query_rows.get_mut(&task_uuid) {
            row.status = if verdict == Verdict::Cancelled {
                RowStatus::Deleted
            } else {
                RowStatus::Completed
            };
        }
        if let Some(detail) = context.query_details.get_mut(&task_uuid) {
            detail.row_status = if verdict == Verdict::Cancelled {
                RowStatus::Deleted
            } else {
                RowStatus::Completed
            };
        }
    }

    let mut launches = Vec::new();
    if release_lease {
        if let Some(lease_id) = &job.lease_id {
            let epoch = context.epoch;
            let status = context.lease.engine().status(lease_id, epoch)?;
            if status.held {
                let released = context.lease.release(lease_id, epoch, Utc::now())?;
                launches.extend(promoted_jobs(context, released.promoted));
            } else {
                context
                    .lease
                    .engine_mut()
                    .cancel_pending_at(lease_id, epoch, Utc::now())?;
            }
            context.lease_jobs.remove(lease_id);
        }
    }
    let evidence = serde_json::to_string(&job.row.evidence)
        .map_err(|error| DaemonError::Invalid(error.to_string()))?;
    Ok(Some(TerminalWork {
        job,
        result,
        evidence,
        evidence_checks: Vec::new(),
        launches,
        scrape_capture,
    }))
}

fn evidence_event(job: &Job, check: &CheckOutcome) -> EmitEvent {
    EmitEvent {
        event: if check.passed {
            TallyEvent::EvidencePass
        } else {
            TallyEvent::EvidenceFail
        },
        task_uuid: job.stable_key(),
        class: job.row.priority,
        source: job.row.source,
        message: Some(check.reason.clone()),
        agent: Some(job.row.adapter.clone()),
        session_ref: job.row.session_ref.clone(),
        unit: Some(format!("tally-job-{}.service", job.stable_key())),
        exit_code: None,
        gpu_seconds: None,
        artifact_hash: None,
        evidence: Some(check.spec.clone()),
        attempt: Some(job.row.attempt),
        lease_epoch: Some(job.row.lease_epoch),
        labor_class: Some(job.labor_class),
        job_id: Some(job.job_id.to_string()),
        parent: job.row.parent_uuid.map(|uuid| uuid.to_string()),
        pools: Some(job.row.pools.clone()),
        executor: job.row.executor.clone(),
    }
}

fn completed_event(job: &Job, result: &JobResult, evidence: String) -> EmitEvent {
    EmitEvent {
        event: match (result.verdict, result.artifact_content_hash.is_some()) {
            (Verdict::Preempted, _) => TallyEvent::Preempted,
            (Verdict::Pass | Verdict::Reused, true) => TallyEvent::Completed,
            (Verdict::Pass | Verdict::Reused, false) => TallyEvent::WitnessEmitted,
            _ => TallyEvent::Failed,
        },
        task_uuid: job.stable_key(),
        class: job.row.priority,
        source: job.row.source,
        message: None,
        agent: Some(job.row.adapter.clone()),
        session_ref: job.row.session_ref.clone(),
        unit: Some(format!("tally-job-{}.service", job.stable_key())),
        exit_code: Some(result.exit_code),
        gpu_seconds: Some(0.0),
        artifact_hash: result.artifact_content_hash.clone(),
        evidence: Some(evidence),
        attempt: Some(result.attempt),
        lease_epoch: Some(result.lease_epoch),
        labor_class: Some(job.labor_class),
        job_id: Some(job.job_id.to_string()),
        parent: job.row.parent_uuid.map(|uuid| uuid.to_string()),
        pools: Some(job.row.pools.clone()),
        executor: job.row.executor.clone(),
    }
}

fn canonical_verdict(
    evidence_verdict: Verdict,
    completion: Option<&SemanticCompletion>,
) -> Verdict {
    if evidence_verdict == Verdict::Pass
        && completion.is_some_and(|completion| completion.gates.status == GateSummaryStatus::Fail)
    {
        Verdict::Failed
    } else {
        evidence_verdict
    }
}

#[cfg(test)]
#[derive(Clone)]
struct LeaseTickHook {
    started: mpsc::UnboundedSender<()>,
    release: watch::Receiver<bool>,
}

pub struct Daemon {
    _state_lock: File,
    listener: UnixListener,
    handler: DaemonHandler,
    completion_rx: mpsc::UnboundedReceiver<ExecutionFinished>,
    fatal_rx: mpsc::UnboundedReceiver<DaemonError>,
    commit_rx: Option<mpsc::UnboundedReceiver<CommitCommand>>,
    committer: Option<Box<dyn ReplicaCommitter>>,
    notifier: SystemdNotifier,
    initial_jobs: Vec<Job>,
    initial_gh_completions: Vec<GhTerminalWork>,
    initial_lost_pools: Vec<String>,
    execution_shutdown: watch::Sender<bool>,
    max_frame_bytes: u64,
    #[cfg(test)]
    lease_tick_hook: Option<LeaseTickHook>,
}

impl Daemon {
    pub async fn open(
        config: Config,
        paths: DaemonPaths,
        settings: DaemonSettings,
        recorder_program: PathBuf,
    ) -> Result<Self, DaemonError> {
        let executor = Executor::new(&paths.state_dir, recorder_program)
            .with_remote_executors(config.executors.clone())
            .require_systemd();
        Self::open_with_executor(config, paths, settings, executor).await
    }

    pub async fn open_with_executor(
        config: Config,
        paths: DaemonPaths,
        settings: DaemonSettings,
        executor: Executor,
    ) -> Result<Self, DaemonError> {
        config
            .validate()
            .map_err(|error| DaemonError::Invalid(error.to_string()))?;
        let executor = executor.with_remote_executors(config.executors.clone());
        let settings = settings.validate()?;
        prepare_paths(&paths)?;
        let state_lock = acquire_daemon_lock(&paths.state_dir)?;
        let witness_path = paths.witness_path();
        let mut witness_ledger = WitnessLedger::open(&witness_path)?;
        let epoch = bump_epoch(&paths.state_dir)?;
        reconcile_pool_loss_intents(&paths, &executor, &mut witness_ledger).await?;
        let mut durable = collect_durable_recovery_facts(&paths.events_dir(), &witness_path)?;
        if reconcile_reuse_witnesses(&durable, &mut witness_ledger)? {
            durable = collect_durable_recovery_facts(&paths.events_dir(), &witness_path)?;
        }
        drop(witness_ledger);
        let units = collect_local_unit_facts(&executor, &durable).await?;
        let producer_engine =
            ProducerEngine::new(&config.producers, paths.events_dir(), &paths.state_dir);
        let confirmed_pool_returns = producer_engine
            .confirmed_pool_returns()
            .map_err(|error| DaemonError::Invalid(error.to_string()))?
            .into_iter()
            .filter(|pool| {
                config
                    .pools
                    .get(pool)
                    .is_some_and(crate::config::PoolConfig::auto_resume_enabled)
            })
            .collect();
        let facts = RecoveryFacts {
            durable,
            current_lease_epoch: epoch,
            units,
            rowless_units: BTreeMap::new(),
            triggers: RecoveryTriggers {
                confirmed_pool_returns,
                resource_returns: BTreeSet::new(),
                bounded_requeues: BTreeSet::new(),
            },
            advisory_return_attestations: Vec::new(),
        };
        let mut startup_policy = settings.recovery_policy;
        startup_policy.retry.auto_pool_return = true;
        if let Err(error) = repair_attestation_tail(&paths.attestations_path()) {
            eprintln!("tally: advisory attestation tail could not be repaired: {error}");
        }
        let triggered_plan = recover(&facts, startup_policy)?;
        let selected = renderable_pool_return_rows(
            &triggered_plan,
            &config,
            &executor,
            &paths.attestations_path(),
        );
        let mut facts_without_pool_returns = facts.clone();
        facts_without_pool_returns
            .triggers
            .confirmed_pool_returns
            .clear();
        let base_plan = recover(&facts_without_pool_returns, startup_policy)?;
        let mut plan = merge_selected_pool_returns(base_plan, triggered_plan, &selected);
        reconcile_retained_adapter_attestations(
            &plan,
            facts.durable.witness(),
            &config,
            &executor,
            &paths.attestations_path(),
        );
        hydrate_completed_adapter_metadata(&mut plan, &config, &executor);
        hydrate_adopted_adapter_metadata(&mut plan, &paths.attestations_path())?;
        hydrate_represent_adapter_metadata(
            &mut plan,
            &config,
            &executor,
            &paths.attestations_path(),
        )?;

        let mut db = TaskDb::open(&paths.data_dir).await?;
        db.rebuild_from_recovery_plan(&plan).await?;
        let adapter_metadata = plan
            .rows
            .iter()
            .filter(|recovery| {
                recovery.row.session_ref.is_some()
                    || recovery.row.model.is_some()
                    || recovery.row.final_message.is_some()
            })
            .map(|recovery| {
                let status = match recovery.state {
                    RecoveryRowState::Pending => Status::Pending,
                    RecoveryRowState::Deleted => Status::Deleted,
                    RecoveryRowState::Completed
                    | RecoveryRowState::AdoptedRunning
                    | RecoveryRowState::AwaitingReconciliation => Status::Completed,
                };
                (
                    recovery.row.uuid,
                    (recovery.row.clone(), status, recovery.labor_class),
                )
            })
            .collect();
        let committer: Box<dyn ReplicaCommitter> = Box::new(TaskDbCommitter {
            db,
            events_dir: paths.events_dir(),
            witness_path: witness_path.clone(),
            adapter_metadata,
        });
        Self::build_locked(
            config, paths, settings, executor, epoch, plan, committer, state_lock,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    fn build(
        config: Config,
        paths: DaemonPaths,
        settings: DaemonSettings,
        executor: Executor,
        epoch: u64,
        plan: crate::recovery::RecoveryPlan,
        committer: Box<dyn ReplicaCommitter>,
    ) -> Result<Self, DaemonError> {
        let state_lock = acquire_daemon_lock(&paths.state_dir)?;
        Self::build_locked(
            config, paths, settings, executor, epoch, plan, committer, state_lock,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_locked(
        config: Config,
        paths: DaemonPaths,
        settings: DaemonSettings,
        executor: Executor,
        epoch: u64,
        plan: crate::recovery::RecoveryPlan,
        committer: Box<dyn ReplicaCommitter>,
        state_lock: File,
    ) -> Result<Self, DaemonError> {
        validate_recovery_briefs(&plan, &paths.data_dir)?;
        let event_log = LeaseEventLog::in_state_dir(&paths.state_dir);
        let lease_engine = LeaseEngine::from_durable_with_aging_threshold(
            epoch,
            settings.yield_grace,
            Duration::from_secs(config.aging_threshold_sec),
            config.pools.clone(),
            event_log,
            &paths.witness_path(),
            Utc::now(),
        )?;
        let completed_witness = facts_witness(&plan, &paths)?;
        let initial_gh_completions = recovery_gh_completions(&plan, &completed_witness)?;
        let initial_lost_pools =
            ProducerEngine::new(&config.producers, paths.events_dir(), &paths.state_dir)
                .confirmed_pool_losses()
                .map_err(|error| DaemonError::Invalid(error.to_string()))?
                .into_iter()
                .collect::<Vec<_>>();
        let query_rows = recovery_query_rows(&plan);
        let query_details = recovery_query_details(&plan);
        let rows = plan
            .rows
            .iter()
            .map(|recovery| (recovery.row.uuid, recovery.row.clone()))
            .collect();
        let guardrail_depths = plan
            .rows
            .iter()
            .map(|recovery| (recovery.row.uuid, recovery.guardrail_depth))
            .collect();
        let mut context = Context {
            config: config.clone(),
            paths: paths.clone(),
            epoch,
            lease: LocalLease::new(lease_engine, SystemdUnitLiveness::default()),
            guardrails: GuardrailState::new(GuardrailConfig {
                depth_cap: config.enqueue.depth_cap,
                fanout_cap: config.enqueue.fanout_cap,
                require_dedup_key: config.enqueue.require_dedup_key,
            })
            .map_err(|error| DaemonError::Invalid(error.message))?,
            witness: WitnessLedger::open(paths.witness_path())?,
            jobs: HashMap::new(),
            aliases: HashMap::new(),
            lease_jobs: HashMap::new(),
            paused_pools: HashSet::new(),
            unreachable_pools: initial_lost_pools.iter().cloned().collect(),
            unreachable_paused_jobs: HashSet::new(),
            applied_pool_transitions: HashSet::new(),
            barriers: BarrierTracker::with_namespace(epoch),
            rows,
            guardrail_depths,
            query_rows,
            query_details,
        };
        restore_completed_aliases(&mut context, &completed_witness)?;
        let initial_jobs = install_recovery_jobs(&mut context, &plan, &executor)?;
        restore_guardrail_parents(&mut context, &plan)?;

        let notifier = SystemdNotifier::from_environment()?;
        if paths.socket.exists() {
            std::fs::remove_file(&paths.socket)
                .map_err(|source| io_error(&paths.socket, source))?;
        }
        let listener =
            UnixListener::bind(&paths.socket).map_err(|source| io_error(&paths.socket, source))?;
        let (completion_tx, completion_rx) = mpsc::unbounded_channel();
        let (fatal_tx, fatal_rx) = mpsc::unbounded_channel();
        let (commit_tx, commit_rx) = mpsc::unbounded_channel();
        let (execution_shutdown, execution_shutdown_rx) = watch::channel(false);
        let (execution_cancel, _) = broadcast::channel(64);
        let post_ack_tasks = Rc::new(RefCell::new(Vec::new()));
        let pool_transition_tasks = Rc::new(RefCell::new(Vec::new()));
        let tally_socket = paths
            .socket
            .to_str()
            .ok_or_else(|| DaemonError::Invalid("daemon socket path must be Unicode".to_owned()))?
            .to_owned();
        let mut changes = ChangeStore::open(&paths.data_dir)?;
        for (name, producer) in &config.producers {
            changes.append_now(
                ChangeKind::Producer,
                json!({
                    "name": name,
                    "kind": producer.kind(),
                    "update": "effective-registry-loaded",
                }),
            )?;
        }
        for pool in config.pools.keys() {
            changes.append_now(
                ChangeKind::Pool,
                json!({"pool": pool, "update": "effective-registry-loaded"}),
            )?;
        }
        let trace_adapters = config
            .adapters
            .iter()
            .filter(|(_, adapter)| adapter.trace.is_some())
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        let handler = DaemonHandler {
            context: Rc::new(RwLock::new(context)),
            settings,
            executor,
            completion: completion_tx,
            commits: commit_tx,
            journal: JournalEmitter::from_config(&config.journald),
            history: Rc::new(RefCell::new(LifecycleStore::open(&paths.data_dir)?)),
            changes: Rc::new(RefCell::new(changes)),
            trace_adapters: Rc::new(trace_adapters),
            pages: Rc::new(RefCell::new(PageCache::default())),
            execution_shutdown: execution_shutdown_rx,
            execution_cancel,
            fatal: fatal_tx,
            post_ack_tasks,
            pool_transition_tasks,
            ingress_sweep: Rc::new(Mutex::new(())),
            pool_transition_sweep: Rc::new(Mutex::new(())),
            gh_program: PathBuf::from("gh"),
            tally_socket,
            brief_root: paths.data_dir.clone(),
        };
        Ok(Self {
            _state_lock: state_lock,
            listener,
            handler,
            completion_rx,
            fatal_rx,
            commit_rx: Some(commit_rx),
            committer: Some(committer),
            notifier,
            initial_jobs,
            initial_gh_completions,
            initial_lost_pools,
            execution_shutdown,
            max_frame_bytes: config.max_frame_bytes,
            #[cfg(test)]
            lease_tick_hook: None,
        })
    }

    pub async fn run(self) -> Result<(), DaemonError> {
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let local = LocalSet::new();
        local.run_until(self.run_loop(shutdown_rx)).await
    }

    pub async fn run_until(self, shutdown: watch::Receiver<bool>) -> Result<(), DaemonError> {
        self.run_loop(shutdown).await
    }

    async fn run_loop(mut self, mut shutdown: watch::Receiver<bool>) -> Result<(), DaemonError> {
        let (socket_path, state_lock_path) = {
            let context = self.handler.context.read().await;
            (
                context.paths.socket.clone(),
                context.paths.state_dir.join("daemon.lock"),
            )
        };
        let mut terminate =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    drop(self.listener);
                    let _ = std::fs::remove_file(&socket_path);
                    return Err(DaemonError::Notify(error.to_string()));
                }
            };
        let committer = self
            .committer
            .take()
            .ok_or(DaemonError::CommitWorkerStopped)?;
        let commit_rx = self
            .commit_rx
            .take()
            .ok_or(DaemonError::CommitWorkerStopped)?;
        let worker_lock = self
            ._state_lock
            .try_clone()
            .map_err(|error| DaemonError::Invalid(format!("cannot clone daemon lock: {error}")))?;
        let commit_worker = match spawn_commit_worker(committer, commit_rx, worker_lock) {
            Ok(worker) => worker,
            Err(error) => {
                drop(self.listener);
                let _ = std::fs::remove_file(&socket_path);
                return Err(error);
            }
        };
        let mut startup_error = None;
        for pool in std::mem::take(&mut self.initial_lost_pools) {
            if let Err(error) = self.handler.apply_pool_loss(&pool).await {
                startup_error = Some(DaemonError::Invalid(format!(
                    "cannot apply confirmed startup pool loss for {pool:?}: {}",
                    error.message
                )));
                break;
            }
        }
        for job in std::mem::take(&mut self.initial_jobs) {
            let running = self
                .handler
                .context
                .read()
                .await
                .jobs
                .get(&job.job_id)
                .is_some_and(|stored| stored.state == JobState::Running);
            if running {
                self.handler.spawn_execution(job);
            }
        }
        for completion in std::mem::take(&mut self.initial_gh_completions) {
            self.handler
                .complete_gh_post_ack(completion.row, completion.result);
        }
        let mut lease_tick = tokio::time::interval(LEASE_TICK);
        lease_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut watchdog = self.notifier.watchdog_interval();
        let mut lease_ticks = JoinSet::new();
        let mut connections = Vec::new();
        let mut result = if let Some(error) = startup_error {
            Err(error)
        } else {
            match self.notifier.ready() {
                Err(error) => Err(error),
                Ok(()) => loop {
                    tokio::select! {
                        accepted = self.listener.accept() => {
                            match accepted {
                                Ok((stream, _)) => {
                                    let handler = self.handler.clone();
                                    let max_frame_bytes = self.max_frame_bytes;
                                    connections.push(tokio::task::spawn_local(async move {
                                        if let Err(error) = serve_connection_with_max_frame_bytes(
                                            stream,
                                            handler,
                                            max_frame_bytes,
                                        )
                                        .await
                                        {
                                            eprintln!("tally: RPC connection failed: {error}");
                                        }
                                    }));
                                }
                                Err(source) => break Err(io_error(&socket_path, source)),
                            }
                        }
                        Some(finished) = self.completion_rx.recv() => {
                            if let Err(error) = self.finish_job(finished).await {
                                break Err(error);
                            }
                        }
                        Some(error) = self.fatal_rx.recv() => break Err(error),
                        _ = lease_tick.tick() => {
                            if lease_ticks.is_empty() {
                                let handler = self.handler.clone();
                                #[cfg(test)]
                                let hook = self.lease_tick_hook.clone();
                                lease_ticks.spawn_local(async move {
                                    #[cfg(test)]
                                    if let Some(mut hook) = hook {
                                        let _ = hook.started.send(());
                                        while !*hook.release.borrow() {
                                            if hook.release.changed().await.is_err() {
                                                break;
                                            }
                                        }
                                    }
                                    Self::tick_leases(handler).await
                                });
                            }
                        }
                        Some(tick_result) = lease_ticks.join_next(), if !lease_ticks.is_empty() => {
                            match tick_result {
                                Ok(Ok(())) => {}
                                Ok(Err(error)) => break Err(error),
                                Err(error) => break Err(DaemonError::Invalid(format!(
                                    "lease tick task failed: {error}"
                                ))),
                            }
                        }
                        _ = watchdog_tick(&mut watchdog), if watchdog.is_some() => {
                            if let Err(error) = self.notifier.watchdog() {
                                break Err(error);
                            }
                        }
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() {
                                break Ok(());
                            }
                        }
                        signal = tokio::signal::ctrl_c() => {
                            match signal {
                                Ok(()) => break Ok(()),
                                Err(error) => break Err(DaemonError::Notify(error.to_string())),
                            }
                        }
                        _ = terminate.recv() => break Ok(()),
                    }
                },
            }
        };
        if let Err(error) = self.notifier.stopping() {
            if result.is_ok() {
                result = Err(error);
            }
        }
        // STOPPING disables the service watchdog before this potentially slow
        // drain. A lease tick can cross physical reclaim and canonical witness
        // writes, so never abort or detach it: finish the transaction while this
        // daemon still owns the state lock, then include any failure in the result.
        while let Some(tick_result) = lease_ticks.join_next().await {
            let tick_result = match tick_result {
                Ok(result) => result,
                Err(error) => Err(DaemonError::Invalid(format!(
                    "lease tick task failed: {error}"
                ))),
            };
            if let Err(error) = tick_result {
                if result.is_ok() {
                    result = Err(error);
                }
            }
        }
        for connection in connections {
            connection.abort();
            let _ = connection.await;
        }
        // Pool-loss application crosses physical reclaim and canonical witness
        // writes. RPC connection cancellation must not detach or abort that
        // transaction; join it under the daemon's exclusive state lock.
        if let Err(error) = self.handler.drain_pool_transition_tasks().await {
            if result.is_ok() {
                result = Err(error);
            }
        }
        let _ = self.execution_shutdown.send(true);
        // Advisory scrape attestations are outside the terminal fsync barrier,
        // but they still belong to this daemon lifetime. Join them while the
        // state lock and replica writer are both exclusively owned.
        self.handler.drain_post_ack_tasks().await;
        let socket = self.handler.context.read().await.paths.socket.clone();
        drop(self.listener);
        match std::fs::remove_file(&socket) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                if result.is_ok() {
                    result = Err(io_error(&socket, source));
                }
            }
        }
        commit_worker.stopping.store(true, Ordering::Release);
        let commit_shutdown = self
            .handler
            .commits
            .send(CommitCommand::Shutdown)
            .map_err(|_| DaemonError::CommitWorkerStopped);
        // Once the socket and watchdog are down, shutdown may wait for a stuck
        // post-ack commit, but it must never detach a live SQLite writer.
        let commit_join = tokio::task::spawn_blocking(move || commit_worker.thread.join())
            .await
            .map_err(|error| {
                DaemonError::Invalid(format!("replica worker join task failed: {error}"))
            })?
            .map_err(|_| DaemonError::Invalid("replica worker panicked".to_owned()));
        if let Err(error) = commit_shutdown {
            if result.is_ok() {
                result = Err(error);
            }
        }
        if let Err(error) = commit_join {
            if result.is_ok() {
                result = Err(error);
            }
        }
        // flock ownership follows the open-file description across fork. CLOEXEC
        // only closes an inherited descriptor at exec, so relying on last-close can
        // leave a clean shutdown briefly fenced by a concurrently spawned child.
        // Explicitly unlock after every lock-protected task and writer has joined.
        if let Err(source) = FileExt::unlock(&self._state_lock) {
            if result.is_ok() {
                result = Err(io_error(&state_lock_path, source));
            }
        }
        result
    }

    async fn finish_job(&self, finished: ExecutionFinished) -> Result<(), DaemonError> {
        let job = {
            let context = self.handler.context.read().await;
            let job = context.jobs.get(&finished.job_id).cloned().ok_or_else(|| {
                DaemonError::Invalid(format!("unknown completed job {}", finished.job_id))
            })?;
            if job.state == JobState::Completed
                || job.row.attempt != finished.attempt
                || job.row.lease_epoch != finished.lease_epoch
            {
                return Ok(());
            }
            job
        };
        let evidence_spec = parse_evidence_specs(&job.row.evidence)
            .map_err(|error| DaemonError::Invalid(error.to_string()))?;
        let scrape_capture = matches!(
            &finished.outcome,
            Some(Ok(outcome)) if outcome.captures_available
        );
        let effective_gate_manifest = effective_gate_manifest(&self.handler.executor, &job)?;
        let semantic_completion = match (&effective_gate_manifest, &finished.outcome) {
            (None, Some(Ok(outcome))) if outcome.semantic_completion.is_some() => {
                return Err(DaemonError::Invalid(format!(
                    "job {} returned semantic completion without a declared gate manifest",
                    job.stable_key()
                )))
            }
            (None, _) => None,
            (Some(spec), Some(Ok(outcome))) => {
                if let Some(completion) = &outcome.semantic_completion {
                    Some(completion.clone())
                } else {
                    if job.row.executor.is_some() {
                        return Err(DaemonError::Invalid(format!(
                            "remote job {} omitted its gate-manifest result",
                            job.stable_key()
                        )));
                    }
                    let execution = execution_fact_for_termination(&outcome.termination);
                    let spec = spec.clone();
                    Some(
                        tokio::task::spawn_blocking(move || evaluate_completion(execution, &spec))
                            .await
                            .map_err(|error| {
                                DaemonError::Invalid(format!(
                                    "gate manifest worker failed: {error}"
                                ))
                            })?,
                    )
                }
            }
            (Some(spec), Some(Err(error))) => {
                let execution = ExecutionFact::failed(format!("executor failed: {error}"));
                let spec = spec.clone();
                Some(
                    tokio::task::spawn_blocking(move || evaluate_completion(execution, &spec))
                        .await
                        .map_err(|error| {
                            DaemonError::Invalid(format!("gate manifest worker failed: {error}"))
                        })?,
                )
            }
            (Some(_), None) => None,
        };
        let (evidence_verdict, exit_code, artifact_hash, evidence_checks) = match finished.outcome {
            None => {
                return Err(DaemonError::Invalid(format!(
                    "job {} stopped without a terminal witness",
                    job.stable_key()
                )))
            }
            Some(Ok(outcome)) => match outcome.termination {
                ExecutionTermination::RuntimeExceeded => {
                    (Verdict::RuntimeExceeded, 1, None, Vec::new())
                }
                ExecutionTermination::Exited(code) => {
                    let gate = if let Some(gate) = outcome.evidence_gate {
                        gate
                    } else {
                        let elapsed = finished.elapsed;
                        tokio::task::spawn_blocking(move || {
                            run_evidence_gate(RunOutcome {
                                exit_code: code,
                                wall_clock_seconds: elapsed.as_secs_f64(),
                                evidence: &evidence_spec,
                            })
                        })
                        .await
                        .map_err(|error| {
                            DaemonError::Invalid(format!("evidence worker failed: {error}"))
                        })?
                    };
                    (gate.verdict, code, gate.artifact_hash, gate.checks)
                }
                ExecutionTermination::Signaled { .. }
                | ExecutionTermination::ServiceFailed { .. } => {
                    (Verdict::Failed, 1, None, Vec::new())
                }
            },
            Some(Err(
                error @ (ExecutorError::UnitProbe { .. }
                | ExecutorError::UnitControl { .. }
                | ExecutorError::ExistingUnit { .. }
                | ExecutorError::IndeterminatePriorLaunch { .. }
                | ExecutorError::AdoptedUnitUnavailable { .. }
                | ExecutorError::AdoptedInvocationMismatch { .. }
                | ExecutorError::AdoptedGenerationMismatch { .. }
                | ExecutorError::UnknownRemoteExecutor(_)
                | ExecutorError::RemoteExecution { .. }
                | ExecutorError::RemoteProtocol { .. }),
            )) => return Err(error.into()),
            Some(Err(error)) => {
                eprintln!("tally: executor failed for {}: {error}", job.stable_key());
                (Verdict::Failed, 1, None, Vec::new())
            }
        };
        let computed_verdict = canonical_verdict(evidence_verdict, semantic_completion.as_ref());

        let (result, evidence, launches) = {
            let mut context = self.handler.context.write().await;
            if context.jobs.get(&finished.job_id).is_some_and(|job| {
                job.state == JobState::Completed
                    || job.row.attempt != finished.attempt
                    || job.row.lease_epoch != finished.lease_epoch
            }) {
                return Ok(());
            }
            let verdict = computed_verdict;
            let model = canonical_job_model(&job);
            let record = context.witness.append(WitnessBody {
                task_uuid: job.task_uuid.map(|uuid| uuid.to_string()),
                transition_timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                verdict,
                exit_code,
                artifact_content_hash: artifact_hash.clone(),
                gpu_seconds: None,
                wall_clock: finished.elapsed.as_secs_f64(),
                attempt: job.row.attempt,
                lease_epoch: job.row.lease_epoch,
                dedup_key: job.row.dedup_key.clone(),
                payload_hash: job.row.payload_hash.clone(),
                brief_hash: job.row.brief_hash.clone(),
                orchestration: job.row.orchestration.clone(),
                labor_class: job.labor_class,
                trace_ref: None,
                pools: Some(job.row.pools.clone()),
                executor: job.row.executor.clone(),
                charge: None,
                model: model.clone(),
                evidence_class: job.row.evidence_class.clone(),
                manifest_hash: job.row.manifest_hash.clone(),
                completion: semantic_completion.clone(),
            })?;
            let result = JobResult {
                task_uuid: job.task_uuid.map(|uuid| uuid.to_string()),
                job_id: job.job_id.to_string(),
                verdict,
                exit_code,
                artifact_content_hash: artifact_hash,
                attempt: job.row.attempt,
                lease_epoch: job.row.lease_epoch,
                witness_seq: record.seq,
                model,
                completion: semantic_completion,
            };
            let stable = job.stable_key();
            context.barriers.complete_job(&stable, result.value());
            let stored = context.jobs.get_mut(&finished.job_id).expect("job exists");
            stored.state = JobState::Completed;
            release_child_charge(&mut context, &job)?;
            context.guardrails.retire_parent(&job.stable_key());
            if let Some(task_uuid) = job.task_uuid {
                if let Some(row) = context.query_rows.get_mut(&task_uuid) {
                    row.status = RowStatus::Completed;
                }
                if let Some(detail) = context.query_details.get_mut(&task_uuid) {
                    detail.row_status = RowStatus::Completed;
                }
            }
            let evidence = serde_json::to_string(&job.row.evidence)
                .map_err(|error| DaemonError::Invalid(error.to_string()))?;
            let mut launches = Vec::new();
            if let Some(lease_id) = &job.lease_id {
                let epoch = context.epoch;
                let released = context.lease.release(lease_id, epoch, Utc::now())?;
                context.lease_jobs.remove(lease_id);
                launches.extend(promoted_jobs(&mut context, released.promoted));
            }
            (result, evidence, launches)
        };

        // Waiters become runnable immediately after the only terminal ack dependency:
        // the witness fsync above. Lease release, scrape, attestations, replica commit,
        // and journald are post-ack.
        tokio::task::yield_now().await;
        self.handler.complete_terminal_post_ack(TerminalWork {
            job,
            result,
            evidence,
            evidence_checks,
            launches,
            scrape_capture,
        });
        Ok(())
    }

    async fn tick_leases(handler: DaemonHandler) -> Result<(), DaemonError> {
        let mut context = handler.context.write().await;
        let mut launches = retry_unleased_jobs(&mut context, &handler.executor);
        let now = Utc::now();
        let planned = context.lease.engine_mut().plan_tick(now)?;
        let targets = planned
            .iter()
            .map(|grant| {
                let job_id = context
                    .lease_jobs
                    .get(&grant.lease_id)
                    .copied()
                    .ok_or_else(|| {
                        DaemonError::Invalid(format!(
                            "hard-preempt candidate {} is not a managed daemon job",
                            grant.lease_id
                        ))
                    })?;
                let job = context.jobs.get(&job_id).ok_or_else(|| {
                    DaemonError::Invalid(format!(
                        "hard-preempt candidate {} has no job",
                        grant.lease_id
                    ))
                })?;
                Ok((
                    grant.lease_id.clone(),
                    job_id,
                    job.identity(),
                    job.adopted_invocation_id.clone(),
                    job.row.executor.clone(),
                    job.row.attempt,
                    job.row.lease_epoch,
                ))
            })
            .collect::<Result<Vec<_>, DaemonError>>()?;

        let mut terminal = Vec::new();
        // Pair each physical reclaim with its canonical witness before touching
        // the next victim. If a later reclaim fails, every already-stopped job
        // is still durably represented and restart recovery is unambiguous.
        for (_, job_id, identity, expected_invocation_id, execution_target, attempt, lease_epoch) in
            &targets
        {
            handler
                .executor
                .reclaim_identity_exact_on(
                    execution_target.as_deref(),
                    identity,
                    expected_invocation_id.as_deref(),
                    *attempt,
                    *lease_epoch,
                )
                .await?;
            let job = context.jobs.get(job_id).expect("preempted job exists");
            let scrape_capture = match handler.executor.capture_generation_matches(
                identity,
                job.row.attempt,
                job.row.lease_epoch,
            ) {
                Ok(matches) => matches,
                Err(error) => {
                    eprintln!(
                        "tally: preempted job {} capture generation is unavailable: {error}",
                        job.stable_key()
                    );
                    false
                }
            };
            if let Some(work) = finalize_forced_locked(
                &mut context,
                *job_id,
                Verdict::Preempted,
                false,
                scrape_capture,
            )? {
                terminal.push(work);
            }
        }
        let reclaimed = targets
            .iter()
            .map(|(lease_id, ..)| lease_id.clone())
            .collect::<Vec<_>>();
        let outcome = context
            .lease
            .engine_mut()
            .commit_preemptions(&reclaimed, Utc::now())?;
        for (lease_id, job_id, ..) in &targets {
            context.lease_jobs.remove(lease_id);
            if let Some(job) = context.jobs.get_mut(job_id) {
                job.lease_id = None;
            }
        }
        launches.extend(promoted_jobs(&mut context, outcome.promoted));
        drop(context);
        for (_, job_id, ..) in targets {
            let _ = handler.execution_cancel.send(job_id);
        }
        for work in terminal {
            handler.complete_terminal_post_ack(work);
        }
        for job in launches {
            handler.spawn_execution(job);
        }
        Ok(())
    }
}

fn promoted_jobs(context: &mut Context, grants: Vec<LeaseGrant>) -> Vec<Job> {
    let mut launches = Vec::new();
    for grant in grants {
        let Some(job_id) = context.lease_jobs.get(&grant.lease_id).copied() else {
            continue;
        };
        if let Some(job) = context.jobs.get_mut(&job_id) {
            job.state = JobState::Running;
            job.lease_id = Some(grant.lease_id);
            launches.push(job.clone());
        }
    }
    launches
}

fn retry_unleased_jobs(context: &mut Context, executor: &Executor) -> Vec<Job> {
    let pending = context
        .jobs
        .values()
        .filter(|job| {
            job.state == JobState::Queued
                && job.lease_id.is_none()
                && !job.row.pools.iter().any(|pool| {
                    context.paused_pools.contains(pool) || context.unreachable_pools.contains(pool)
                })
        })
        .map(|job| job.job_id)
        .collect::<Vec<_>>();
    let mut launches = Vec::new();
    for job_id in pending {
        let job = context.jobs.get(&job_id).cloned().expect("job exists");
        let request = lease_request(&job, executor.unit_name(&job.identity()));
        match context.lease.admit(request, Utc::now()) {
            Ok(AdmitOutcome::Granted(grant)) => {
                context.lease_jobs.insert(grant.lease_id.clone(), job_id);
                let stored = context.jobs.get_mut(&job_id).expect("job exists");
                stored.lease_id = Some(grant.lease_id);
                stored.state = JobState::Running;
                launches.push(stored.clone());
            }
            Ok(AdmitOutcome::Queued { ticket_id, .. }) => {
                context.lease_jobs.insert(ticket_id.clone(), job_id);
                context.jobs.get_mut(&job_id).expect("job exists").lease_id = Some(ticket_id);
            }
            Err(error) => {
                eprintln!(
                    "tally: lease retry for {} failed: {error}",
                    job.stable_key()
                );
            }
        }
    }
    launches
}

fn resume_paused_jobs_locked(
    context: &mut Context,
    executor: &Executor,
    job_ids: Vec<Uuid>,
) -> Vec<Job> {
    let mut launches = Vec::new();
    for job_id in job_ids {
        let Some(job) = context.jobs.get(&job_id).cloned() else {
            continue;
        };
        if job.state != JobState::Paused
            || job.row.pools.iter().any(|pool| {
                context.paused_pools.contains(pool) || context.unreachable_pools.contains(pool)
            })
        {
            continue;
        }
        let unit = executor.unit_name(&job.identity());
        match context.lease.admit(lease_request(&job, unit), Utc::now()) {
            Ok(AdmitOutcome::Granted(grant)) => {
                context.lease_jobs.insert(grant.lease_id.clone(), job_id);
                let stored = context.jobs.get_mut(&job_id).expect("job exists");
                stored.lease_id = Some(grant.lease_id);
                stored.state = JobState::Running;
                launches.push(stored.clone());
            }
            Ok(AdmitOutcome::Queued { ticket_id, .. }) => {
                context.lease_jobs.insert(ticket_id.clone(), job_id);
                let stored = context.jobs.get_mut(&job_id).expect("job exists");
                stored.lease_id = Some(ticket_id);
                stored.state = JobState::Queued;
            }
            Err(error) => {
                eprintln!(
                    "tally: resumed job {} is waiting for lease retry: {error}",
                    job.stable_key()
                );
                let stored = context.jobs.get_mut(&job_id).expect("job exists");
                stored.lease_id = None;
                stored.state = JobState::Queued;
            }
        }
    }
    launches
}

fn pool_representations(
    mut plan: crate::recovery::RecoveryPlan,
    pool: &str,
    renderable: &BTreeSet<Uuid>,
) -> crate::recovery::RecoveryPlan {
    let selected = plan
        .actions
        .iter()
        .filter_map(|action| match action {
            RecoveryAction::RePresent { row, .. }
                if row.pools.iter().any(|name| name == pool) && renderable.contains(&row.uuid) =>
            {
                Some(row.uuid)
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    plan.actions.retain(|action| {
        matches!(action, RecoveryAction::RePresent { row, .. } if selected.contains(&row.uuid))
    });
    plan.rows.retain(|row| selected.contains(&row.row.uuid));
    plan.lease_epoch_fences
        .retain(|fence| match fence.identity {
            RecoveryIdentity::Task(uuid) => selected.contains(&uuid),
            RecoveryIdentity::Job(_) => false,
        });
    plan.advisory_return_attestations.clear();
    plan
}

fn renderable_pool_return_rows(
    plan: &crate::recovery::RecoveryPlan,
    config: &Config,
    executor: &Executor,
    attestation_path: &Path,
) -> BTreeSet<Uuid> {
    let mut selected = BTreeSet::new();
    for action in &plan.actions {
        let RecoveryAction::RePresent {
            row,
            trigger: RetryTrigger::PoolReturn,
            ..
        } = action
        else {
            continue;
        };
        if config
            .adapters
            .get(&row.adapter)
            .is_none_or(|adapter| adapter.resume.is_none())
        {
            eprintln!(
                "tally: leaving pool-return row {} terminal because adapter {:?} has no resume template",
                row.uuid, row.adapter
            );
            continue;
        }
        match recovery_adapter_invocation(config, action, row, executor, attestation_path) {
            Ok(_) => {
                selected.insert(row.uuid);
            }
            Err(error) => eprintln!(
                "tally: leaving pool-return row {} terminal because its resume checkpoint is unavailable: {error}",
                row.uuid
            ),
        }
    }
    selected
}

fn merge_selected_pool_returns(
    mut base: crate::recovery::RecoveryPlan,
    triggered: crate::recovery::RecoveryPlan,
    selected: &BTreeSet<Uuid>,
) -> crate::recovery::RecoveryPlan {
    if selected.is_empty() {
        return base;
    }
    base.rows.retain(|row| !selected.contains(&row.row.uuid));
    base.rows.extend(
        triggered
            .rows
            .iter()
            .filter(|row| selected.contains(&row.row.uuid))
            .cloned(),
    );
    base.actions.retain(|action| {
        recovery_action_task_uuid(action).is_none_or(|uuid| !selected.contains(&uuid))
    });
    base.actions.extend(
        triggered
            .actions
            .iter()
            .filter(|action| {
                recovery_action_task_uuid(action).is_some_and(|uuid| selected.contains(&uuid))
            })
            .cloned(),
    );
    base.lease_epoch_fences
        .retain(|fence| match fence.identity {
            RecoveryIdentity::Task(uuid) => !selected.contains(&uuid),
            RecoveryIdentity::Job(_) => true,
        });
    base.lease_epoch_fences.extend(
        triggered
            .lease_epoch_fences
            .iter()
            .filter(|fence| {
                matches!(fence.identity, RecoveryIdentity::Task(uuid) if selected.contains(&uuid))
            })
            .cloned(),
    );
    base
}

fn recovery_action_task_uuid(action: &RecoveryAction) -> Option<Uuid> {
    match action {
        RecoveryAction::QueueExisting { task_uuid, .. }
        | RecoveryAction::AwaitRetry { task_uuid, .. }
        | RecoveryAction::RetryExhausted { task_uuid, .. } => Some(*task_uuid),
        RecoveryAction::AdoptRunning {
            identity: RecoveryIdentity::Task(uuid),
            ..
        }
        | RecoveryAction::ReconcileExit {
            identity: RecoveryIdentity::Task(uuid),
            ..
        } => Some(*uuid),
        RecoveryAction::RePresent { row, .. } => Some(row.uuid),
        RecoveryAction::AwaitUnitCollection {
            identity: RecoveryIdentity::Task(uuid),
            ..
        } => Some(*uuid),
        RecoveryAction::AdoptRunning {
            identity: RecoveryIdentity::Job(_),
            ..
        }
        | RecoveryAction::ReconcileExit {
            identity: RecoveryIdentity::Job(_),
            ..
        }
        | RecoveryAction::AwaitUnitCollection {
            identity: RecoveryIdentity::Job(_),
            ..
        } => None,
    }
}

const MAX_POOL_LOSS_INTENT_BYTES: u64 = 1024 * 1024;

fn pool_loss_intent_directory(state_dir: &Path) -> PathBuf {
    state_dir.join("producers/pool-loss-intents")
}

fn write_pool_loss_intent(state_dir: &Path, job: &Job) -> Result<PathBuf, DaemonError> {
    let directory = pool_loss_intent_directory(state_dir);
    create_daemon_dir_durable(&directory)?;
    let path = directory.join(format!(
        "{}-{}-{}.json",
        job.row.uuid, job.row.attempt, job.row.lease_epoch
    ));
    let intent = PoolLossIntent {
        schema_version: 1,
        row: job.row.clone(),
        labor_class: job.labor_class,
        adopted_invocation_id: job.adopted_invocation_id.clone(),
        model_is_advisory: job.model_is_advisory,
    };
    if path.exists() {
        if read_pool_loss_intent(&path)? == intent {
            return Ok(path);
        }
        return Err(DaemonError::Invalid(format!(
            "pool-loss intent {} conflicts with the active execution generation",
            path.display()
        )));
    }
    let bytes = serde_json::to_vec(&intent).map_err(|error| {
        DaemonError::Invalid(format!("cannot encode pool-loss intent: {error}"))
    })?;
    if bytes.len().saturating_add(1) > MAX_POOL_LOSS_INTENT_BYTES as usize {
        return Err(DaemonError::Invalid(format!(
            "pool-loss intent exceeds the {MAX_POOL_LOSS_INTENT_BYTES} byte limit"
        )));
    }
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(&temporary, source))?;
    match std::fs::hard_link(&temporary, &path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if read_pool_loss_intent(&path)? != intent {
                let _ = std::fs::remove_file(&temporary);
                return Err(DaemonError::Invalid(format!(
                    "pool-loss intent {} raced with a conflicting generation",
                    path.display()
                )));
            }
        }
        Err(source) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(io_error(&path, source));
        }
    }
    std::fs::remove_file(&temporary).map_err(|source| io_error(&temporary, source))?;
    File::open(&directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(&directory, source))?;
    Ok(path)
}

fn read_pool_loss_intent(path: &Path) -> Result<PoolLossIntent, DaemonError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.is_file() || metadata.len() > MAX_POOL_LOSS_INTENT_BYTES {
        return Err(DaemonError::Invalid(format!(
            "pool-loss intent {} is not a bounded regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_POOL_LOSS_INTENT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() as u64 > MAX_POOL_LOSS_INTENT_BYTES {
        return Err(DaemonError::Invalid(format!(
            "pool-loss intent {} grew beyond its byte limit",
            path.display()
        )));
    }
    let intent: PoolLossIntent = serde_json::from_slice(&bytes)
        .map_err(|error| DaemonError::Invalid(format!("invalid pool-loss intent: {error}")))?;
    if intent.schema_version != 1 {
        return Err(DaemonError::Invalid(format!(
            "pool-loss intent {} has unsupported schema version {}",
            path.display(),
            intent.schema_version
        )));
    }
    intent
        .row
        .validate()
        .map_err(|error| DaemonError::Invalid(error.to_string()))?;
    Ok(intent)
}

fn clear_pool_loss_intent(path: &Path) -> Result<(), DaemonError> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error(path, source)),
    }
    let parent = path.parent().ok_or_else(|| {
        DaemonError::Invalid(format!("pool-loss intent {} has no parent", path.display()))
    })?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))
}

async fn reconcile_pool_loss_intents(
    paths: &DaemonPaths,
    executor: &Executor,
    ledger: &mut WitnessLedger,
) -> Result<(), DaemonError> {
    let directory = pool_loss_intent_directory(&paths.state_dir);
    if !directory.exists() {
        return Ok(());
    }
    let durable_ids = read_acknowledged_events(&paths.events_dir())?
        .into_iter()
        .map(|event| event.row.uuid)
        .collect::<BTreeSet<_>>();
    let (report, mut records) = read_verified_records(&paths.witness_path())?;
    if !report.ok {
        return Err(DaemonError::Invalid(
            "witness verification failed while reconciling pool-loss intents".to_owned(),
        ));
    }
    let mut entries = std::fs::read_dir(&directory)
        .map_err(|source| io_error(&directory, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error(&directory, source))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| !name.starts_with('.') && name.ends_with(".json"))
        {
            continue;
        }
        let intent = read_pool_loss_intent(&path)?;
        if !durable_ids.contains(&intent.row.uuid) {
            return Err(DaemonError::Invalid(format!(
                "pool-loss intent {} has no acknowledged durable row",
                path.display()
            )));
        }
        let task_uuid = intent.row.uuid.to_string();
        let same_generation = records
            .iter()
            .filter(|record| {
                record.task_uuid.as_deref() == Some(task_uuid.as_str())
                    && record.attempt == intent.row.attempt
            })
            .collect::<Vec<_>>();
        match same_generation.as_slice() {
            [record]
                if record.verdict == Verdict::PoolVanished
                    && record.lease_epoch == intent.row.lease_epoch =>
            {
                clear_pool_loss_intent(&path)?;
                continue;
            }
            [] => {}
            _ => {
                return Err(DaemonError::Invalid(format!(
                    "pool-loss intent {} conflicts with canonical witness history",
                    path.display()
                )))
            }
        }
        let identity = ExecutionIdentity {
            job_id: intent.row.uuid,
            task_uuid: Some(intent.row.uuid),
        };
        executor
            .reclaim_identity_exact_on(
                intent.row.executor.as_deref(),
                &identity,
                intent.adopted_invocation_id.as_deref(),
                intent.row.attempt,
                intent.row.lease_epoch,
            )
            .await?;
        let job = Job {
            job_id: intent.row.uuid,
            task_uuid: Some(intent.row.uuid),
            row: intent.row,
            invocation: AdapterInvocation {
                argv: Vec::new(),
                env: BTreeMap::new(),
                hardening: Default::default(),
                yield_hook: None,
            },
            labor_class: intent.labor_class,
            state: JobState::Running,
            lease_id: None,
            adopted: intent.adopted_invocation_id.is_some(),
            adopted_invocation_id: intent.adopted_invocation_id,
            model_is_advisory: intent.model_is_advisory,
        };
        records.push(ledger.append(forced_witness(&job, Verdict::PoolVanished))?);
        clear_pool_loss_intent(&path)?;
    }
    Ok(())
}

fn create_daemon_dir_durable(path: &Path) -> Result<(), DaemonError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => return Ok(()),
        Ok(_) => {
            return Err(DaemonError::Invalid(format!(
                "{} is not a real directory",
                path.display()
            )))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(io_error(path, source)),
    }
    let parent = path.parent().ok_or_else(|| {
        DaemonError::Invalid(format!("directory {} has no parent", path.display()))
    })?;
    create_daemon_dir_durable(parent)?;
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata =
                std::fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(DaemonError::Invalid(format!(
                    "{} is not a real directory",
                    path.display()
                )));
            }
        }
        Err(source) => return Err(io_error(path, source)),
    }
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))
}

fn pool_transition_marker(state_dir: &Path, producer: &str, generation: u64) -> PathBuf {
    state_dir
        .join("producers")
        .join("pool-transition-applied")
        .join(format!("{producer}-{generation}.json"))
}

fn pool_transition_marker_exists(path: &Path) -> Result<bool, DaemonError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(DaemonError::Invalid(format!(
            "pool transition marker {} is not a regular file",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(path, source)),
    }
}

fn write_pool_transition_marker(
    path: &Path,
    producer: &str,
    transition: ReachabilityTransition,
    generation: u64,
) -> Result<(), DaemonError> {
    let parent = path.parent().ok_or_else(|| {
        DaemonError::Invalid(format!(
            "pool transition marker {} has no parent",
            path.display()
        ))
    })?;
    create_daemon_dir_durable(parent)?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec(&json!({
        "producer": producer,
        "transition": transition,
        "generation": generation,
    }))
    .map_err(|error| DaemonError::Invalid(error.to_string()))?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .map_err(|source| io_error(&temporary, source))?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error(&temporary, source))?;
    match std::fs::hard_link(&temporary, path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(io_error(path, source));
        }
    }
    std::fs::remove_file(&temporary).map_err(|source| io_error(&temporary, source))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(parent, source))
}

fn prepare_paths(paths: &DaemonPaths) -> Result<(), DaemonError> {
    for path in [&paths.state_dir, &paths.data_dir] {
        if !path.is_absolute() {
            return Err(DaemonError::Invalid(format!(
                "daemon path must be absolute: {}",
                path.display()
            )));
        }
        std::fs::create_dir_all(path).map_err(|source| io_error(path, source))?;
    }
    let socket_parent = paths
        .socket
        .parent()
        .ok_or_else(|| DaemonError::Invalid("socket has no parent directory".to_owned()))?;
    std::fs::create_dir_all(socket_parent).map_err(|source| io_error(socket_parent, source))?;
    Ok(())
}

fn acquire_daemon_lock(state_dir: &Path) -> Result<File, DaemonError> {
    let path = state_dir.join("daemon.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|source| io_error(&path, source))?;
    file.try_lock_exclusive().map_err(|source| {
        if source.kind() == io::ErrorKind::WouldBlock {
            DaemonError::Invalid(format!(
                "another tally daemon already owns {}",
                path.display()
            ))
        } else {
            io_error(&path, source)
        }
    })?;
    Ok(file)
}

fn facts_witness(
    _plan: &crate::recovery::RecoveryPlan,
    paths: &DaemonPaths,
) -> Result<Vec<crate::witness::WitnessRecord>, DaemonError> {
    let (report, records) = crate::witness::read_verified_records(&paths.witness_path())?;
    if !report.ok {
        return Err(DaemonError::Invalid(
            "witness verification failed".to_owned(),
        ));
    }
    Ok(records)
}

fn recovery_gh_completions(
    plan: &crate::recovery::RecoveryPlan,
    records: &[crate::witness::WitnessRecord],
) -> Result<Vec<GhTerminalWork>, DaemonError> {
    let rows = plan
        .rows
        .iter()
        .filter(|row| row.row.gh_origin.is_some())
        .map(|row| (row.row.uuid, row.row.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut latest = BTreeMap::<Uuid, &crate::witness::WitnessRecord>::new();
    for record in records {
        if !matches!(record.verdict, Verdict::Pass | Verdict::Reused) {
            continue;
        }
        let Some(task_uuid) = record.task_uuid.as_deref() else {
            continue;
        };
        let task_uuid = Uuid::parse_str(task_uuid).map_err(|_| {
            DaemonError::Invalid(format!(
                "successful GitHub witness {} has invalid task UUID {task_uuid:?}",
                record.seq
            ))
        })?;
        if rows.contains_key(&task_uuid)
            && latest
                .get(&task_uuid)
                .is_none_or(|selected| record.seq > selected.seq)
        {
            latest.insert(task_uuid, record);
        }
    }
    Ok(latest
        .into_iter()
        .map(|(task_uuid, record)| GhTerminalWork {
            row: rows[&task_uuid].clone(),
            result: JobResult {
                task_uuid: Some(task_uuid.to_string()),
                job_id: task_uuid.to_string(),
                verdict: record.verdict,
                exit_code: record.exit_code,
                artifact_content_hash: record.artifact_content_hash.clone(),
                attempt: record.attempt,
                lease_epoch: record.lease_epoch,
                witness_seq: record.seq,
                model: record.model.clone(),
                completion: record.completion.clone(),
            },
        })
        .collect())
}

fn reconcile_reuse_witnesses(
    durable: &DurableRecoveryFacts,
    ledger: &mut WitnessLedger,
) -> Result<bool, DaemonError> {
    let mut appended = false;
    for event in durable.events() {
        let Some(reuse) = &event.reuse else {
            continue;
        };
        let dedup_key = event.row.dedup_key.as_deref().ok_or_else(|| {
            DaemonError::Invalid(format!("reuse event {} has no dedup key", event.event_id))
        })?;
        let matched = durable
            .witness()
            .iter()
            .find(|record| record.seq == reuse.matched_witness_seq)
            .ok_or_else(|| {
                DaemonError::Invalid(format!(
                    "reuse event {} references missing witness {}",
                    event.event_id, reuse.matched_witness_seq
                ))
            })?;
        if matched.verdict != Verdict::Pass
            || matched.dedup_key.as_deref() != Some(dedup_key)
            || matched.artifact_content_hash.as_deref()
                != Some(reuse.artifact_content_hash.as_str())
        {
            return Err(DaemonError::Invalid(format!(
                "reuse event {} does not match prior passing witness {}",
                event.event_id, reuse.matched_witness_seq
            )));
        }

        let task_uuid = event.row.uuid.to_string();
        let existing = durable
            .witness()
            .iter()
            .filter(|record| record.task_uuid.as_deref() == Some(task_uuid.as_str()))
            .collect::<Vec<_>>();
        match existing.as_slice() {
            [] => {
                ledger.append(WitnessBody {
                    task_uuid: Some(task_uuid),
                    transition_timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                    verdict: Verdict::Reused,
                    exit_code: 0,
                    artifact_content_hash: Some(reuse.artifact_content_hash.clone()),
                    gpu_seconds: None,
                    wall_clock: 0.0,
                    attempt: event.row.attempt,
                    lease_epoch: event.row.lease_epoch,
                    dedup_key: event.row.dedup_key.clone(),
                    payload_hash: event.row.payload_hash.clone(),
                    brief_hash: event.row.brief_hash.clone(),
                    orchestration: event.row.orchestration.clone(),
                    labor_class: LaborClass::Reused,
                    trace_ref: None,
                    pools: Some(event.row.pools.clone()),
                    executor: event.row.executor.clone(),
                    charge: None,
                    model: event.row.model.clone(),
                    evidence_class: event.row.evidence_class.clone(),
                    manifest_hash: event.row.manifest_hash.clone(),
                    completion: None,
                })?;
                appended = true;
            }
            [record]
                if record.seq > reuse.matched_witness_seq
                    && reuse_record_matches(event, reuse, record) => {}
            _ => {
                return Err(DaemonError::Invalid(format!(
                    "reuse event {} has a conflicting canonical witness history",
                    event.event_id
                )));
            }
        }
    }
    Ok(appended)
}

fn reuse_record_matches(
    event: &DurableEnqueueEvent,
    reuse: &crate::taskdb::DurableReuse,
    record: &WitnessRecord,
) -> bool {
    record.verdict == Verdict::Reused
        && record.exit_code == 0
        && record.artifact_content_hash.as_deref() == Some(reuse.artifact_content_hash.as_str())
        && record.attempt == event.row.attempt
        && record.lease_epoch == event.row.lease_epoch
        && record.dedup_key == event.row.dedup_key
        && record.payload_hash == event.row.payload_hash
        && record.brief_hash == event.row.brief_hash
        && record.orchestration == event.row.orchestration
        && record.labor_class == LaborClass::Reused
        && record.pools.as_ref() == Some(&event.row.pools)
        && record.executor == event.row.executor
}

fn recovery_query_rows(plan: &crate::recovery::RecoveryPlan) -> BTreeMap<Uuid, RowFact> {
    plan.rows
        .iter()
        .map(|recovery| {
            let status = match recovery.state {
                RecoveryRowState::Completed => RowStatus::Completed,
                RecoveryRowState::Deleted => RowStatus::Deleted,
                RecoveryRowState::Pending
                | RecoveryRowState::AdoptedRunning
                | RecoveryRowState::AwaitingReconciliation => RowStatus::Pending,
            };
            (recovery.row.uuid, query_row(&recovery.row, status))
        })
        .collect()
}

fn recovery_query_details(plan: &crate::recovery::RecoveryPlan) -> BTreeMap<Uuid, RowDetailFact> {
    plan.rows
        .iter()
        .map(|recovery| {
            let status = match recovery.state {
                RecoveryRowState::Completed => RowStatus::Completed,
                RecoveryRowState::Deleted => RowStatus::Deleted,
                RecoveryRowState::Pending
                | RecoveryRowState::AdoptedRunning
                | RecoveryRowState::AwaitingReconciliation => RowStatus::Pending,
            };
            (
                recovery.row.uuid,
                RowDetailFact::from_seed(&recovery.row, status, recovery.labor_class),
            )
        })
        .collect()
}

fn hydrate_completed_adapter_metadata(
    plan: &mut crate::recovery::RecoveryPlan,
    config: &Config,
    executor: &Executor,
) {
    let engine = AdapterEngine::new(&config.adapters);
    for recovery in &mut plan.rows {
        if !matches!(
            recovery.state,
            RecoveryRowState::Completed | RecoveryRowState::Deleted
        ) || config
            .adapters
            .get(&recovery.row.adapter)
            .is_none_or(|adapter| adapter.scrape.is_empty())
        {
            continue;
        }
        let identity = ExecutionIdentity {
            job_id: recovery.row.uuid,
            task_uuid: Some(recovery.row.uuid),
        };
        match executor.capture_generation_matches(
            &identity,
            recovery.row.attempt,
            recovery.row.lease_epoch,
        ) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                eprintln!(
                    "tally: retained capture generation for {} could not be read: {error}",
                    recovery.row.uuid
                );
                continue;
            }
        }
        let paths = executor.paths(&identity);
        match engine.scrape_paths(&recovery.row.adapter, &paths) {
            Ok(captures) => {
                if let Ok(Some(session_ref)) = captures.session_ref() {
                    recovery.row.session_ref = Some(session_ref.to_owned());
                }
                if let Ok(Some(model)) = captures.model() {
                    recovery.row.model = Some(model.to_owned());
                }
                if let Ok(Some(final_message)) = captures.final_message() {
                    recovery.row.final_message = Some(final_message.to_owned());
                }
            }
            Err(error) => eprintln!(
                "tally: retained adapter metadata for {} could not be scraped: {error}",
                recovery.row.uuid
            ),
        }
    }
}

fn hydrate_represent_adapter_metadata(
    plan: &mut crate::recovery::RecoveryPlan,
    config: &Config,
    executor: &Executor,
    attestation_path: &Path,
) -> Result<(), DaemonError> {
    let updates = plan
        .actions
        .iter()
        .filter_map(|action| match action {
            RecoveryAction::RePresent { row, .. } => Some((action, row.as_ref())),
            _ => None,
        })
        .map(|(action, row)| {
            let (_, captures) =
                recovery_adapter_invocation(config, action, row, executor, attestation_path)?;
            let captures = captures.expect("RePresent always returns its resume captures");
            Ok::<_, DaemonError>((
                row.uuid,
                captures.session_ref()?.map(str::to_owned),
                captures.model()?.map(str::to_owned),
                captures.final_message()?.map(str::to_owned),
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (uuid, session_ref, model, final_message) in updates {
        let recovery = plan
            .rows
            .iter_mut()
            .find(|recovery| recovery.row.uuid == uuid)
            .ok_or_else(|| DaemonError::Invalid(format!("recovery row {uuid} is absent")))?;
        if session_ref.is_some() {
            recovery.row.session_ref = session_ref;
        }
        if model.is_some() {
            recovery.row.model = model;
        }
        if final_message.is_some() {
            recovery.row.final_message = final_message;
        }
    }
    Ok(())
}

fn hydrate_adopted_adapter_metadata(
    plan: &mut crate::recovery::RecoveryPlan,
    attestation_path: &Path,
) -> Result<(), DaemonError> {
    let targets = plan
        .actions
        .iter()
        .filter_map(|action| match action {
            RecoveryAction::AdoptRunning {
                identity: RecoveryIdentity::Task(uuid),
                attempt,
                ..
            } => Some((*uuid, *attempt)),
            RecoveryAction::ReconcileExit {
                identity: RecoveryIdentity::Task(uuid),
                record,
                ..
            } => Some((*uuid, record.attempt)),
            _ => None,
        })
        .collect::<Vec<_>>();
    for (uuid, current_attempt) in targets {
        let recovery = plan
            .rows
            .iter_mut()
            .find(|recovery| recovery.row.uuid == uuid)
            .ok_or_else(|| DaemonError::Invalid(format!("recovery row {uuid} is absent")))?;
        let captures = match verified_latest_adapter_attestation_before(
            attestation_path,
            uuid,
            &recovery.row.adapter,
            current_attempt,
        ) {
            Ok(Some(captures)) => captures,
            Ok(None) => continue,
            Err(error) => {
                eprintln!(
                    "tally: adopted adapter metadata for {uuid} could not be hydrated: {error}"
                );
                continue;
            }
        };
        if let Some(session_ref) = captures.session_ref()? {
            recovery.row.session_ref = Some(session_ref.to_owned());
        }
        if let Some(model) = captures.model()? {
            recovery.row.model = Some(model.to_owned());
        }
        if let Some(final_message) = captures.final_message()? {
            recovery.row.final_message = Some(final_message.to_owned());
        }
    }
    Ok(())
}

fn reconcile_retained_adapter_attestations(
    plan: &crate::recovery::RecoveryPlan,
    witness: &[WitnessRecord],
    config: &Config,
    executor: &Executor,
    attestation_path: &Path,
) {
    let existing = match adapter_attestation_keys(attestation_path) {
        Ok(existing) => existing,
        Err(error) => {
            eprintln!("tally: retained adapter attestations cannot be reconciled: {error}");
            return;
        }
    };
    let rows = plan
        .rows
        .iter()
        .map(|recovery| (recovery.row.uuid, &recovery.row))
        .collect::<BTreeMap<_, _>>();
    let mut latest = BTreeMap::<Uuid, &WitnessRecord>::new();
    for record in witness {
        if let Some(uuid) = record
            .task_uuid
            .as_deref()
            .and_then(|task_uuid| Uuid::parse_str(task_uuid).ok())
        {
            latest.insert(uuid, record);
        }
    }
    let engine = AdapterEngine::new(&config.adapters);
    for (task_uuid, record) in latest {
        let Some(row) = rows.get(&task_uuid) else {
            continue;
        };
        if existing.contains(&(task_uuid.to_string(), record.attempt, record.lease_epoch))
            || config
                .adapters
                .get(&row.adapter)
                .is_none_or(|adapter| adapter.scrape.is_empty())
        {
            continue;
        }
        let identity = ExecutionIdentity {
            job_id: task_uuid,
            task_uuid: Some(task_uuid),
        };
        match executor.capture_generation_matches(&identity, record.attempt, record.lease_epoch) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                eprintln!(
                    "tally: retained capture generation for {task_uuid} could not be read: {error}"
                );
                continue;
            }
        }
        let captures = match engine.scrape_paths(&row.adapter, &executor.paths(&identity)) {
            Ok(captures) if !captures.captures.is_empty() => captures,
            Ok(_) => continue,
            Err(error) => {
                eprintln!(
                    "tally: retained adapter capture for {task_uuid} could not be scraped: {error}"
                );
                continue;
            }
        };
        if let Err(error) = append_attestation(
            attestation_path,
            json!({
                "kind": "adapter-scrape",
                "taskUuid": task_uuid.to_string(),
                "jobId": task_uuid.to_string(),
                "adapter": row.adapter,
                "attempt": record.attempt,
                "leaseEpoch": record.lease_epoch,
                "captures": captures.captures,
                "usageAuthority": "advisory-only",
                "reconciledAfterRestart": true,
            }),
        ) {
            eprintln!(
                "tally: retained adapter attestation for {task_uuid} could not be appended: {error}"
            );
        }
    }
}

fn adapter_attestation_keys(path: &Path) -> Result<BTreeSet<(String, u32, u64)>, DaemonError> {
    let report = verify_attestations(path)?;
    if !report.ok {
        return Err(DaemonError::Invalid(format!(
            "attestation chain is invalid: {}",
            report
                .problem
                .as_deref()
                .unwrap_or("unknown verification failure")
        )));
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(source) => return Err(io_error(path, source)),
    };
    let mut keys = BTreeSet::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: AttestationRecord = serde_json::from_str(line).map_err(|error| {
            DaemonError::Invalid(format!(
                "attestation line {} cannot be decoded: {error}",
                index + 1
            ))
        })?;
        if record.payload.get("kind").and_then(Value::as_str) != Some("adapter-scrape") {
            continue;
        }
        if let (Some(task_uuid), Some(attempt), Some(lease_epoch)) = (
            record.payload.get("taskUuid").and_then(Value::as_str),
            record
                .payload
                .get("attempt")
                .and_then(Value::as_u64)
                .and_then(|attempt| u32::try_from(attempt).ok()),
            record.payload.get("leaseEpoch").and_then(Value::as_u64),
        ) {
            keys.insert((task_uuid.to_owned(), attempt, lease_epoch));
        }
    }
    Ok(keys)
}

fn verified_adapter_attestation_captures(
    path: &Path,
    task_uuid: Uuid,
    adapter: &str,
    attempt: u32,
    lease_epoch: u64,
) -> Result<Option<ScrapeResult>, DaemonError> {
    let report = verify_attestations(path)?;
    if !report.ok {
        return Err(DaemonError::Invalid(format!(
            "attestation chain is invalid: {}",
            report
                .problem
                .as_deref()
                .unwrap_or("unknown verification failure")
        )));
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path, source)),
    };
    let task_uuid = task_uuid.to_string();
    let mut selected = None;
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: AttestationRecord = serde_json::from_str(line).map_err(|error| {
            DaemonError::Invalid(format!(
                "attestation line {} cannot be decoded: {error}",
                index + 1
            ))
        })?;
        let payload = &record.payload;
        if payload.get("kind").and_then(Value::as_str) != Some("adapter-scrape")
            || payload.get("taskUuid").and_then(Value::as_str) != Some(task_uuid.as_str())
            || payload.get("adapter").and_then(Value::as_str) != Some(adapter)
            || payload.get("attempt").and_then(Value::as_u64) != Some(u64::from(attempt))
            || payload.get("leaseEpoch").and_then(Value::as_u64) != Some(lease_epoch)
        {
            continue;
        }
        let captures = payload.get("captures").cloned().ok_or_else(|| {
            DaemonError::Invalid(format!(
                "adapter scrape attestation for {task_uuid} attempt {attempt} has no captures"
            ))
        })?;
        let result = ScrapeResult {
            captures: serde_json::from_value(captures).map_err(|error| {
                DaemonError::Invalid(format!(
                    "adapter scrape attestation for {task_uuid} attempt {attempt} has invalid captures: {error}"
                ))
            })?,
        };
        result.session_ref()?;
        result.model()?;
        result.final_message()?;
        selected = Some(result);
    }
    Ok(selected)
}

fn verified_latest_adapter_attestation_before(
    path: &Path,
    task_uuid: Uuid,
    adapter: &str,
    before_attempt: u32,
) -> Result<Option<ScrapeResult>, DaemonError> {
    let report = verify_attestations(path)?;
    if !report.ok {
        return Err(DaemonError::Invalid(format!(
            "attestation chain is invalid: {}",
            report
                .problem
                .as_deref()
                .unwrap_or("unknown verification failure")
        )));
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path, source)),
    };
    let task_uuid = task_uuid.to_string();
    let mut selected = None;
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: AttestationRecord = serde_json::from_str(line).map_err(|error| {
            DaemonError::Invalid(format!(
                "attestation line {} cannot be decoded: {error}",
                index + 1
            ))
        })?;
        let payload = &record.payload;
        let Some(attempt) = payload
            .get("attempt")
            .and_then(Value::as_u64)
            .and_then(|attempt| u32::try_from(attempt).ok())
        else {
            continue;
        };
        if payload.get("kind").and_then(Value::as_str) != Some("adapter-scrape")
            || payload.get("taskUuid").and_then(Value::as_str) != Some(task_uuid.as_str())
            || payload.get("adapter").and_then(Value::as_str) != Some(adapter)
            || attempt >= before_attempt
            || payload.get("leaseEpoch").and_then(Value::as_u64).is_none()
        {
            continue;
        }
        let captures = payload.get("captures").cloned().ok_or_else(|| {
            DaemonError::Invalid(format!(
                "adapter scrape attestation for {task_uuid} attempt {attempt} has no captures"
            ))
        })?;
        let result = ScrapeResult {
            captures: serde_json::from_value(captures).map_err(|error| {
                DaemonError::Invalid(format!(
                    "adapter scrape attestation for {task_uuid} attempt {attempt} has invalid captures: {error}"
                ))
            })?,
        };
        result.session_ref()?;
        result.model()?;
        result.final_message()?;
        if selected
            .as_ref()
            .is_none_or(|(selected_attempt, _)| attempt >= *selected_attempt)
        {
            selected = Some((attempt, result));
        }
    }
    Ok(selected.map(|(_, captures)| captures))
}

fn recovered_model_is_advisory(
    row: &RowSeed,
    captures: Option<&ScrapeResult>,
    adopted: bool,
) -> bool {
    if captures.is_some_and(|captures| captures.captures.contains_key("model")) {
        return true;
    }
    // Recovery availability must not depend on advisory-chain health. Treat
    // any model projected onto an adopted execution conservatively as
    // advisory; the durable enqueue API does not currently accept a model.
    adopted && row.model.is_some()
}

fn ensure_verified_resume_attestation(
    path: &Path,
    row: &RowSeed,
    attempt: u32,
    lease_epoch: u64,
    captures: &ScrapeResult,
) -> Result<(), DaemonError> {
    if let Some(stored) =
        verified_adapter_attestation_captures(path, row.uuid, &row.adapter, attempt, lease_epoch)?
    {
        if stored != *captures {
            return Err(DaemonError::Invalid(format!(
                "verified adapter scrape attestation for {} attempt {} disagrees with retained capture",
                row.uuid, attempt
            )));
        }
        return Ok(());
    }
    append_attestation(
        path,
        json!({
            "kind": "adapter-scrape",
            "taskUuid": row.uuid.to_string(),
            "jobId": row.uuid.to_string(),
            "adapter": row.adapter,
            "attempt": attempt,
            "leaseEpoch": lease_epoch,
            "captures": captures.captures,
            "usageAuthority": "advisory-only",
            "recoveryCheckpoint": true,
        }),
    )?;
    let stored =
        verified_adapter_attestation_captures(path, row.uuid, &row.adapter, attempt, lease_epoch)?
            .ok_or_else(|| {
                DaemonError::Invalid(format!(
                    "adapter scrape attestation for {} attempt {} was not durable after append",
                    row.uuid, attempt
                ))
            })?;
    if stored != *captures {
        return Err(DaemonError::Invalid(format!(
            "durable adapter scrape attestation for {} attempt {} changed during append",
            row.uuid, attempt
        )));
    }
    Ok(())
}

fn query_row(row: &RowSeed, status: RowStatus) -> RowFact {
    RowFact {
        task_uuid: row.uuid.to_string(),
        description: row.description.clone(),
        argv: row.argv.clone(),
        brief_hash: row.brief_hash.clone(),
        orchestration: row.orchestration.clone(),
        status,
        priority: priority_name(row.priority).to_owned(),
        pools: Some(row.pools.clone()),
        executor: row.executor.clone(),
        source: Some(source_name(row.source).to_owned()),
        session_ref: row.session_ref.clone(),
        final_message: row.final_message.clone(),
        cwd: row
            .cwd
            .as_ref()
            .map(|cwd| cwd.to_string_lossy().into_owned()),
        workspace: row.workspace.clone(),
        resumed_from: row.resumed_from.clone(),
        attempt: row.attempt,
        model: row.model.clone(),
        gh_origin: row
            .gh_origin
            .as_ref()
            .and_then(crate::query::GhOriginProjection::from_origin),
        related_trigger: row.related_trigger.clone(),
    }
}

fn priority_name(priority: Priority) -> &'static str {
    match priority {
        Priority::Interrupt => "interrupt",
        Priority::High => "high",
        Priority::Medium => "medium",
        Priority::Low => "low",
    }
}

fn source_name(source: EnqueueSource) -> &'static str {
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

fn restore_completed_aliases(
    context: &mut Context,
    records: &[crate::witness::WitnessRecord],
) -> Result<(), DaemonError> {
    for record in records {
        let Some(task_uuid) = record.task_uuid.as_deref() else {
            continue;
        };
        let uuid = Uuid::parse_str(task_uuid)
            .map_err(|_| DaemonError::Invalid(format!("invalid witnessed UUID {task_uuid}")))?;
        context.aliases.insert(task_uuid.to_owned(), uuid);
    }
    Ok(())
}

fn validate_recovery_briefs(
    plan: &crate::recovery::RecoveryPlan,
    data_dir: &Path,
) -> Result<(), DaemonError> {
    let hashes = plan
        .rows
        .iter()
        .filter_map(|recovery| recovery.row.brief_hash.as_deref())
        .collect::<BTreeSet<_>>();
    for hash in hashes {
        let path = brief::content_path(data_dir, hash)
            .map_err(|error| DaemonError::Invalid(error.to_string()))?;
        brief::read_verified(&path, hash)
            .map_err(|error| DaemonError::Invalid(error.to_string()))?;
    }
    Ok(())
}

fn restore_guardrail_parents(
    context: &mut Context,
    plan: &crate::recovery::RecoveryPlan,
) -> Result<(), DaemonError> {
    let mut child_counts = HashMap::<Uuid, u32>::new();
    for recovery in &plan.rows {
        if matches!(
            recovery.state,
            RecoveryRowState::Completed | RecoveryRowState::Deleted
        ) || recovery.guardrail_depth == 0
        {
            continue;
        }
        if let Some(parent) = recovery.row.parent_uuid {
            let count = child_counts.entry(parent).or_default();
            *count = count
                .checked_add(1)
                .ok_or_else(|| DaemonError::Invalid("recovered child count overflow".to_owned()))?;
        }
    }
    for recovery in &plan.rows {
        let task_uuid = recovery.row.uuid;
        let terminal = matches!(
            recovery.state,
            RecoveryRowState::Completed | RecoveryRowState::Deleted
        );
        let outstanding = child_counts.get(&task_uuid).copied().unwrap_or(0);
        if terminal && outstanding == 0 {
            continue;
        }
        context.guardrails.register_parent(
            task_uuid.to_string(),
            ParentInfo {
                parent_uuid: task_uuid.to_string(),
                depth: recovery.guardrail_depth,
                outstanding,
                no_enqueue: recovery.row.no_enqueue,
                terminal,
            },
        );
    }
    Ok(())
}

fn install_recovery_jobs(
    context: &mut Context,
    plan: &crate::recovery::RecoveryPlan,
    executor: &Executor,
) -> Result<Vec<Job>, DaemonError> {
    let rows = plan
        .rows
        .iter()
        .map(|row| (row.row.uuid, row))
        .collect::<BTreeMap<_, _>>();
    let mut child_counts = HashMap::<Uuid, u32>::new();
    for parent in plan
        .rows
        .iter()
        .filter(|row| {
            !matches!(
                row.state,
                RecoveryRowState::Completed | RecoveryRowState::Deleted
            )
        })
        .filter(|row| row.guardrail_depth > 0)
        .filter_map(|row| row.row.parent_uuid)
    {
        let children = child_counts.entry(parent).or_default();
        *children = children
            .checked_add(1)
            .ok_or_else(|| DaemonError::Invalid("recovered child count overflow".to_owned()))?;
    }
    let mut launches = Vec::new();
    let mut actions = plan.actions.iter().collect::<Vec<_>>();
    actions.sort_by_key(|action| match action {
        RecoveryAction::AdoptRunning { .. } => 0_u8,
        RecoveryAction::ReconcileExit { .. } => 1,
        RecoveryAction::QueueExisting { .. } | RecoveryAction::RePresent { .. } => 2,
        _ => 3,
    });
    let mut adapter_invocations = BTreeMap::new();
    for action in &actions {
        let Some((task_uuid, _, _)) = task_recovery_action(action) else {
            continue;
        };
        let recovery_row = rows
            .get(&task_uuid)
            .ok_or_else(|| DaemonError::Invalid(format!("recovery row {task_uuid} is absent")))?;
        if recovery_action_already_installed(context, &recovery_row.row)? {
            continue;
        }
        let rendered = recovery_adapter_invocation(
            &context.config,
            action,
            &recovery_row.row,
            executor,
            &context.paths.attestations_path(),
        )?;
        if adapter_invocations.insert(task_uuid, rendered).is_some() {
            return Err(DaemonError::Invalid(format!(
                "recovery task {task_uuid} has more than one executable action"
            )));
        }
    }
    for action in &actions {
        let Some((task_uuid, adopted, needs_lease)) = task_recovery_action(action) else {
            continue;
        };
        let recovery_row = rows
            .get(&task_uuid)
            .ok_or_else(|| DaemonError::Invalid(format!("recovery row {task_uuid} is absent")))?;
        if recovery_action_already_installed(context, &recovery_row.row)? {
            continue;
        }
        if needs_lease
            && !matches!(
                recovery_row.state,
                RecoveryRowState::Completed | RecoveryRowState::Deleted
            )
        {
            let job = Job {
                job_id: task_uuid,
                task_uuid: Some(task_uuid),
                row: recovery_row.row.clone(),
                invocation: adapter_invocations
                    .get(&task_uuid)
                    .expect("recovery invocation was rendered above")
                    .0
                    .clone(),
                labor_class: recovery_row.labor_class,
                state: JobState::Queued,
                lease_id: None,
                adopted,
                adopted_invocation_id: recovery_expected_invocation_id(action),
                model_is_advisory: false,
            };
            let unit = executor.unit_name(&job.identity());
            context
                .lease
                .engine()
                .validate_admission(&lease_request(&job, unit))?;
        }
    }
    for action in actions {
        let Some((task_uuid, adopted, needs_lease)) = task_recovery_action(action) else {
            continue;
        };
        let recovery_row = rows
            .get(&task_uuid)
            .ok_or_else(|| DaemonError::Invalid(format!("recovery row {task_uuid} is absent")))?;
        if recovery_action_already_installed(context, &recovery_row.row)? {
            continue;
        }
        if matches!(
            recovery_row.state,
            RecoveryRowState::Completed | RecoveryRowState::Deleted
        ) {
            continue;
        }
        let job_id = task_uuid;
        let stable = task_uuid.to_string();
        context
            .barriers
            .register_job(&stable, recovery_row.row.attempt);
        let (invocation, captures) = adapter_invocations
            .remove(&task_uuid)
            .expect("recovery invocation was rendered above");
        let mut row = recovery_row.row.clone();
        let mut model_is_advisory = recovered_model_is_advisory(&row, captures.as_ref(), adopted);
        if let Some(captures) = captures {
            if let Some(session_ref) = captures.session_ref()? {
                row.session_ref = Some(session_ref.to_owned());
            }
            if let Some(model) = captures.model()? {
                row.model = Some(model.to_owned());
                model_is_advisory = true;
            }
        }
        let mut job = Job {
            job_id,
            task_uuid: Some(task_uuid),
            row,
            invocation,
            labor_class: recovery_row.labor_class,
            state: JobState::Queued,
            lease_id: None,
            adopted,
            adopted_invocation_id: recovery_expected_invocation_id(action),
            model_is_advisory,
        };
        if needs_lease
            && !adopted
            && job.row.pools.iter().any(|pool| {
                context.paused_pools.contains(pool) || context.unreachable_pools.contains(pool)
            })
        {
            job.state = JobState::Paused;
            if job
                .row
                .pools
                .iter()
                .any(|pool| context.unreachable_pools.contains(pool))
            {
                context.unreachable_paused_jobs.insert(job_id);
            }
        } else if needs_lease {
            let unit = executor.unit_name(&job.identity());
            match context.lease.admit(lease_request(&job, unit), Utc::now()) {
                Ok(AdmitOutcome::Granted(grant)) => {
                    job.state = JobState::Running;
                    job.lease_id = Some(grant.lease_id.clone());
                    context.lease_jobs.insert(grant.lease_id, job_id);
                    launches.push(job.clone());
                }
                Ok(AdmitOutcome::Queued { ticket_id, .. }) => {
                    job.lease_id = Some(ticket_id.clone());
                    context.lease_jobs.insert(ticket_id, job_id);
                }
                Err(error) => {
                    eprintln!(
                        "tally: recovered job {task_uuid} is waiting for lease retry: {error}"
                    );
                }
            }
        } else {
            job.state = JobState::Running;
            launches.push(job.clone());
        }
        context.aliases.insert(stable, job_id);
        context.guardrails.register_parent(
            job_id.to_string(),
            ParentInfo {
                parent_uuid: task_uuid.to_string(),
                depth: recovery_row.guardrail_depth,
                outstanding: child_counts.get(&task_uuid).copied().unwrap_or(0),
                no_enqueue: job.row.no_enqueue,
                terminal: false,
            },
        );
        if let Some(row) = context.query_rows.get_mut(&task_uuid) {
            row.session_ref.clone_from(&job.row.session_ref);
            row.model.clone_from(&job.row.model);
        }
        if let Some(detail) = context.query_details.get_mut(&task_uuid) {
            detail.session_ref.clone_from(&job.row.session_ref);
            detail.observed_model.clone_from(&job.row.model);
            detail.attempt = job.row.attempt;
            detail.lease_epoch = job.row.lease_epoch;
            detail.labor_class = job.labor_class;
        }
        context.jobs.insert(job_id, job);
    }
    Ok(launches)
}

fn recovery_action_already_installed(
    context: &Context,
    candidate: &RowSeed,
) -> Result<bool, DaemonError> {
    let Some(existing) = context.jobs.get(&candidate.uuid) else {
        return Ok(false);
    };
    if existing.row.attempt < candidate.attempt {
        return Ok(false);
    }
    if existing.row.attempt > candidate.attempt {
        return Err(DaemonError::Invalid(format!(
            "recovery action for {} attempt {} is stale; attempt {} is already installed",
            candidate.uuid, candidate.attempt, existing.row.attempt
        )));
    }
    if existing.row.lease_epoch != candidate.lease_epoch
        || existing.row.pools != candidate.pools
        || existing.row.executor != candidate.executor
        || existing.row.adapter != candidate.adapter
        || existing.row.argv != candidate.argv
        || existing.row.dedup_key != candidate.dedup_key
        || existing.row.payload_hash != candidate.payload_hash
        || existing.row.cwd != candidate.cwd
        || existing.row.workspace != candidate.workspace
        || existing.row.adapter_options != candidate.adapter_options
        || existing.row.gate_manifest != candidate.gate_manifest
        || existing.row.resumed_from != candidate.resumed_from
    {
        return Err(DaemonError::Invalid(format!(
            "recovery action for {} attempt {} conflicts with the installed generation",
            candidate.uuid, candidate.attempt
        )));
    }
    Ok(true)
}

fn recovery_adapter_invocation(
    config: &Config,
    action: &RecoveryAction,
    row: &RowSeed,
    executor: &Executor,
    attestation_path: &Path,
) -> Result<(AdapterInvocation, Option<ScrapeResult>), DaemonError> {
    let engine = AdapterEngine::new(&config.adapters);
    match action {
        RecoveryAction::RePresent {
            previous_attempt,
            previous_lease_epoch,
            ..
        } => {
            repair_attestation_tail(attestation_path)?;
            let identity = ExecutionIdentity {
                job_id: row.uuid,
                task_uuid: Some(row.uuid),
            };
            let checkpoint = verified_adapter_attestation_captures(
                attestation_path,
                row.uuid,
                &row.adapter,
                *previous_attempt,
                *previous_lease_epoch,
            )?;
            let captures = if let Some(checkpoint) = checkpoint {
                checkpoint
            } else {
                match executor.capture_generation_matches(
                    &identity,
                    *previous_attempt,
                    *previous_lease_epoch,
                ) {
                    Ok(true) => {
                        let captures =
                            engine.scrape_paths(&row.adapter, &executor.paths(&identity))?;
                        ensure_verified_resume_attestation(
                            attestation_path,
                            row,
                            *previous_attempt,
                            *previous_lease_epoch,
                            &captures,
                        )?;
                        captures
                    }
                    Ok(false) | Err(_) => {
                        return Err(DaemonError::Invalid(format!(
                        "retained capture generation for {} does not match prior attempt {} at lease epoch {}, and no verified adapter scrape attestation can resume it",
                        row.uuid, previous_attempt, previous_lease_epoch
                    )));
                    }
                }
            };
            let invocation = engine.resume_with_options(
                &row.adapter,
                &row.argv,
                &captures,
                &row.adapter_options,
                row.cwd.as_deref(),
            )?;
            Ok((invocation, Some(captures)))
        }
        RecoveryAction::QueueExisting { .. } => {
            if row.resumed_from.is_some() {
                let session_ref = row.session_ref.clone().ok_or_else(|| {
                    DaemonError::Invalid(format!(
                        "continued row {} omitted its durable session reference",
                        row.uuid
                    ))
                })?;
                let mut captures =
                    BTreeMap::from([("sessionRef".to_owned(), Value::String(session_ref))]);
                if let Some(model) = &row.model {
                    captures.insert("model".to_owned(), Value::String(model.clone()));
                }
                let captures = ScrapeResult { captures };
                Ok((
                    engine.resume_with_options(
                        &row.adapter,
                        &row.argv,
                        &captures,
                        &row.adapter_options,
                        row.cwd.as_deref(),
                    )?,
                    Some(captures),
                ))
            } else {
                Ok((
                    engine.launch_with_options(
                        &row.adapter,
                        &row.argv,
                        &row.adapter_options,
                        row.cwd.as_deref(),
                    )?,
                    None,
                ))
            }
        }
        RecoveryAction::AdoptRunning { .. } | RecoveryAction::ReconcileExit { .. } => Ok((
            AdapterInvocation {
                argv: row.argv.clone(),
                env: BTreeMap::new(),
                hardening: engine.adapter(&row.adapter)?.hardening,
                yield_hook: None,
            },
            None,
        )),
        _ => Err(DaemonError::Invalid(
            "non-executable recovery action reached adapter rendering".to_owned(),
        )),
    }
}

fn task_recovery_action(action: &RecoveryAction) -> Option<(Uuid, bool, bool)> {
    match action {
        RecoveryAction::QueueExisting { task_uuid, .. } => Some((*task_uuid, false, true)),
        RecoveryAction::RePresent { row, .. } => Some((row.uuid, false, true)),
        RecoveryAction::AdoptRunning {
            identity: RecoveryIdentity::Task(uuid),
            ..
        } => Some((*uuid, true, true)),
        RecoveryAction::ReconcileExit {
            identity: RecoveryIdentity::Task(uuid),
            ..
        } => Some((*uuid, true, false)),
        _ => None,
    }
}

fn recovery_expected_invocation_id(action: &RecoveryAction) -> Option<String> {
    match action {
        RecoveryAction::AdoptRunning { invocation_id, .. } => Some(invocation_id.clone()),
        RecoveryAction::ReconcileExit { record, .. } => Some(record.invocation_id.clone()),
        _ => None,
    }
}

async fn watchdog_tick(interval: &mut Option<tokio::time::Interval>) {
    if let Some(interval) = interval {
        interval.tick().await;
    } else {
        std::future::pending::<()>().await;
    }
}

#[derive(Debug, Clone)]
pub struct SystemdNotifier {
    socket: Option<PathBuf>,
    watchdog: Option<Duration>,
}

impl SystemdNotifier {
    pub fn from_environment() -> Result<Self, DaemonError> {
        let socket = std::env::var_os("NOTIFY_SOCKET").map(PathBuf::from);
        let watchdog = match std::env::var("WATCHDOG_USEC") {
            Ok(value) => {
                if let Ok(pid) = std::env::var("WATCHDOG_PID") {
                    let pid = pid
                        .parse::<u32>()
                        .map_err(|_| DaemonError::Notify("WATCHDOG_PID is invalid".to_owned()))?;
                    if pid != std::process::id() {
                        None
                    } else {
                        Some(parse_watchdog(&value)?)
                    }
                } else {
                    Some(parse_watchdog(&value)?)
                }
            }
            Err(std::env::VarError::NotPresent) => None,
            Err(error) => return Err(DaemonError::Notify(error.to_string())),
        };
        Ok(Self { socket, watchdog })
    }

    pub fn with_socket(socket: PathBuf, watchdog: Option<Duration>) -> Self {
        Self {
            socket: Some(socket),
            watchdog,
        }
    }

    fn send(&self, payload: &str) -> Result<(), DaemonError> {
        let Some(socket) = &self.socket else {
            return Ok(());
        };
        if socket.as_os_str().as_encoded_bytes().starts_with(b"@") {
            return send_abstract_notify(socket, payload.as_bytes());
        }
        let datagram =
            UnixDatagram::unbound().map_err(|error| DaemonError::Notify(error.to_string()))?;
        datagram
            .send_to(payload.as_bytes(), socket)
            .map_err(|error| DaemonError::Notify(error.to_string()))?;
        Ok(())
    }

    pub fn ready(&self) -> Result<(), DaemonError> {
        self.send("READY=1\nSTATUS=tally daemon ready")
    }

    pub fn watchdog(&self) -> Result<(), DaemonError> {
        self.send("WATCHDOG=1")
    }

    pub fn stopping(&self) -> Result<(), DaemonError> {
        self.send("STOPPING=1")
    }

    fn watchdog_interval(&self) -> Option<tokio::time::Interval> {
        self.watchdog.map(|duration| {
            let cadence = duration.checked_div(2).unwrap_or(Duration::from_micros(1));
            let mut interval = tokio::time::interval(cadence.max(Duration::from_micros(1)));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval
        })
    }
}

fn parse_watchdog(value: &str) -> Result<Duration, DaemonError> {
    let micros = value
        .parse::<u64>()
        .map_err(|_| DaemonError::Notify("WATCHDOG_USEC is invalid".to_owned()))?;
    if micros == 0 {
        return Err(DaemonError::Notify(
            "WATCHDOG_USEC must be positive".to_owned(),
        ));
    }
    Ok(Duration::from_micros(micros))
}

fn send_abstract_notify(socket: &Path, payload: &[u8]) -> Result<(), DaemonError> {
    use std::mem::size_of;
    use std::os::fd::RawFd;
    use std::os::unix::ffi::OsStrExt;

    let bytes = socket.as_os_str().as_bytes();
    let name = bytes
        .strip_prefix(b"@")
        .ok_or_else(|| DaemonError::Notify("abstract notify path is invalid".to_owned()))?;
    if name.is_empty() || name.len() >= 108 {
        return Err(DaemonError::Notify(
            "abstract notify path length is invalid".to_owned(),
        ));
    }
    let fd: RawFd =
        unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(DaemonError::Notify(io::Error::last_os_error().to_string()));
    }
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (index, byte) in name.iter().enumerate() {
        address.sun_path[index + 1] = *byte as libc::c_char;
    }
    let length = (size_of::<libc::sa_family_t>() + 1 + name.len()) as libc::socklen_t;
    let sent = unsafe {
        libc::sendto(
            fd,
            payload.as_ptr().cast(),
            payload.len(),
            0,
            (&address as *const libc::sockaddr_un).cast(),
            length,
        )
    };
    let error = (sent < 0).then(io::Error::last_os_error);
    unsafe {
        libc::close(fd);
    }
    if let Some(error) = error {
        return Err(DaemonError::Notify(error.to_string()));
    }
    Ok(())
}

pub type SupervisedFuture = Pin<Box<dyn Future<Output = Result<(), String>>>>;
pub type SupervisedFactory = Rc<dyn Fn() -> SupervisedFuture>;

#[derive(Clone)]
pub struct SupervisedTask {
    pub name: String,
    pub restart_delay: Duration,
    pub factory: SupervisedFactory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisionEvent {
    Started {
        name: String,
        generation: u64,
    },
    Restarting {
        name: String,
        generation: u64,
        reason: String,
    },
}

pub fn spawn_supervised(
    task: SupervisedTask,
    mut shutdown: watch::Receiver<bool>,
    events: mpsc::UnboundedSender<SupervisionEvent>,
) -> JoinHandle<()> {
    tokio::task::spawn_local(async move {
        let mut generation = 0_u64;
        loop {
            if *shutdown.borrow() {
                break;
            }
            generation = generation.saturating_add(1);
            let _ = events.send(SupervisionEvent::Started {
                name: task.name.clone(),
                generation,
            });
            let mut child = tokio::task::spawn_local((task.factory)());
            let reason = tokio::select! {
                result = &mut child => match result {
                    Ok(Ok(())) => "producer exited".to_owned(),
                    Ok(Err(error)) => error,
                    Err(error) if error.is_panic() => "producer panicked".to_owned(),
                    Err(error) => format!("producer join failed: {error}"),
                },
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        child.abort();
                        let _ = child.await;
                        break;
                    }
                    continue;
                }
            };
            let _ = events.send(SupervisionEvent::Restarting {
                name: task.name.clone(),
                generation,
                reason,
            });
            tokio::select! {
                _ = tokio::time::sleep(task.restart_delay) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use tempfile::tempdir;

    use super::*;
    use crate::adapters::{
        AdapterConfig, AdapterTrace, ScrapeCapture, ScrapeMode, ScrapeStream, TraceFraming,
    };
    use crate::config::{
        CoResidencyPredicate, ExecutionTargetConfig, JournaldConfig, MeterBudgetClass, PoolConfig,
        PoolPredicate, ResourceKind, SshExecutorConfig, UsageMeterConfig,
    };
    use crate::evidence::{hash_artifact_file, RetryPolicy};
    use crate::executor::{
        read_exit_record, write_exit_record, ExecutionPaths, LocalUnitFact, LocalUnitProbe,
        LocalUnitState, RemoteCapture, RemoteCompletion, RemoteExecutorReply,
        RemoteExecutorRequest, RemoteExecutorResult, RemoteTransport, RemoteTransportError,
        UnitExitRecord, REMOTE_EXECUTOR_PROTOCOL_VERSION, UNIT_EXIT_SCHEMA_VERSION,
    };
    use crate::producers::{
        EmitOutcome, GhCliIntake, GhObservation, ProducerConfig, ProducerEngine,
        ReachabilityTransition,
    };
    use crate::recovery::RecoveryPlan;
    use crate::taskdb::{
        GhContextSnapshot, GhItemState, GhItemType, GhOrigin, GH_CONTEXT_SCHEMA_VERSION,
        GH_ORIGIN_SCHEMA_VERSION,
    };
    use tally_client::RpcClient;

    struct ExitFileProbe;

    impl LocalUnitProbe for ExitFileProbe {
        fn inspect(
            &self,
            unit: &str,
            paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            if !paths.exit_record.exists() {
                return Ok(LocalUnitFact::absent(unit));
            }
            let record = read_exit_record(&paths.exit_record, unit)?;
            Ok(LocalUnitFact {
                unit: unit.to_owned(),
                loaded: false,
                state: LocalUnitState::Exited,
                invocation_id: Some(record.invocation_id.clone()),
                attempt: Some(record.attempt),
                lease_epoch: Some(record.lease_epoch),
                exit_record: Some(record),
            })
        }
    }

    struct RunningProbe {
        attempt: u32,
        lease_epoch: u64,
    }

    impl LocalUnitProbe for RunningProbe {
        fn inspect(
            &self,
            unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            Ok(LocalUnitFact {
                unit: unit.to_owned(),
                loaded: true,
                state: LocalUnitState::Running,
                invocation_id: Some("restart-invocation".to_owned()),
                attempt: Some(self.attempt),
                lease_epoch: Some(self.lease_epoch),
                exit_record: None,
            })
        }
    }

    struct IntentObservingProbe {
        path: PathBuf,
        task_uuid: Uuid,
        inspections: Arc<AtomicUsize>,
    }

    impl LocalUnitProbe for IntentObservingProbe {
        fn inspect(
            &self,
            unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            let intent =
                read_pool_loss_intent(&self.path).map_err(|error| ExecutorError::UnitProbe {
                    unit: unit.to_owned(),
                    detail: format!("pool-loss intent was not durable before reclaim: {error}"),
                })?;
            if intent.row.uuid != self.task_uuid {
                return Err(ExecutorError::UnitProbe {
                    unit: unit.to_owned(),
                    detail: "pool-loss intent names the wrong task generation".to_owned(),
                });
            }
            self.inspections.fetch_add(1, Ordering::SeqCst);
            Ok(LocalUnitFact::absent(unit))
        }
    }

    struct StallingCommitter {
        started: Option<oneshot::Sender<()>>,
        release: Arc<AtomicBool>,
    }

    impl ReplicaCommitter for StallingCommitter {
        fn commit<'a>(
            &'a mut self,
            _command: CommitCommand,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + 'a>> {
            let started = self.started.take();
            let release = self.release.clone();
            Box::pin(async move {
                if let Some(started) = started {
                    let _ = started.send(());
                }
                while !release.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            })
        }
    }

    fn one_pool_config() -> Config {
        Config {
            pools: BTreeMap::from([(
                "slot".to_owned(),
                PoolConfig {
                    resource: ResourceKind::BuildSlot,
                    predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                    ..PoolConfig::default()
                },
            )]),
            enqueue: Default::default(),
            lease: Default::default(),
            adapters: BTreeMap::from([("shell".to_owned(), AdapterConfig::default())]),
            producers: BTreeMap::new(),
            executors: BTreeMap::new(),
            journald: JournaldConfig { native: false },
            ..Config::default()
        }
    }

    fn two_pool_config() -> Config {
        let mut config = one_pool_config();
        config.pools.insert(
            "zeta".to_owned(),
            PoolConfig {
                resource: ResourceKind::BuildSlot,
                predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                ..PoolConfig::default()
            },
        );
        config
    }

    fn window_pool_config() -> Config {
        Config {
            pools: BTreeMap::from([(
                "api".to_owned(),
                PoolConfig {
                    resource: ResourceKind::Budget,
                    predicate: PoolPredicate::WindowedConsumption(
                        crate::config::WindowedConsumptionPredicate {
                            window_sec: 60,
                            consumption_cap: 100,
                        },
                    ),
                    ..PoolConfig::default()
                },
            )]),
            enqueue: Default::default(),
            lease: Default::default(),
            adapters: BTreeMap::from([("shell".to_owned(), AdapterConfig::default())]),
            producers: BTreeMap::new(),
            executors: BTreeMap::new(),
            journald: JournaldConfig { native: false },
            ..Config::default()
        }
    }

    fn hard_preempt_config() -> Config {
        let mut config = one_pool_config();
        config.pools.get_mut("slot").unwrap().hard_preempt = true;
        config
    }

    fn remote_config() -> Config {
        let mut config = one_pool_config();
        config.executors.insert(
            "worker".to_owned(),
            ExecutionTargetConfig::Ssh(SshExecutorConfig {
                host: "worker.example".to_owned(),
                user: "tally-worker".to_owned(),
                port: 22,
                ssh_program: PathBuf::from("/run/current-system/sw/bin/ssh"),
                identity_file: PathBuf::from("/run/credentials/tally-worker-key"),
                known_hosts_file: PathBuf::from("/etc/tally/worker-known-hosts"),
                program: PathBuf::from("/run/current-system/sw/bin/tally"),
                state_dir: PathBuf::from("/var/lib/tally-remote"),
                connect_timeout_sec: 3,
                server_alive_interval_sec: 2,
                server_alive_count_max: 2,
                retry_interval_ms: 10,
            }),
        );
        config
    }

    #[derive(Clone)]
    struct RecoveringRemoteTransport {
        calls: Arc<AtomicUsize>,
        release: Arc<AtomicBool>,
    }

    impl RemoteTransport for RecoveringRemoteTransport {
        fn call<'a>(
            &'a self,
            _config: &'a SshExecutorConfig,
            request: RemoteExecutorRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>,
        > {
            let calls = self.calls.clone();
            let release = self.release.clone();
            Box::pin(async move {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    return Err(RemoteTransportError {
                        detail: "simulated SSH interruption after launch".to_owned(),
                    });
                }
                while !release.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                let RemoteExecutorRequest::Ensure {
                    request, evidence, ..
                } = request
                else {
                    return Err(RemoteTransportError {
                        detail: "unexpected remote operation".to_owned(),
                    });
                };
                let unit = format!("tally-job-{}.service", request.identity.unit_uuid());
                let evidence =
                    parse_evidence_specs(&evidence).map_err(|error| RemoteTransportError {
                        detail: error.to_string(),
                    })?;
                Ok(RemoteExecutorReply::Ok {
                    protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                    result: Box::new(RemoteExecutorResult::Completion(Box::new(
                        RemoteCompletion {
                            unit: unit.clone(),
                            record: UnitExitRecord {
                                schema_version: UNIT_EXIT_SCHEMA_VERSION,
                                unit,
                                invocation_id: "remote-long-job".to_owned(),
                                attempt: request.attempt,
                                lease_epoch: request.lease_epoch,
                                service_result: "success".to_owned(),
                                exit_code: Some("exited".to_owned()),
                                exit_status: Some("0".to_owned()),
                            },
                            termination: ExecutionTermination::Exited(0),
                            capture: RemoteCapture {
                                attempt: request.attempt,
                                lease_epoch: request.lease_epoch,
                                stdout_base64: Some(String::new()),
                                stderr_base64: Some(String::new()),
                                error: None,
                            },
                            evidence_gate: Some(run_evidence_gate(RunOutcome {
                                exit_code: 0,
                                wall_clock_seconds: 1.0,
                                evidence: &evidence,
                            })),
                            semantic_completion: None,
                        },
                    ))),
                })
            })
        }
    }

    #[derive(Clone)]
    struct RestartRemoteTransport {
        calls: Arc<std::sync::Mutex<Vec<RemoteExecutorRequest>>>,
        attempt: u32,
        lease_epoch: u64,
    }

    impl RemoteTransport for RestartRemoteTransport {
        fn call<'a>(
            &'a self,
            _config: &'a SshExecutorConfig,
            request: RemoteExecutorRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>,
        > {
            let calls = self.calls.clone();
            let attempt = self.attempt;
            let lease_epoch = self.lease_epoch;
            Box::pin(async move {
                calls.lock().unwrap().push(request.clone());
                let result = match request {
                    RemoteExecutorRequest::Probe { identity, .. } => {
                        let unit = format!("tally-job-{}.service", identity.unit_uuid());
                        RemoteExecutorResult::Fact(LocalUnitFact {
                            unit,
                            loaded: true,
                            state: LocalUnitState::Running,
                            invocation_id: Some("restart-remote-invocation".to_owned()),
                            attempt: Some(attempt),
                            lease_epoch: Some(lease_epoch),
                            exit_record: None,
                        })
                    }
                    RemoteExecutorRequest::Adopt {
                        request,
                        expected_invocation_id,
                        evidence,
                        ..
                    } => {
                        if expected_invocation_id != "restart-remote-invocation" {
                            return Ok(RemoteExecutorReply::Error {
                                protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                                message: "unexpected adoption identity".to_owned(),
                            });
                        }
                        let evidence = parse_evidence_specs(&evidence).map_err(|error| {
                            RemoteTransportError {
                                detail: error.to_string(),
                            }
                        })?;
                        let unit = format!("tally-job-{}.service", request.identity.unit_uuid());
                        RemoteExecutorResult::Completion(Box::new(RemoteCompletion {
                            unit: unit.clone(),
                            record: UnitExitRecord {
                                schema_version: UNIT_EXIT_SCHEMA_VERSION,
                                unit,
                                invocation_id: expected_invocation_id,
                                attempt: request.attempt,
                                lease_epoch: request.lease_epoch,
                                service_result: "success".to_owned(),
                                exit_code: Some("exited".to_owned()),
                                exit_status: Some("0".to_owned()),
                            },
                            termination: ExecutionTermination::Exited(0),
                            capture: RemoteCapture {
                                attempt: request.attempt,
                                lease_epoch: request.lease_epoch,
                                stdout_base64: Some(String::new()),
                                stderr_base64: Some(String::new()),
                                error: None,
                            },
                            evidence_gate: Some(run_evidence_gate(RunOutcome {
                                exit_code: 0,
                                wall_clock_seconds: 1.0,
                                evidence: &evidence,
                            })),
                            semantic_completion: None,
                        }))
                    }
                    RemoteExecutorRequest::Ensure { .. } => {
                        return Ok(RemoteExecutorReply::Error {
                            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                            message: "restart attempted a duplicate launch".to_owned(),
                        });
                    }
                    RemoteExecutorRequest::Reclaim { .. } => {
                        return Ok(RemoteExecutorReply::Error {
                            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                            message: "unexpected reclaim".to_owned(),
                        });
                    }
                };
                Ok(RemoteExecutorReply::Ok {
                    protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                    result: Box::new(result),
                })
            })
        }
    }

    fn structured_adapter(program: &Path) -> AdapterConfig {
        AdapterConfig {
            argv: vec![
                program.to_string_lossy().into_owned(),
                "--structured".to_owned(),
            ],
            resume: Some(vec![
                program.to_string_lossy().into_owned(),
                "--resume".to_owned(),
                "%<sessionRef>%".to_owned(),
                "--model".to_owned(),
                "%<model>%".to_owned(),
            ]),
            scrape: BTreeMap::from([
                (
                    "branch".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stderr,
                        mode: ScrapeMode::Regex,
                        pattern: "(?m)^branch=(.+)$".to_owned(),
                    },
                ),
                (
                    "model".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..model".to_owned(),
                    },
                ),
                (
                    "sessionRef".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..session_id".to_owned(),
                    },
                ),
                (
                    "usage".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..usage".to_owned(),
                    },
                ),
                (
                    "finalMessage".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..final_message".to_owned(),
                    },
                ),
            ]),
            trace: None,
            yield_hook: Some(vec![
                "tally".to_owned(),
                "lease".to_owned(),
                "status".to_owned(),
            ]),
            env: BTreeMap::from([("CUSTOM_AGENT_MODE".to_owned(), "batch".to_owned())]),
            launch: crate::adapters::AdapterLaunchConfig::default(),
            hardening: Default::default(),
            extra_config: BTreeMap::from([(
                "modelFlag".to_owned(),
                Value::String("--model".to_owned()),
            )]),
        }
    }

    fn settings() -> DaemonSettings {
        DaemonSettings {
            unit_limits: UnitLimits {
                cpu_weight: 100,
                memory_max_bytes: 64 * 1024 * 1024,
            },
            yield_grace: Duration::from_secs(1),
            recovery_policy: RecoveryPolicy {
                retry: RetryPolicy {
                    auto_pool_return: false,
                    auto_resource_return: false,
                    auto_bounded_requeue: false,
                },
                max_attempts: 1,
            },
        }
    }

    fn fs1_paths(root: &Path) -> DaemonPaths {
        DaemonPaths {
            socket: root.join("run/tally.sock"),
            state_dir: root.join("state"),
            data_dir: root.join("data"),
        }
    }

    async fn fs1_daemon(paths: &DaemonPaths) -> Daemon {
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
            .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);
        Daemon::open_with_executor(one_pool_config(), paths.clone(), settings(), executor)
            .await
            .unwrap()
    }

    fn fs1_full_payload(
        dedup_key: &str,
        argv: &[&str],
        evidence: impl IntoIterator<Item = String>,
    ) -> Value {
        json!({
            "argv": argv,
            "pool": "slot",
            "priority": "high",
            "adapter": "shell",
            "source": "manual",
            "dedupKey": dedup_key,
            "submission": {"mode": "full"},
            "evidence": evidence.into_iter().collect::<Vec<_>>(),
        })
    }

    async fn fs1_wait(client: &RpcClient, response: &Value) -> Value {
        client
            .call(
                "queue.await_job",
                Some(json!({"task_uuid": response["task_uuid"]})),
            )
            .await
            .unwrap()
    }

    fn fs1_conflict(error: WireIoError) -> Value {
        match error {
            WireIoError::Rpc(WireErrorCode::DedupKeyConflict, _, Some(data)) => data,
            other => panic!("expected dedup-key-conflict, got {other:?}"),
        }
    }

    fn durable_row(uuid: Uuid, dedup_key: &str, lease_epoch: u64) -> RowSeed {
        RowSeed {
            uuid,
            description: "durable reuse fixture".to_owned(),
            priority: Priority::High,
            source: EnqueueSource::Manual,
            adapter: "shell".to_owned(),
            pools: vec!["slot".to_owned()],
            executor: None,
            model: None,
            cwd: None,
            workspace: None,
            adapter_options: Default::default(),
            gate_manifest: None,
            resumed_from: None,
            dedup_key: Some(dedup_key.to_owned()),
            payload_hash: None,
            brief_hash: None,
            orchestration: None,
            session_ref: None,
            final_message: None,
            lease_epoch,
            attempt: 1,
            argv: vec!["true".to_owned()],
            evidence: vec!["exit:0".to_owned()],
            parent_uuid: None,
            consumption_estimate: None,
            runtime_max_sec: None,
            no_enqueue: false,
            credentials: BTreeMap::new(),
            origin: None,
            gh_origin: None,
            related_trigger: None,
            evidence_class: None,
            manifest_hash: None,
        }
    }

    fn append_history_event(
        store: &mut LifecycleStore,
        row: &RowSeed,
        event: TallyEvent,
        attempt: u32,
        lease_epoch: u64,
        realtime_us: u64,
    ) {
        let terminal = matches!(event, TallyEvent::Completed | TallyEvent::Failed);
        let fields = EmitEvent {
            event,
            task_uuid: row.uuid.to_string(),
            class: row.priority,
            source: row.source,
            message: Some(format!("fixture {event} attempt={attempt}")),
            agent: Some(row.adapter.clone()),
            session_ref: row.session_ref.clone(),
            unit: Some(format!("tally-job-{}.service", row.uuid)),
            exit_code: terminal.then_some(if event == TallyEvent::Completed { 0 } else { 1 }),
            gpu_seconds: terminal.then_some(0.0),
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
            job_id: Some(row.uuid.to_string()),
            parent: row.parent_uuid.map(|uuid| uuid.to_string()),
            pools: Some(row.pools.clone()),
            executor: row.executor.clone(),
        }
        .into_fields()
        .unwrap();
        store.append_at(fields, realtime_us).unwrap();
    }

    fn append_fixture_witness(
        ledger: &mut WitnessLedger,
        row: &RowSeed,
        timestamp: &str,
        verdict: Verdict,
        exit_code: i32,
        attempt: u32,
        lease_epoch: u64,
    ) -> WitnessRecord {
        ledger
            .append(WitnessBody {
                task_uuid: Some(row.uuid.to_string()),
                transition_timestamp: timestamp.to_owned(),
                verdict,
                exit_code,
                artifact_content_hash: (verdict == Verdict::Pass)
                    .then(|| format!("sha256:{}", "a".repeat(64))),
                gpu_seconds: Some(f64::from(attempt)),
                wall_clock: 10.0 + f64::from(attempt),
                attempt,
                lease_epoch,
                dedup_key: row.dedup_key.clone(),
                payload_hash: row.payload_hash.clone(),
                brief_hash: row.brief_hash.clone(),
                orchestration: row.orchestration.clone(),
                labor_class: if attempt == 1 {
                    LaborClass::Fresh
                } else {
                    LaborClass::Recovered
                },
                trace_ref: None,
                pools: Some(row.pools.clone()),
                executor: row.executor.clone(),
                charge: None,
                model: None,
                evidence_class: Some(json!({"fixture": "acceptance-24"})),
                manifest_hash: Some(json!("sha256:fixture-manifest")),
                completion: None,
            })
            .unwrap()
    }

    fn seed_durable_query_fixture(
        root: &Path,
    ) -> (DaemonPaths, Uuid, Uuid, WitnessRecord, WitnessRecord) {
        let paths = DaemonPaths {
            socket: root.join("run/tally.sock"),
            state_dir: root.join("state"),
            data_dir: root.join("data"),
        };
        prepare_paths(&paths).unwrap();
        // Simulate the epochs owned by the two recorded attempts. The daemon
        // opened by the acceptance test is therefore a later restart.
        assert_eq!(bump_epoch(&paths.state_dir).unwrap(), 1);
        assert_eq!(bump_epoch(&paths.state_dir).unwrap(), 2);

        let parent_uuid = Uuid::new_v4();
        let child_uuid = Uuid::new_v4();
        let mut parent = durable_row(parent_uuid, "acceptance-parent", 1);
        parent.description = "acceptance parent".to_owned();
        let mut child = durable_row(child_uuid, "acceptance-child", 1);
        child.description = "acceptance child".to_owned();
        child.parent_uuid = Some(parent_uuid);
        write_enqueue_event_atomic(
            &paths.events_dir(),
            &DurableEnqueueEvent::new(parent.clone()).unwrap(),
        )
        .unwrap();
        write_enqueue_event_atomic(
            &paths.events_dir(),
            &DurableEnqueueEvent::new(child.clone()).unwrap(),
        )
        .unwrap();

        let mut history = LifecycleStore::open(&paths.data_dir).unwrap();
        let mut timestamp = 1_786_000_000_000_000_u64;
        for event in [
            TallyEvent::Enqueued,
            TallyEvent::Dispatched,
            TallyEvent::Started,
            TallyEvent::EvidenceFail,
            TallyEvent::Preempted,
        ] {
            append_history_event(&mut history, &parent, event, 1, 1, timestamp);
            timestamp += 1;
        }
        for event in [
            TallyEvent::Resumed,
            TallyEvent::Dispatched,
            TallyEvent::Started,
            TallyEvent::EvidencePass,
            TallyEvent::Completed,
        ] {
            append_history_event(&mut history, &parent, event, 2, 2, timestamp);
            timestamp += 1;
        }
        for event in [
            TallyEvent::Enqueued,
            TallyEvent::Dispatched,
            TallyEvent::Started,
            TallyEvent::EvidencePass,
            TallyEvent::Completed,
        ] {
            append_history_event(&mut history, &child, event, 1, 1, timestamp);
            timestamp += 1;
        }
        drop(history);

        let mut ledger = WitnessLedger::open(paths.witness_path()).unwrap();
        append_fixture_witness(
            &mut ledger,
            &parent,
            "2026-08-05T12:00:00.000Z",
            Verdict::Preempted,
            1,
            1,
            1,
        );
        let parent_pass = append_fixture_witness(
            &mut ledger,
            &parent,
            "2026-08-05T12:01:00.000Z",
            Verdict::Pass,
            0,
            2,
            2,
        );
        let chain_head = append_fixture_witness(
            &mut ledger,
            &child,
            "2026-08-05T12:02:00.000Z",
            Verdict::Pass,
            0,
            1,
            1,
        );
        drop(ledger);
        append_attestation(
            &paths.attestations_path(),
            json!({
                "kind": "adapter-scrape",
                "taskUuid": parent_uuid.to_string(),
                "jobId": parent_uuid.to_string(),
                "adapter": "shell",
                "attempt": 2,
                "leaseEpoch": 2,
                "captures": {"sessionRef": "advisory-session"},
                "usageAuthority": "advisory-only",
            }),
        )
        .unwrap();
        (paths, parent_uuid, child_uuid, parent_pass, chain_head)
    }

    fn gh_test_observation(node_id: &str, item_type: GhItemType) -> GhObservation {
        GhObservation {
            source: "notifications".to_owned(),
            repo: "acme/widgets".to_owned(),
            number: 42,
            html_url: match item_type {
                GhItemType::Issue => "https://github.com/acme/widgets/issues/42",
                GhItemType::PullRequest => "https://github.com/acme/widgets/pull/42",
            }
            .to_owned(),
            item_type,
            head_sha: (item_type == GhItemType::PullRequest)
                .then(|| "4242424242424242424242424242424242424242".to_owned()),
            node_id: node_id.to_owned(),
            item_author: "issue-author".to_owned(),
            trigger_actor: "contributor".to_owned(),
            self_actor: "tally-bot".to_owned(),
            notification_reason: Some("mention".to_owned()),
            trigger_kind: "assignment".to_owned(),
            event_id: Some("event-42".to_owned()),
            comment_id: None,
            trigger_timestamp: "2026-07-20T12:30:00Z".to_owned(),
            trigger_value: Some("tally-bot".to_owned()),
            context: GhContextSnapshot {
                schema_version: GH_CONTEXT_SCHEMA_VERSION,
                title: "Origin fixture".to_owned(),
                body: "untrusted body".to_owned(),
                state: Some(GhItemState::Open),
                head_sha: (item_type == GhItemType::PullRequest)
                    .then(|| "4242424242424242424242424242424242424242".to_owned()),
                labels: vec!["build".to_owned()],
                assignees: Vec::new(),
                triggering_comment: None,
            },
        }
    }

    fn gh_test_origin(node_id: &str, item_type: GhItemType) -> GhOrigin {
        let observation = gh_test_observation(node_id, item_type);
        GhOrigin {
            schema_version: GH_ORIGIN_SCHEMA_VERSION,
            producer: "github".to_owned(),
            source: observation.source,
            repo: observation.repo,
            number: observation.number,
            html_url: observation.html_url,
            item_type: Some(observation.item_type),
            head_sha: observation.head_sha,
            node_id: observation.node_id,
            item_author: observation.item_author,
            trigger_actor: observation.trigger_actor,
            self_actor: observation.self_actor,
            notification_reason: observation.notification_reason,
            trigger_kind: observation.trigger_kind,
            event_id: observation.event_id,
            comment_id: observation.comment_id,
            trigger_timestamp: Some(observation.trigger_timestamp),
            trigger_value: observation.trigger_value,
            context: Some(observation.context),
            actor_exclude: "self".to_owned(),
            allow_self_triggered: false,
            allowed_actors: Vec::new(),
        }
    }

    fn empty_plan() -> RecoveryPlan {
        RecoveryPlan {
            witness_lsn: 0,
            rows: Vec::new(),
            actions: Vec::new(),
            lease_epoch_fences: Vec::new(),
            advisory_return_attestations: Vec::new(),
        }
    }

    #[test]
    fn fsync_barrier_is_closed_over_exactly_three_stages() {
        assert_eq!(
            FSYNC_BEFORE_ACK_STAGES,
            &[
                AckStage::Admission,
                AckStage::LeaseGrant,
                AckStage::VerdictWitness
            ]
        );
    }

    #[test]
    fn no_gate_manifest_leaves_every_evidence_verdict_unchanged() {
        for verdict in [
            Verdict::Pass,
            Verdict::CleanExitNoArtifact,
            Verdict::Failed,
            Verdict::Cancelled,
            Verdict::Reused,
            Verdict::PoolVanished,
            Verdict::Preempted,
            Verdict::RuntimeExceeded,
        ] {
            assert_eq!(canonical_verdict(verdict, None), verdict);
        }
    }

    #[test]
    fn terminal_witness_beats_a_stale_live_query_snapshot() {
        let projection = |witness_seq: Option<u64>| JobProjection {
            anchor: "job-1".to_owned(),
            task_uuid: Some("job-1".to_owned()),
            description: None,
            argv: None,
            brief_hash: None,
            orchestration: None,
            pools: Some(vec!["slot".to_owned()]),
            executor: None,
            source: Some("manual".to_owned()),
            session_ref: None,
            final_message: None,
            cwd: None,
            workspace: None,
            resumed_from: None,
            model: None,
            gh_origin: None,
            state: "completed".to_owned(),
            verdict: witness_seq.map(|_| Verdict::Pass),
            gpu_seconds: None,
            canonical_gpu_seconds: None,
            last_event_at: None,
            witness_seq,
            completion: None,
        };
        let live = HashMap::from([("job-1".to_owned(), "running".to_owned())]);
        let mut terminal = vec![projection(Some(7))];
        overlay_live_states(&mut terminal, &live);
        assert_eq!(terminal[0].state, "completed");
        assert_eq!(terminal[0].verdict, Some(Verdict::Pass));

        let mut unwitnessed = vec![projection(None)];
        overlay_live_states(&mut unwitnessed, &live);
        assert_eq!(unwitnessed[0].state, "running");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn events_ingress_uses_the_identical_enqueue_narrower_and_repairs_archive_gap() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut config = one_pool_config();
                config.pools.get_mut("slot").unwrap().credentials.insert(
                    "pool-token".to_owned(),
                    PathBuf::from("/run/credentials/pool-token"),
                );
                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();

                let direct_payload = json!({
                    "argv": ["same-narrower", "literal arg"],
                    "pool": "slot",
                    "priority": "high",
                    "adapter": "shell",
                    "source": "events-dir",
                    "dedupKey": "direct",
                    "evidence": ["exit:0"],
                    "evidenceClass": {
                        "arbitrary": [true, 7, {"nested": null}]
                    },
                    "manifestHash": "deliberately-not-validated://events manifest",
                    "credentials": {"token": "/run/credentials/token"}
                });
                let conflicting = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["must-not-run"],
                        "pool": "slot",
                        "credentials": {"pool-token": "/run/credentials/wrong"}
                    })))
                    .await
                    .unwrap_err();
                assert_eq!(conflicting.code, WireErrorCode::InvalidParams);
                assert!(conflicting
                    .message
                    .contains("conflicting pool and enqueue sources"));
                let direct = daemon
                    .handler
                    .enqueue(Some(direct_payload.clone()))
                    .await
                    .unwrap();

                fs::create_dir_all(paths.events_dir()).unwrap();
                let mut file_payload = direct_payload.clone();
                file_payload["dedupKey"] = Value::String("from-file".to_owned());
                fs::write(
                    paths.events_dir().join("valid.json"),
                    serde_json::to_vec(&file_payload).unwrap(),
                )
                .unwrap();
                let malformed = json!({
                    "argv": ["one"],
                    "invocation": "two",
                    "pool": "slot"
                });
                fs::write(
                    paths.events_dir().join("malformed.json"),
                    serde_json::to_vec(&malformed).unwrap(),
                )
                .unwrap();
                std::os::unix::fs::symlink("/etc/passwd", paths.events_dir().join("hostile.json"))
                    .unwrap();
                let durable_oversize = json!({
                    "argv": ["x".repeat(600 * 1024)],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "events-dir"
                });
                let durable_oversize_bytes = serde_json::to_vec(&durable_oversize).unwrap();
                assert!(durable_oversize_bytes.len() < 1024 * 1024);
                fs::write(
                    paths.events_dir().join("durable-oversize.json"),
                    durable_oversize_bytes,
                )
                .unwrap();
                let direct_error = daemon.handler.enqueue(Some(malformed)).await.unwrap_err();

                let drained = daemon.handler.drain(None).await.unwrap();
                assert_eq!(drained["enqueued"], 1);
                assert_eq!(drained["rejected"], 3);
                assert_eq!(drained["repaired"], 0);
                assert!(drained["outcomes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|outcome| outcome["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains(&direct_error.message))));
                assert!(paths.events_dir().join("done/valid.json").is_file());
                assert!(paths.events_dir().join("rejected/malformed.json").is_file());
                assert!(paths
                    .events_dir()
                    .join("rejected/durable-oversize.json")
                    .is_file());
                assert!(drained["outcomes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|outcome| outcome["file"] == "durable-oversize.json"
                        && outcome["reason"]
                            .as_str()
                            .is_some_and(|reason| reason.contains("durable-event limit"))));
                assert!(
                    fs::symlink_metadata(paths.events_dir().join("rejected/hostile.json"))
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );

                let direct_id = Uuid::parse_str(direct["job_id"].as_str().unwrap()).unwrap();
                let context = daemon.handler.context.read().await;
                let direct_row = &context.jobs[&direct_id].row;
                let file_row = context
                    .jobs
                    .values()
                    .find(|job| job.row.dedup_key.as_deref() == Some("from-file"))
                    .unwrap()
                    .row
                    .clone();
                assert_eq!(file_row.argv, direct_row.argv);
                assert_eq!(file_row.pools, direct_row.pools);
                assert_eq!(file_row.priority, direct_row.priority);
                assert_eq!(file_row.adapter, direct_row.adapter);
                assert_eq!(file_row.source, direct_row.source);
                assert_eq!(file_row.evidence, direct_row.evidence);
                assert_eq!(file_row.evidence_class, direct_row.evidence_class);
                assert_eq!(file_row.manifest_hash, direct_row.manifest_hash);
                assert_eq!(
                    file_row.evidence_class,
                    Some(json!({"arbitrary": [true, 7, {"nested": null}]}))
                );
                assert_eq!(
                    file_row.manifest_hash,
                    Some(Value::String(
                        "deliberately-not-validated://events manifest".to_owned()
                    ))
                );
                assert_eq!(file_row.credentials, direct_row.credentials);
                assert_eq!(
                    direct_row.credentials["pool-token"],
                    PathBuf::from("/run/credentials/pool-token")
                );
                assert_eq!(
                    direct_row.credentials["token"],
                    PathBuf::from("/run/credentials/token")
                );
                drop(context);

                let repair_payload = json!({
                    "argv": ["repair-gap"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "events-dir",
                    "dedupKey": "repair-gap"
                });
                fs::write(
                    paths.events_dir().join("repair.json"),
                    serde_json::to_vec(&repair_payload).unwrap(),
                )
                .unwrap();
                let claims = claim_ingress_files(&paths.events_dir()).unwrap();
                assert_eq!(claims.len(), 1);
                let payload = read_ingress_payload(&claims[0]).unwrap();
                daemon
                    .handler
                    .enqueue_payload(payload, Some(claims[0].ingress_id.clone()))
                    .await
                    .unwrap();
                assert!(claims[0].path.exists());

                let repaired = daemon.handler.drain(None).await.unwrap();
                assert_eq!(repaired["enqueued"], 0);
                assert_eq!(repaired["rejected"], 0);
                assert_eq!(repaired["repaired"], 1);
                assert!(paths.events_dir().join("done/repair.json").is_file());
                let events = crate::taskdb::read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| {
                            event.ingress_id.as_deref() == Some(&claims[0].ingress_id)
                        })
                        .count(),
                    1
                );

                let transient_payload = json!({
                    "argv": ["transient-read"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "events-dir"
                });
                fs::write(
                    paths.events_dir().join("transient.json"),
                    serde_json::to_vec(&transient_payload).unwrap(),
                )
                .unwrap();
                let transient_claim = claim_ingress_files(&paths.events_dir()).unwrap().remove(0);
                fs::set_permissions(&transient_claim.path, fs::Permissions::from_mode(0o000))
                    .unwrap();
                let transient_error = daemon.handler.drain(None).await.unwrap_err();
                assert_eq!(transient_error.code, WireErrorCode::Internal);
                assert!(transient_claim.path.exists());
                assert!(!paths.events_dir().join("rejected/transient.json").exists());
                fs::set_permissions(&transient_claim.path, fs::Permissions::from_mode(0o600))
                    .unwrap();
                let retried = daemon.handler.drain(None).await.unwrap();
                assert_eq!(retried["enqueued"], 1);
                assert!(paths.events_dir().join("done/transient.json").is_file());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn enqueue_opaque_metadata_witnesses_and_queries_verbatim() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    two_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let daemon_history = daemon.handler.history.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let evidence_class = json!({
                    "arbitrary": [true, 7, {"nested": null}],
                    "label": "opaque"
                });
                let manifest_hash = "deliberately-not-validated://manifest value";
                let admitted = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["true"],
                            "pool": ["zeta", "slot"],
                            "priority": "high",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"],
                            "evidenceClass": evidence_class,
                            "manifestHash": manifest_hash
                        })),
                    )
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let terminal = client
                    .call("queue.await_job", Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");

                let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                let record = records
                    .iter()
                    .find(|record| record.task_uuid.as_deref() == Some(&task_uuid))
                    .unwrap();
                assert_eq!(
                    record.pools.as_deref(),
                    Some(["slot".to_owned(), "zeta".to_owned()].as_slice())
                );
                assert_eq!(record.evidence_class.as_ref(), Some(&evidence_class));
                assert_eq!(
                    record.manifest_hash,
                    Some(Value::String(manifest_hash.to_owned()))
                );

                let raw_witness = fs::read_to_string(paths.witness_path()).unwrap();
                let fielded_line = raw_witness
                    .lines()
                    .find(|line| line.contains(&task_uuid))
                    .unwrap();
                assert!(
                    fielded_line.find("\"evidence_class\"").unwrap()
                        < fielded_line.find("\"manifest_hash\"").unwrap()
                );
                assert!(
                    fielded_line.find("\"manifest_hash\"").unwrap()
                        < fielded_line.find("\"seq\"").unwrap()
                );

                let log = client
                    .call("query.log", Some(json!({"task": task_uuid})))
                    .await
                    .unwrap();
                let queried = log
                    .get("items")
                    .and_then(Value::as_array)
                    .unwrap()
                    .iter()
                    .find(|entry| entry["origin"] == "witness")
                    .unwrap();
                assert_eq!(queried["evidenceClass"], evidence_class);
                assert_eq!(queried["manifestHash"], manifest_hash);
                assert_eq!(queried["pool"], json!(["slot", "zeta"]));

                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                let durable = events
                    .iter()
                    .find(|event| event.row.uuid.to_string() == task_uuid)
                    .unwrap();
                assert_eq!(durable.row.pools, ["slot", "zeta"]);
                let status = client.call("query.status", Some(json!({}))).await.unwrap();
                let projected = status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == task_uuid)
                    .unwrap();
                assert_eq!(projected["pool"], json!(["slot", "zeta"]));
                assert!(daemon_history
                    .borrow()
                    .snapshot()
                    .records
                    .iter()
                    .any(|record| {
                        record.fields.task_uuid == task_uuid
                            && record.fields.pools.as_deref()
                                == Some(["slot".to_owned(), "zeta".to_owned()].as_slice())
                    }));

                let absent = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["true"],
                            "pool": ["slot", "zeta"],
                            "priority": "high",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                let absent_uuid = absent["task_uuid"].as_str().unwrap().to_owned();
                let terminal = client
                    .call("queue.await_job", Some(json!({"task_uuid": absent_uuid})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");
                let raw_witness = fs::read_to_string(paths.witness_path()).unwrap();
                let absent_line = raw_witness
                    .lines()
                    .find(|line| line.contains(&absent_uuid))
                    .unwrap();
                assert!(!absent_line.contains("\"evidence_class\""));
                assert!(!absent_line.contains("\"manifest_hash\""));

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn confirmed_pool_loss_witnesses_and_return_re_presents_the_same_row() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("resumable-agent");
                let started = temp.path().join("started");
                let resumed = temp.path().join("resumed");
                fs::write(
                    &program,
                    format!(
                        concat!(
                            "#!/bin/sh\n",
                            "if test \"$1\" = --resume; then\n",
                            "  printf '%s' \"$2\" > '{}'\n",
                            "  exit 0\n",
                            "fi\n",
                            "printf '%s\\n' '{{\"session_id\":\"durable-session\"}}'\n",
                            "> '{}'\n",
                            "sleep 30\n"
                        ),
                        resumed.display(),
                        started.display(),
                    ),
                )
                .unwrap();
                fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();

                let mut config = two_pool_config();
                config.pools.get_mut("slot").unwrap().auto_resume = Some(true);
                config.adapters.insert(
                    "resumable".to_owned(),
                    AdapterConfig {
                        argv: vec![program.to_string_lossy().into_owned()],
                        resume: Some(vec![
                            program.to_string_lossy().into_owned(),
                            "--resume".to_owned(),
                            "%<sessionRef>%".to_owned(),
                        ]),
                        scrape: BTreeMap::from([(
                            "sessionRef".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..session_id".to_owned(),
                            },
                        )]),
                        trace: None,
                        yield_hook: None,
                        env: BTreeMap::new(),
                        launch: crate::adapters::AdapterLaunchConfig::default(),
                        hardening: Default::default(),
                        extra_config: BTreeMap::new(),
                    },
                );
                config.producers = serde_json::from_value(json!({
                    "health": {
                        "kind": "pool-reachability",
                        "probePool": "slot",
                        "hysteresis": 1
                    }
                }))
                .unwrap();
                let registry = config.producers.clone();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let restart_config = config.clone();
                let restart_executor = executor.clone();
                let mut retry_settings = settings();
                retry_settings.recovery_policy.max_attempts = 2;
                let mut daemon =
                    Daemon::open_with_executor(config, paths.clone(), retry_settings, executor)
                        .await
                        .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["initial"],
                        "pool": "slot",
                        "adapter": "resumable",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !started.exists() {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                let parent_task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let child = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "zeta",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"],
                        "dedupKey": "acceptance-child",
                        "parent": parent_task_uuid,
                        "callerJobId": admitted["job_id"],
                    })))
                    .await
                    .unwrap();
                let child_task_uuid = child["task_uuid"].as_str().unwrap().to_owned();
                let child_finished =
                    tokio::time::timeout(Duration::from_secs(2), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(
                    child_finished.job_id.to_string(),
                    child["job_id"].as_str().unwrap()
                );
                daemon.finish_job(child_finished).await.unwrap();
                daemon.handler.drain_post_ack_tasks().await;

                let engine = ProducerEngine::new(&registry, paths.events_dir(), &paths.state_dir);
                let lost = engine
                    .observe_reachability("health", false, Utc::now())
                    .unwrap();
                assert_eq!(lost.transition, Some(ReachabilityTransition::Lost));
                let applied = daemon
                    .handler
                    .pool_transition(Some(json!({
                        "producer": "health",
                        "transition": "lost",
                        "generation": lost.generation,
                    })))
                    .await
                    .unwrap();
                assert_eq!(applied["affected"], 1);
                engine
                    .acknowledge_reachability_transition("health", lost.generation)
                    .unwrap();
                daemon.handler.drain_post_ack_tasks().await;
                let task_uuid = Uuid::parse_str(admitted["task_uuid"].as_str().unwrap()).unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Completed);
                    assert!(context.unreachable_pools.contains("slot"));
                }
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 2);
                let first_parent = records
                    .iter()
                    .find(|record| record.task_uuid.as_deref() == Some(parent_task_uuid.as_str()))
                    .unwrap();
                assert_eq!(first_parent.verdict, Verdict::PoolVanished);
                assert_eq!(first_parent.attempt, 1);

                let returned = engine
                    .observe_reachability("health", true, Utc::now())
                    .unwrap();
                assert_eq!(returned.transition, Some(ReachabilityTransition::Returned));
                let transition_params = json!({
                    "producer": "health",
                    "transition": "returned",
                    "generation": returned.generation,
                });
                let first_handler = daemon.handler.clone();
                let second_handler = daemon.handler.clone();
                let (first, second) = tokio::join!(
                    first_handler.pool_transition(Some(transition_params.clone())),
                    second_handler.pool_transition(Some(transition_params)),
                );
                let first = first.unwrap();
                let second = second.unwrap();
                assert_eq!(
                    [first["applied"].as_bool(), second["applied"].as_bool()],
                    [Some(true), Some(false)]
                );
                assert_eq!(first["affected"], 1);
                assert_eq!(second["alreadyApplied"], true);
                engine
                    .acknowledge_reachability_transition("health", returned.generation)
                    .unwrap();

                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let finished = daemon.completion_rx.recv().await.unwrap();
                        daemon.finish_job(finished).await.unwrap();
                        if daemon
                            .handler
                            .context
                            .read()
                            .await
                            .jobs
                            .get(&task_uuid)
                            .is_some_and(|job| {
                                job.state == JobState::Completed && job.row.attempt == 2
                            })
                        {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                assert_eq!(fs::read_to_string(&resumed).unwrap(), "durable-session");
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid.to_string()})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");
                assert_eq!(terminal["attempt"], 2);
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 3);
                let parent_records = records
                    .iter()
                    .filter(|record| record.task_uuid.as_deref() == Some(parent_task_uuid.as_str()))
                    .collect::<Vec<_>>();
                assert_eq!(parent_records.len(), 2);
                assert_eq!(parent_records[1].verdict, Verdict::Pass);
                assert_eq!(parent_records[1].attempt, 2);
                assert_eq!(parent_records[1].labor_class, LaborClass::Recovered);
                daemon.handler.drain_post_ack_tasks().await;

                drop(daemon);
                let restarted = Daemon::open_with_executor(
                    restart_config,
                    paths.clone(),
                    retry_settings,
                    restart_executor,
                )
                .await
                .unwrap();
                let jobs = restarted
                    .handler
                    .query("query.jobs", Some(json!({})))
                    .await
                    .unwrap();
                let parent = jobs["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == parent_task_uuid)
                    .unwrap();
                assert_eq!(parent["currentAttempt"], 2);
                assert_eq!(parent["terminalVerdict"], "pass");
                assert_eq!(parent["childTaskUuids"], json!([child_task_uuid.clone()]));
                let detail = restarted
                    .handler
                    .query("query.job", Some(json!({"id": parent_task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(detail["attempts"].as_array().unwrap().len(), 2);
                assert_eq!(detail["attempts"][0]["attempt"], 1);
                assert_eq!(detail["attempts"][1]["attempt"], 2);
                let log = restarted
                    .handler
                    .query(
                        "query.log",
                        Some(json!({"task": detail["job"]["taskUuid"]})),
                    )
                    .await
                    .unwrap();
                assert!(log["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["attempt"] == 1 && event["event"] == "failed"));
                assert!(log["items"].as_array().unwrap().iter().any(|event| {
                    event["attempt"] == 2
                        && event["authority"] == "canonical-witness-fact"
                        && event["terminalVerdict"] == "pass"
                }));
                let proof = restarted
                    .handler
                    .query(
                        "query.proof",
                        Some(json!({
                            "task": detail["job"]["taskUuid"],
                            "attempt": 2,
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(proof["status"], "verified");
                assert_eq!(proof["witnessRecord"]["verdict"], "pass");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pool_loss_intent_recovers_both_crash_windows_exactly_once() {
        let temp = tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        prepare_paths(&paths).unwrap();
        let row = durable_row(Uuid::new_v4(), "pool-loss-crash-window", 7);
        write_enqueue_event_atomic(
            &paths.events_dir(),
            &DurableEnqueueEvent::new(row.clone()).unwrap(),
        )
        .unwrap();
        let job = Job {
            job_id: row.uuid,
            task_uuid: Some(row.uuid),
            row: row.clone(),
            invocation: AdapterInvocation {
                argv: vec!["true".to_owned()],
                env: BTreeMap::new(),
                hardening: Default::default(),
                yield_hook: None,
            },
            labor_class: LaborClass::Fresh,
            state: JobState::Running,
            lease_id: None,
            adopted: false,
            adopted_invocation_id: None,
            model_is_advisory: false,
        };

        // Simulate a crash after the durable intent and before physical reclaim.
        let intent_path = write_pool_loss_intent(&paths.state_dir, &job).unwrap();
        assert_eq!(read_pool_loss_intent(&intent_path).unwrap().row, row);
        let inspections = Arc::new(AtomicUsize::new(0));
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
            .with_unit_probe(IntentObservingProbe {
                path: intent_path.clone(),
                task_uuid: job.row.uuid,
                inspections: inspections.clone(),
            });
        let mut ledger = WitnessLedger::open(paths.witness_path()).unwrap();
        reconcile_pool_loss_intents(&paths, &executor, &mut ledger)
            .await
            .unwrap();
        assert_eq!(inspections.load(Ordering::SeqCst), 1);
        assert!(!intent_path.exists());
        let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
        assert!(report.ok);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].task_uuid.as_deref(),
            Some(row.uuid.to_string().as_str())
        );
        assert_eq!(records[0].verdict, Verdict::PoolVanished);
        assert_eq!(records[0].attempt, row.attempt);
        assert_eq!(records[0].lease_epoch, row.lease_epoch);

        // Simulate a second crash after witness fsync and before intent removal.
        assert_eq!(
            write_pool_loss_intent(&paths.state_dir, &job).unwrap(),
            intent_path
        );
        reconcile_pool_loss_intents(&paths, &executor, &mut ledger)
            .await
            .unwrap();
        assert_eq!(inspections.load(Ordering::SeqCst), 1);
        assert!(!intent_path.exists());
        let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
        assert!(report.ok);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].verdict, Verdict::PoolVanished);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn startup_pool_loss_preserves_an_already_recorded_real_exit() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                let row = durable_row(Uuid::new_v4(), "startup-real-exit", 1);
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(row.clone()).unwrap(),
                )
                .unwrap();
                let mut config = one_pool_config();
                config.producers = serde_json::from_value(json!({
                    "health": {
                        "kind": "pool-reachability",
                        "probePool": "slot",
                        "hysteresis": 1
                    }
                }))
                .unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let identity = ExecutionIdentity {
                    job_id: row.uuid,
                    task_uuid: Some(row.uuid),
                };
                write_exit_record(
                    &executor.paths(&identity).exit_record,
                    &UnitExitRecord {
                        schema_version: crate::executor::UNIT_EXIT_SCHEMA_VERSION,
                        unit: executor.unit_name(&identity),
                        invocation_id: "recorded-before-startup".to_owned(),
                        attempt: 1,
                        lease_epoch: 1,
                        service_result: "success".to_owned(),
                        exit_code: Some("exited".to_owned()),
                        exit_status: Some("0".to_owned()),
                    },
                )
                .unwrap();
                let engine =
                    ProducerEngine::new(&config.producers, paths.events_dir(), &paths.state_dir);
                let lost = engine
                    .observe_reachability("health", false, Utc::now())
                    .unwrap();
                assert_eq!(lost.transition, Some(ReachabilityTransition::Lost));
                assert!(!pool_loss_intent_directory(&paths.state_dir).exists());

                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                assert_eq!(daemon.initial_jobs.len(), 1);
                assert!(daemon.handler.context.read().await.jobs[&row.uuid]
                    .lease_id
                    .is_none());
                let (shutdown, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let result = client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": row.uuid.to_string()})),
                    )
                    .await
                    .unwrap();
                assert_eq!(result["verdict"], "pass");
                shutdown.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].verdict, Verdict::Pass);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn confirmed_return_leaves_nonresumable_rows_terminal() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                let row = durable_row(Uuid::new_v4(), "nonresumable-return", 1);
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(row.clone()).unwrap(),
                )
                .unwrap();
                WitnessLedger::open(paths.witness_path())
                    .unwrap()
                    .append(WitnessBody {
                        task_uuid: Some(row.uuid.to_string()),
                        transition_timestamp: Utc::now()
                            .to_rfc3339_opts(SecondsFormat::Millis, true),
                        verdict: Verdict::PoolVanished,
                        exit_code: 1,
                        artifact_content_hash: None,
                        gpu_seconds: None,
                        wall_clock: 0.0,
                        attempt: 1,
                        lease_epoch: 1,
                        dedup_key: row.dedup_key.clone(),
                        payload_hash: row.payload_hash.clone(),
                        brief_hash: row.brief_hash.clone(),
                        orchestration: row.orchestration.clone(),
                        labor_class: LaborClass::Fresh,
                        trace_ref: None,
                        pools: Some(vec!["slot".to_owned()]),
                        executor: None,
                        charge: None,
                        model: None,
                        evidence_class: None,
                        manifest_hash: None,
                        completion: None,
                    })
                    .unwrap();
                let mut config = one_pool_config();
                config.pools.get_mut("slot").unwrap().auto_resume = Some(true);
                config.producers = serde_json::from_value(json!({
                    "health": {
                        "kind": "pool-reachability",
                        "probePool": "slot",
                        "hysteresis": 1
                    }
                }))
                .unwrap();
                let engine =
                    ProducerEngine::new(&config.producers, paths.events_dir(), &paths.state_dir);
                let lost = engine
                    .observe_reachability("health", false, Utc::now())
                    .unwrap();
                engine
                    .acknowledge_reachability_transition("health", lost.generation)
                    .unwrap();
                let returned = engine
                    .observe_reachability("health", true, Utc::now())
                    .unwrap();
                assert_eq!(returned.transition, Some(ReachabilityTransition::Returned));

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(config, paths, settings(), executor)
                    .await
                    .unwrap();
                assert!(daemon.initial_jobs.is_empty());
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": row.uuid.to_string()})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pool-vanished");
                assert_eq!(terminal["attempt"], 1);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn successful_durable_gh_row_runs_the_concrete_completed_mutation_once() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let mut config = one_pool_config();
                config.producers = serde_json::from_value(json!({
                    "github": {
                        "kind": "gh",
                        "enable": true,
                        "sources": [{"notifications": {"repo": "acme/widgets"}}],
                        "triggers": {"assignments": ["tally-bot"]},
                        "postEvidence": true,
                        "enqueue": {"argv": ["true"], "pool": "slot"}
                    }
                }))
                .unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(config, paths, settings(), executor)
                        .await
                        .unwrap();
                let gh = temp.path().join("fake-gh");
                let requests = temp.path().join("gh-requests.jsonl");
                let calls = temp.path().join("gh-calls");
                fs::write(
                    &gh,
                    format!(
                        concat!(
                            "#!/bin/sh\n",
                            "[ \"$1 $2 $3 $4\" = 'api graphql --input -' ] || exit 91\n",
                            "request=$(cat)\n",
                            "printf '%s\\n' \"$request\" >> '{}'\n",
                            "printf x >> '{}'\n",
                            "case \"$request\" in\n",
                            "  *TallyCompletionState*) printf '{{\"data\":{{\"node\":{{\"__typename\":\"Issue\",\"state\":\"OPEN\",\"comments\":{{\"nodes\":[],\"pageInfo\":{{\"hasNextPage\":false,\"endCursor\":null}}}}}}}}}}' ;;\n",
                            "  *TallyCompletionComment*) printf '{{\"data\":{{\"addComment\":{{}}}}}}' ;;\n",
                            "  *TallyCompletionIssue*) printf '{{\"data\":{{\"closeIssue\":{{}}}}}}' ;;\n",
                            "  *) exit 92 ;;\n",
                            "esac\n"
                        ),
                        requests.display(),
                        calls.display(),
                    ),
                )
                .unwrap();
                fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
                daemon.handler.gh_program = gh;

                let mut row = durable_row(Uuid::new_v4(), "gh:github:item-1", 1);
                row.source = EnqueueSource::Gh;
                row.adapter = "codex".to_owned();
                row.gh_origin = Some(gh_test_origin("item-1", GhItemType::Issue));
                let result = JobResult {
                    task_uuid: Some(row.uuid.to_string()),
                    job_id: row.uuid.to_string(),
                    verdict: Verdict::Pass,
                    exit_code: 0,
                    artifact_content_hash: Some("sha256:artifact".to_owned()),
                    attempt: 1,
                    lease_epoch: 1,
                    witness_seq: 9,
                    model: Some("gpt-5.6-codex".to_owned()),
                    completion: None,
                };
                daemon
                    .handler
                    .complete_gh_post_ack(row.clone(), result.clone());
                daemon
                    .handler
                    .complete_gh_post_ack(row.clone(), result.clone());
                daemon.handler.drain_post_ack_tasks().await;

                assert_eq!(fs::read(&calls).unwrap(), b"xxxx");
                let requests = fs::read_to_string(&requests)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).unwrap())
                    .collect::<Vec<_>>();
                let comment = requests
                    .iter()
                    .find(|request| request["query"]
                        .as_str()
                        .unwrap()
                        .contains("TallyCompletionComment"))
                    .unwrap();
                assert_eq!(comment["variables"]["itemId"], "item-1");
                let body = comment["variables"]["body"].as_str().unwrap();
                let (_, remainder) = body.split_once('\n').unwrap();
                let (encoded, trailer) = remainder.split_once("\n\n").unwrap();
                let evidence: Value = serde_json::from_str(encoded).unwrap();
                assert_eq!(evidence["producer"], "github");
                assert_eq!(evidence["source"], "notifications");
                assert_eq!(evidence["itemId"], "item-1");
                assert_eq!(evidence["state"], "COMPLETED");
                assert_eq!(evidence["evidence"]["taskUuid"], row.uuid.to_string());
                assert_eq!(evidence["evidence"]["witnessSeq"], 9);
                assert_eq!(evidence["evidence"]["verdict"], "pass");
                assert_eq!(
                    trailer,
                    format!(
                        "Assisted-by: codex:gpt-5.6-codex (tally:{} witness:9)",
                        row.uuid
                    )
                );

                let mut failed = result;
                failed.witness_seq = 10;
                failed.verdict = Verdict::Failed;
                daemon.handler.complete_gh_post_ack(row, failed);
                daemon.handler.drain_post_ack_tasks().await;
                assert_eq!(fs::read(calls).unwrap(), b"xxxx");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_7_producer_origin_survives_restart_and_joins_inventory() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let mut config = one_pool_config();
                config.producers = serde_json::from_value(json!({
                    "daily": {
                        "kind": "calendar",
                        "onCalendar": "daily",
                        "enqueue": {"argv": ["calendar"], "pool": "slot"}
                    },
                    "drop": {"kind": "events-dir"},
                    "github": {
                        "kind": "gh",
                        "enable": true,
                        "sources": [{"notifications": {"repo": "acme/widgets"}}],
                        "triggers": {"assignments": ["tally-bot"]},
                        "postEvidence": true,
                        "enqueue": {"argv": ["github"], "pool": "slot"}
                    },
                    "effects": {
                        "kind": "build-effect",
                        "watch": "jsonl",
                        "path": "/var/empty/tally-effects.jsonl",
                        "onKey": {"argv": ["effect"], "pool": "slot"}
                    },
                    "health": {
                        "kind": "pool-reachability",
                        "probePool": "slot",
                        "hysteresis": 2,
                        "onLost": {"argv": ["lost"], "pool": "slot"},
                        "onReturn": {"argv": ["returned"], "pool": "slot"},
                        "onReturnAttest": {
                            "argv": ["attest"],
                            "pool": "slot",
                            "noEnqueue": true
                        }
                    }
                }))
                .unwrap();
                config.validate().unwrap();
                let now = Utc::now();
                let engine =
                    ProducerEngine::new(&config.producers, paths.events_dir(), &paths.state_dir);
                engine.emit_calendar("daily", now).unwrap();
                engine
                    .emit_gh(
                        "github",
                        &gh_test_observation("PR-live-producer", GhItemType::PullRequest),
                        now,
                    )
                    .unwrap();
                engine
                    .emit_build_effect(
                        "effects",
                        Path::new("/nix/store/00000000000000000000000000000000-live-producer"),
                        now,
                    )
                    .unwrap();
                assert!(engine
                    .observe_reachability("health", false, now)
                    .unwrap()
                    .transition
                    .is_none());
                let lost = engine.observe_reachability("health", false, now).unwrap();
                assert!(lost.transition.is_some());
                engine
                    .acknowledge_reachability_transition("health", lost.generation)
                    .unwrap();
                assert!(engine
                    .observe_reachability("health", true, now)
                    .unwrap()
                    .transition
                    .is_none());
                let returned = engine.observe_reachability("health", true, now).unwrap();
                assert!(returned.transition.is_some());
                engine
                    .acknowledge_reachability_transition("health", returned.generation)
                    .unwrap();
                assert!(matches!(
                    config.producers["drop"],
                    ProducerConfig::EventsDir(_)
                ));
                fs::write(
                    paths.events_dir().join("drop-fixture.producer.json"),
                    serde_json::to_vec(&json!({
                        "argv": ["event"],
                        "pool": "slot"
                    }))
                    .unwrap(),
                )
                .unwrap();

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    config.clone(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let drained = daemon
                    .handler
                    .drain(Some(json!({"producer": "drop"})))
                    .await
                    .unwrap();
                assert_eq!(drained["enqueued"], 7);
                assert_eq!(drained["rejected"], 0);

                let context = daemon.handler.context.read().await;
                for (source, expected) in [
                    (EnqueueSource::Calendar, 1),
                    (EnqueueSource::EventsDir, 1),
                    (EnqueueSource::Gh, 1),
                    (EnqueueSource::BuildEffect, 1),
                    (EnqueueSource::PoolReachability, 3),
                ] {
                    assert_eq!(
                        context
                            .jobs
                            .values()
                            .filter(|job| job.row.source == source)
                            .count(),
                        expected
                    );
                }
                assert_eq!(
                    context
                        .jobs
                        .values()
                        .filter(|job| job.row.no_enqueue)
                        .count(),
                    1
                );
                let expected_origins = context
                    .jobs
                    .values()
                    .map(|job| {
                        (
                            job.row.uuid.to_string(),
                            job.row
                                .origin
                                .as_ref()
                                .and_then(|origin| origin.producer.as_ref())
                                .map(|producer| producer.name.clone())
                                .unwrap(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(expected_origins.len(), 7);
                assert_eq!(
                    expected_origins.values().fold(
                        BTreeMap::<String, usize>::new(),
                        |mut counts, name| {
                            *counts.entry(name.clone()).or_default() += 1;
                            counts
                        }
                    ),
                    BTreeMap::from([
                        ("daily".to_owned(), 1),
                        ("drop".to_owned(), 1),
                        ("effects".to_owned(), 1),
                        ("github".to_owned(), 1),
                        ("health".to_owned(), 3),
                    ])
                );
                drop(context);
                assert!(crate::taskdb::read_acknowledged_events(&paths.events_dir())
                    .unwrap()
                    .iter()
                    .all(|event| event.ingress_id.is_some()));

                drop(daemon);
                let restarted =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                let jobs = restarted
                    .handler
                    .query("query.jobs", Some(json!({"limit": 100})))
                    .await
                    .unwrap();
                let observed_origins = jobs["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|job| {
                        (
                            job["taskUuid"].as_str().unwrap().to_owned(),
                            job["origin"]["value"]["producer"]["name"]
                                .as_str()
                                .unwrap()
                                .to_owned(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(observed_origins, expected_origins);

                let producers = restarted
                    .handler
                    .query("query.producers", Some(json!({"name": "daily"})))
                    .await
                    .unwrap();
                assert_eq!(producers["items"][0]["name"], "daily");
                assert_eq!(producers["items"][0]["kind"], "calendar");
                assert_eq!(
                    producers["items"][0]["schedule"]["calendarExpression"],
                    "daily"
                );
                let watch_tail = restarted
                    .handler
                    .query("query.watch", Some(json!({})))
                    .await
                    .unwrap()["nextCursor"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                restarted
                    .handler
                    .producer_runtime_observed(Some(json!({"producer": "daily"})))
                    .await
                    .unwrap();
                let changes = restarted
                    .handler
                    .query(
                        "query.watch",
                        Some(json!({"after": watch_tail, "limit": 100})),
                    )
                    .await
                    .unwrap();
                assert_eq!(changes["items"][0]["kind"], "producer");
                assert_eq!(changes["items"][0]["payload"]["name"], "daily");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hydrated_github_pr_origin_reaches_launch_status_and_survives_restart() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let mut config = one_pool_config();
                config.producers = serde_json::from_value(json!({
                    "github": {
                        "kind": "gh",
                        "enable": true,
                        "sources": [{"notifications": {"repo": "acme/widgets"}}],
                        "triggers": {"mentions": ["@tally-bot inspect this"]},
                        "allowedActors": ["maintainer"],
                        "postEvidence": false,
                        "closeOnPass": false,
                        "enqueue": {"argv": ["handle-origin"], "pool": "slot"}
                    }
                }))
                .unwrap();
                config.validate().unwrap();

                let gh = temp.path().join("fake-gh-origin");
                fs::write(
                    &gh,
                    concat!(
                        "#!/bin/sh\n",
                        "case \"$*\" in\n",
                        "  'api user') printf '{\"login\":\"tally-bot\"}' ;;\n",
                        "  'api --method GET notifications -f all=false -f participating=false -f per_page=100')\n",
                        "    printf '[{\"id\":\"notification-42\",\"reason\":\"mention\",\"updated_at\":\"2026-07-24T08:00:00Z\",\"repository\":{\"full_name\":\"acme/widgets\"},\"subject\":{\"type\":\"PullRequest\",\"url\":\"https://api.github.com/repos/acme/widgets/pulls/42\",\"latest_comment_url\":\"https://api.github.com/repos/acme/widgets/issues/comments/4200\"}}]' ;;\n",
                        "  'api /repos/acme/widgets/pulls/42')\n",
                        "    printf '{\"node_id\":\"PR_origin_42\",\"number\":42,\"html_url\":\"https://github.com/acme/widgets/pull/42\",\"state\":\"open\",\"title\":\"Hydrated PR\",\"body\":\"untrusted $(never-executed)\",\"user\":{\"login\":\"item-author\"},\"head\":{\"sha\":\"4242424242424242424242424242424242424242\"},\"labels\":[{\"name\":\"build\"}],\"assignees\":[{\"login\":\"tally-bot\"}]}' ;;\n",
                        "  'api /repos/acme/widgets/issues/comments/4200')\n",
                        "    printf '{\"id\":4200,\"body\":\"@tally-bot inspect this\",\"created_at\":\"2026-07-24T08:00:00Z\",\"updated_at\":\"2026-07-24T08:00:00Z\",\"user\":{\"login\":\"maintainer\"}}' ;;\n",
                        "  *) exit 91 ;;\n",
                        "esac\n",
                    ),
                )
                .unwrap();
                fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
                let engine =
                    ProducerEngine::new(&config.producers, paths.events_dir(), &paths.state_dir);
                let outcomes = engine
                    .poll_gh("github", &GhCliIntake::with_program(&gh), Utc::now())
                    .unwrap();
                let emitted = match outcomes.as_slice() {
                    [EmitOutcome::Emitted(path)] => path,
                    other => panic!("expected one emitted hydrated PR origin, got {other:?}"),
                };
                let payload: EnqueuePayload =
                    serde_json::from_slice(&fs::read(emitted).unwrap()).unwrap();
                let captured_origin = payload.gh_origin.unwrap();
                assert_eq!(captured_origin.repo, "acme/widgets");
                assert_eq!(captured_origin.number, 42);
                assert_eq!(
                    captured_origin.html_url,
                    "https://github.com/acme/widgets/pull/42"
                );
                assert_eq!(captured_origin.item_type, Some(GhItemType::PullRequest));
                assert_eq!(
                    captured_origin.head_sha.as_deref(),
                    Some("4242424242424242424242424242424242424242")
                );
                assert_eq!(captured_origin.node_id, "PR_origin_42");
                assert_eq!(captured_origin.item_author, "item-author");
                assert_eq!(captured_origin.trigger_actor, "maintainer");
                assert_eq!(captured_origin.self_actor, "tally-bot");
                assert_eq!(
                    captured_origin.notification_reason.as_deref(),
                    Some("mention")
                );
                assert_eq!(captured_origin.trigger_kind, "mention");
                assert_eq!(
                    captured_origin.event_id.as_deref(),
                    Some("notification-42")
                );
                assert_eq!(captured_origin.comment_id.as_deref(), Some("4200"));
                assert_eq!(
                    captured_origin
                        .context
                        .as_ref()
                        .unwrap()
                        .triggering_comment
                        .as_ref()
                        .unwrap()
                        .author,
                    "maintainer"
                );

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    config.clone(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                assert_eq!(daemon.handler.drain(None).await.unwrap()["enqueued"], 1);

                let job = daemon
                    .handler
                    .context
                    .read()
                    .await
                    .jobs
                    .values()
                    .find(|job| job.row.source == EnqueueSource::Gh)
                    .cloned()
                    .unwrap();
                assert_eq!(job.row.gh_origin.as_ref(), Some(&captured_origin));
                let request = execution_request(
                    &executor,
                    &job,
                    settings().unit_limits,
                    "/run/tally/tally.sock",
                    &paths.data_dir,
                )
                .unwrap();
                let args = executor
                    .build_systemd_argv(&request)
                    .unwrap()
                    .into_iter()
                    .map(|arg| arg.into_string().unwrap())
                    .collect::<Vec<_>>();
                let launched_environment = args
                    .windows(2)
                    .filter(|pair| pair[0] == "--setenv")
                    .filter_map(|pair| pair[1].split_once('='))
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .collect::<BTreeMap<_, _>>();
                let github_environment = launched_environment
                    .into_iter()
                    .filter(|(name, _)| name.starts_with("TALLY_GH_"))
                    .collect::<BTreeMap<_, _>>();
                assert_eq!(
                    github_environment,
                    BTreeMap::from([
                        ("TALLY_GH_REPO".to_owned(), "acme/widgets".to_owned()),
                        ("TALLY_GH_NUMBER".to_owned(), "42".to_owned()),
                        (
                            "TALLY_GH_URL".to_owned(),
                            "https://github.com/acme/widgets/pull/42".to_owned()
                        ),
                        ("TALLY_GH_TYPE".to_owned(), "pull_request".to_owned()),
                        (
                            "TALLY_GH_HEAD_SHA".to_owned(),
                            "4242424242424242424242424242424242424242".to_owned()
                        ),
                        ("TALLY_GH_NODE_ID".to_owned(), "PR_origin_42".to_owned()),
                        (
                            "TALLY_GH_TRIGGER_KIND".to_owned(),
                            "mention".to_owned()
                        ),
                        (
                            "TALLY_GH_TRIGGER_ACTOR".to_owned(),
                            "maintainer".to_owned()
                        ),
                        (
                            "TALLY_GH_EVENT_ID".to_owned(),
                            "notification-42".to_owned()
                        ),
                        ("TALLY_GH_COMMENT_ID".to_owned(), "4200".to_owned()),
                        (
                            "TALLY_GH_CONTEXT".to_owned(),
                            executor
                                .gh_context_path(&request.identity)
                                .to_string_lossy()
                                .into_owned()
                        ),
                    ])
                );

                let row = query_row(&job.row, RowStatus::Pending);
                let expected_projection = crate::query::GhOriginProjection {
                    repo: "acme/widgets".to_owned(),
                    number: 42,
                    url: "https://github.com/acme/widgets/pull/42".to_owned(),
                };
                let task_uuid = job.row.uuid.to_string();
                assert_eq!(row.gh_origin, Some(expected_projection.clone()));
                let status = daemon
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                let projected = status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|projected| projected["taskUuid"] == task_uuid)
                    .unwrap();
                assert_eq!(
                    projected["ghOrigin"],
                    serde_json::to_value(&expected_projection).unwrap()
                );
                let standup = query_standup(
                    &[row],
                    &[],
                    &[],
                    &StandupOptions {
                        since: None,
                        since_realtime_us: None,
                        until: "2026-07-24T00:00:00Z".to_owned(),
                        source: Some("gh".to_owned()),
                    },
                );
                assert_eq!(standup.in_flight.len(), 1);
                assert_eq!(
                    standup.in_flight[0].gh_origin,
                    Some(expected_projection.clone())
                );

                drop(daemon);
                tokio::task::yield_now().await;
                let restarted =
                    Daemon::open_with_executor(config, paths, settings(), executor)
                        .await
                        .unwrap();
                let restarted_status = restarted
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                let restarted_job = restarted_status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|projected| projected["taskUuid"] == task_uuid)
                    .unwrap();
                assert_eq!(
                    restarted_job["ghOrigin"],
                    serde_json::to_value(expected_projection).unwrap()
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn codex_job_options_cwd_and_workspace_reach_systemd_as_exact_direct_values() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let mut config = one_pool_config();
                config.adapters.insert(
                    "codex".to_owned(),
                    AdapterConfig {
                        argv: vec![
                            "codex".to_owned(),
                            "exec".to_owned(),
                            "--json".to_owned(),
                            "--".to_owned(),
                        ],
                        launch: crate::adapters::AdapterLaunchConfig {
                            allow_pre_prompt_argv: true,
                            cwd_argv: Some(vec!["-C".to_owned(), "%<cwd>%".to_owned()]),
                            approval_policies: BTreeMap::from([("never".to_owned(), Vec::new())]),
                            sandbox_policies: BTreeMap::from([(
                                "danger-full-access".to_owned(),
                                Vec::new(),
                            )]),
                            ..crate::adapters::AdapterLaunchConfig::default()
                        },
                        ..AdapterConfig::default()
                    },
                );
                config.validate().unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor.clone())
                        .await
                        .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["author wave 3"],
                        "pool": "slot",
                        "adapter": "codex",
                        "cwd": "/worktrees/issue-28",
                        "workspace": {
                            "repo": "mecattaf/tally.nix",
                            "baseRev": "origin/main",
                            "branch": "wave-3-ergonomics",
                            "worktreePath": "/worktrees/issue-28"
                        },
                        "adapterOptions": {
                            "prePromptArgv": ["--dangerously-bypass-approvals-and-sandbox"],
                            "environment": {"NO_COLOR": "1"},
                            "approvalPolicy": "never",
                            "sandboxPolicy": "danger-full-access"
                        }
                    })))
                    .await
                    .unwrap();
                let job_id = Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap();
                let job = daemon
                    .handler
                    .context
                    .read()
                    .await
                    .jobs
                    .get(&job_id)
                    .cloned()
                    .unwrap();
                assert_eq!(
                    job.invocation.argv,
                    [
                        "codex",
                        "exec",
                        "--json",
                        "--dangerously-bypass-approvals-and-sandbox",
                        "-C",
                        "/worktrees/issue-28",
                        "--",
                        "author wave 3",
                    ]
                );
                assert_eq!(job.invocation.env["NO_COLOR"], "1");
                assert_eq!(
                    job.row.workspace.as_ref().unwrap().repo,
                    "mecattaf/tally.nix"
                );

                let request = execution_request(
                    &executor,
                    &job,
                    settings().unit_limits,
                    "/run/tally/tally.sock",
                    &paths.data_dir,
                )
                .unwrap();
                let args = executor
                    .build_systemd_argv(&request)
                    .unwrap()
                    .into_iter()
                    .map(|argument| argument.into_string().unwrap())
                    .collect::<Vec<_>>();
                assert!(args
                    .windows(2)
                    .any(|pair| { pair == ["--working-directory", "/worktrees/issue-28"] }));
                for expected in [
                    "NO_COLOR=1",
                    "TALLY_WORKSPACE_REPO=mecattaf/tally.nix",
                    "TALLY_WORKSPACE_BASE_REV=origin/main",
                    "TALLY_WORKSPACE_BRANCH=wave-3-ergonomics",
                    "TALLY_WORKSPACE_PATH=/worktrees/issue-28",
                ] {
                    assert!(args.windows(2).any(|pair| pair == ["--setenv", expected]));
                }
                assert!(args.ends_with(&[
                    "--".to_owned(),
                    "codex".to_owned(),
                    "exec".to_owned(),
                    "--json".to_owned(),
                    "--dangerously-bypass-approvals-and-sandbox".to_owned(),
                    "-C".to_owned(),
                    "/worktrees/issue-28".to_owned(),
                    "--".to_owned(),
                    "author wave 3".to_owned(),
                ]));
                assert_eq!(
                    query_row(&job.row, RowStatus::Pending)
                        .workspace
                        .unwrap()
                        .worktree_path,
                    PathBuf::from("/worktrees/issue-28")
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn public_continuation_uses_the_scraped_session_without_manual_captures() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("resumable-agent");
                fs::write(
                    &program,
                    "#!/bin/sh\nprintf '%s\\n' '{\"thread_id\":\"session-28\"}'\n",
                )
                .unwrap();
                fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
                let mut config = one_pool_config();
                config.adapters.insert(
                    "resumable".to_owned(),
                    AdapterConfig {
                        argv: vec![
                            program.to_string_lossy().into_owned(),
                            "fresh".to_owned(),
                            "--".to_owned(),
                        ],
                        resume: Some(vec![
                            program.to_string_lossy().into_owned(),
                            "resume".to_owned(),
                            "%<sessionRef>%".to_owned(),
                            "--".to_owned(),
                        ]),
                        scrape: BTreeMap::from([(
                            "sessionRef".to_owned(),
                            ScrapeCapture {
                                stream: ScrapeStream::Stdout,
                                mode: ScrapeMode::JsonPath,
                                pattern: "$..thread_id".to_owned(),
                            },
                        )]),
                        ..AdapterConfig::default()
                    },
                );
                config.validate().unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(config, paths, settings(), executor)
                    .await
                    .unwrap();
                let first = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["initial request"],
                        "pool": "slot",
                        "adapter": "resumable"
                    })))
                    .await
                    .unwrap();
                let finished =
                    tokio::time::timeout(Duration::from_secs(2), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                daemon.handler.drain_post_ack_tasks().await;
                let first_id = first["job_id"].as_str().unwrap();
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .jobs
                        .get(&Uuid::parse_str(first_id).unwrap())
                        .unwrap()
                        .row
                        .session_ref
                        .as_deref(),
                    Some("session-28")
                );

                let continued = daemon
                    .handler
                    .continue_job(Some(json!({
                        "resumeFrom": first_id,
                        "argv": ["address review"]
                    })))
                    .await
                    .unwrap();
                let continued_id = Uuid::parse_str(continued["job_id"].as_str().unwrap()).unwrap();
                let continued_job = daemon
                    .handler
                    .context
                    .read()
                    .await
                    .jobs
                    .get(&continued_id)
                    .cloned()
                    .unwrap();
                assert_eq!(
                    continued_job.invocation.argv,
                    [
                        program.to_string_lossy().into_owned(),
                        "resume".to_owned(),
                        "session-28".to_owned(),
                        "--".to_owned(),
                        "address review".to_owned(),
                    ]
                );
                assert_eq!(continued_job.row.resumed_from.as_deref(), Some(first_id));
                assert_eq!(continued_job.row.session_ref.as_deref(), Some("session-28"));

                let finished =
                    tokio::time::timeout(Duration::from_secs(2), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"job_id": continued_id.to_string()})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn zero_exit_with_failed_and_missing_declared_gates_is_semantically_rejected() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let manifest = temp.path().join("gates.json");
                fs::write(
                    &manifest,
                    r#"{"schemaVersion":1,"artifact":{"commit":"abc"},"gates":[{"id":"tests","status":"fail","command":"cargo test","reason":"one test failed"}]}"#,
                )
                .unwrap();
                let program = temp.path().join("successful-job");
                fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
                fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": [program],
                        "pool": "slot",
                        "gateManifest": {
                            "path": manifest,
                            "requiredGateIds": ["tests", "live"],
                            "acceptancePolicy": "execution-and-gates"
                        }
                    })))
                    .await
                    .unwrap();
                let finished = tokio::time::timeout(
                    Duration::from_secs(2),
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap()
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": admitted["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "failed");
                assert_eq!(terminal["exit_code"], 0);
                assert_eq!(terminal["completion"]["execution"]["status"], "success");
                assert_eq!(terminal["completion"]["gates"]["status"], "fail");
                assert_eq!(
                    terminal["completion"]["gates"]["missingRequiredGateIds"],
                    json!(["live"])
                );
                assert_eq!(
                    terminal["completion"]["acceptance"]["status"],
                    "rejected"
                );
                let status = daemon
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap();
                let public_job = status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == task_uuid)
                    .unwrap();
                assert_eq!(public_job["verdict"], "failed");
                let standup = daemon
                    .handler
                    .query("query.standup", Some(json!({})))
                    .await
                    .unwrap();
                assert!(standup["completed"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|entry| entry["taskUuid"] != task_uuid));
                let gate_failure = standup["gateFails"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["taskUuid"] == task_uuid)
                    .unwrap();
                assert_eq!(gate_failure["verdict"], "failed");
                let (_, witness) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(witness[0].verdict, Verdict::Failed);
                assert_eq!(witness[0].exit_code, 0);
                let completion = witness[0].completion.as_ref().unwrap();
                assert_eq!(
                    completion.execution.status,
                    crate::completion::ExecutionStatus::Success
                );
                assert_eq!(
                    completion.gates.status,
                    crate::completion::GateSummaryStatus::Fail
                );
                assert_eq!(
                    completion.acceptance.status,
                    crate::completion::AcceptanceStatus::Rejected
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preset_gate_defaults_distinguish_absent_manifest_from_gates_passed() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let observed_path = temp.path().join("observed-manifest-path");
                let program = temp.path().join("gate-aware-agent");
                fs::write(
                    &program,
                    format!(
                        concat!(
                            "#!/bin/sh\n",
                            "test -n \"$TALLY_GATE_MANIFEST\" || exit 51\n",
                            "printf '%s' \"$TALLY_GATE_MANIFEST\" > '{}'\n",
                            "if test \"$1\" = write; then\n",
                            "  printf '%s' '{{\"schemaVersion\":1,\"artifact\":null,\"gates\":[]}}' > \"$TALLY_GATE_MANIFEST\"\n",
                            "fi\n",
                        ),
                        observed_path.display(),
                    ),
                )
                .unwrap();
                fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
                let mut config = one_pool_config();
                config
                    .adapters
                    .insert("codex".to_owned(), AdapterConfig::default());
                let executor =
                    Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                        .with_systemd_run(temp.path().join("absent-systemd-run"))
                        .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();

                let absent = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": [program, "absent"],
                        "pool": "slot",
                        "adapter": "codex",
                    })))
                    .await
                    .unwrap();
                let finished =
                    tokio::time::timeout(Duration::from_secs(2), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let absent_result = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": absent["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(absent_result["verdict"], "pass");
                assert_eq!(absent_result["completion"]["gates"]["status"], "not-run");
                let absent_uuid =
                    Uuid::parse_str(absent["task_uuid"].as_str().unwrap()).unwrap();
                assert!(daemon
                    .handler
                    .context
                    .read()
                    .await
                    .jobs
                    .get(&absent_uuid)
                    .unwrap()
                    .row
                    .gate_manifest
                    .is_none());
                assert_eq!(
                    fs::read_to_string(&observed_path).unwrap(),
                    paths
                        .state_dir
                        .join("capture")
                        .join(format!("{absent_uuid}.attempt-1.gates.json"))
                        .to_string_lossy()
                );

                let passed = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": [program, "write"],
                        "pool": "slot",
                        "adapter": "codex",
                    })))
                    .await
                    .unwrap();
                let finished =
                    tokio::time::timeout(Duration::from_secs(2), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let passed_result = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": passed["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(passed_result["verdict"], "pass");
                assert_eq!(passed_result["completion"]["gates"]["status"], "pass");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_5_trace_and_scraped_usage_are_advisory_only() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("custom-agent");
                fs::write(
                    &program,
                    concat!(
                        "#!/bin/sh\n",
                        "test \"$1\" = --structured || exit 41\n",
                        "test \"$2\" = 'literal;$(not-a-shell)' || exit 42\n",
                        "test \"$CUSTOM_AGENT_MODE\" = batch || exit 43\n",
                        "test \"$TALLY_YIELD_HOOK\" = '[\"tally\",\"lease\",\"status\"]' || exit 44\n",
                        "test -S \"$TALLY_SOCKET\" || exit 45\n",
                        "test \"$3\" = '' || exit 46\n",
                        "test \"$4\" = --option-looking || exit 47\n",
                        "printf '%s\\n' '{\"event\":{\"session_id\":\"session-opaque\",\"model\":\"Provider/Model.Exact-CASE\",\"usage\":{\"input_tokens\":999999},\"final_message\":\"{\\\"answer\\\":42}\",\"claimed_verdict\":\"fail\",\"claimed_evidence\":\"fail\",\"claimed_charge\":999999,\"claimed_gpu_seconds\":999999}}'\n",
                        "printf '%s\\n' 'branch=adapter-test' >&2\n",
                        "sleep 0.1\n"
                    ),
                )
                .unwrap();
                fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
                let mut config = one_pool_config();
                let mut adapter = structured_adapter(&program);
                adapter.trace = Some(AdapterTrace {
                    stream: ScrapeStream::Stdout,
                    framing: TraceFraming::JsonLines,
                });
                config.adapters.insert("from-nix".to_owned(), adapter);
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon =
                    Daemon::open_with_executor(
                        config.clone(),
                        paths.clone(),
                        settings(),
                        executor.clone(),
                    )
                        .await
                        .unwrap();
                let unknown = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["must-not-run"],
                        "pool": "slot",
                        "adapter": "not-declared"
                    })))
                    .await
                    .unwrap_err();
                assert_eq!(unknown.code, WireErrorCode::InvalidParams);
                assert!(unknown.message.contains("unknown adapter"));
                assert!(daemon.handler.context.read().await.jobs.is_empty());
                assert!(!paths.events_dir().exists());
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["literal;$(not-a-shell)", "", "--option-looking"],
                        "pool": "slot",
                        "priority": "high",
                        "adapter": "from-nix",
                        "source": "manual",
                        "evidence": ["exit:0"],
                        "consumptionEstimate": 7
                    })))
                    .await
                    .unwrap();
                let job_id = admitted["job_id"].as_str().unwrap();
                let hook_status = daemon
                    .handler
                    .lease_status(Some(json!({"jobId": job_id})))
                    .await
                    .unwrap();
                assert_eq!(hook_status["held"], true);

                let finished = tokio::time::timeout(
                    Duration::from_secs(2),
                    daemon.completion_rx.recv(),
                )
                .await
                .unwrap()
                .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let terminal = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": admitted["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");

                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let enriched = daemon
                            .handler
                            .context
                            .read()
                            .await
                            .jobs
                            .get(&Uuid::parse_str(job_id).unwrap())
                            .and_then(|job| job.row.session_ref.as_deref())
                            == Some("session-opaque");
                        if enriched && paths.attestations_path().exists() {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();

                let attestation_line = fs::read_to_string(paths.attestations_path()).unwrap();
                let attestation: crate::witness::AttestationRecord =
                    serde_json::from_str(attestation_line.lines().next().unwrap()).unwrap();
                assert_eq!(attestation.payload["kind"], "adapter-scrape");
                assert_eq!(
                    attestation.payload["captures"]["model"],
                    "Provider/Model.Exact-CASE"
                );
                assert_eq!(
                    attestation.payload["captures"]["usage"]["input_tokens"],
                    999999
                );
                assert_eq!(attestation.payload["usageAuthority"], "advisory-only");
                let (report, witness) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                assert_eq!(witness.len(), 1);
                assert_eq!(witness[0].verdict, Verdict::Pass);
                assert_eq!(witness[0].gpu_seconds, None);
                assert_eq!(witness[0].charge, None);
                assert_eq!(witness[0].model, None);
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .jobs
                        .get(&Uuid::parse_str(job_id).unwrap())
                        .unwrap()
                        .row
                        .model
                        .as_deref(),
                    Some("Provider/Model.Exact-CASE")
                );
                let task_uuid = admitted["task_uuid"].as_str().unwrap();
                let before = daemon
                    .handler
                    .query("query.job", Some(json!({"id": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(
                    before["job"]["finalMessage"],
                    json!({
                        "value": "{\"answer\":42}",
                        "authority": "advisory-provider-capture",
                        "provenance": "adapter-scrape",
                    })
                );
                let canonical_before = json!({
                    "priority": before["job"]["priority"],
                    "pool": before["job"]["pool"],
                    "evidenceSpecs": before["job"]["evidenceSpecs"],
                    "evidenceResult": before["job"]["evidenceResult"],
                    "terminalVerdict": before["job"]["terminalVerdict"],
                    "charge": before["job"]["charge"],
                    "gpuSeconds": before["job"]["gpuSeconds"],
                    "canonicalGpuSeconds": before["job"]["canonicalGpuSeconds"],
                });
                assert_eq!(canonical_before["priority"], "high");
                assert_eq!(canonical_before["pool"], "slot");
                assert_eq!(canonical_before["evidenceSpecs"], json!(["exit:0"]));
                assert_eq!(canonical_before["evidenceResult"], "pass");
                assert_eq!(canonical_before["terminalVerdict"], "pass");
                assert_eq!(canonical_before["charge"], Value::Null);
                assert_eq!(canonical_before["gpuSeconds"], Value::Null);
                assert_eq!(canonical_before["canonicalGpuSeconds"], Value::Null);

                let trace = daemon
                    .handler
                    .query(
                        "query.trace",
                        Some(json!({"task": task_uuid, "limit": 100})),
                    )
                    .await
                    .unwrap();
                assert_eq!(trace["items"].as_array().unwrap().len(), 1);
                assert_eq!(
                    trace["items"][0]["authority"],
                    "advisory-provider-capture"
                );
                assert_eq!(trace["items"][0]["provenance"], "provider-capture");
                assert_eq!(
                    trace["items"][0]["payload"]["event"]["claimed_verdict"],
                    "fail"
                );
                assert_eq!(
                    trace["items"][0]["payload"]["event"]["claimed_charge"],
                    999999
                );
                let after = daemon
                    .handler
                    .query("query.job", Some(json!({"id": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(
                    canonical_before,
                    json!({
                        "priority": after["job"]["priority"],
                        "pool": after["job"]["pool"],
                        "evidenceSpecs": after["job"]["evidenceSpecs"],
                        "evidenceResult": after["job"]["evidenceResult"],
                        "terminalVerdict": after["job"]["terminalVerdict"],
                        "charge": after["job"]["charge"],
                        "gpuSeconds": after["job"]["gpuSeconds"],
                        "canonicalGpuSeconds": after["job"]["canonicalGpuSeconds"],
                    })
                );

                drop(daemon);
                tokio::task::yield_now().await;
                fs::remove_file(paths.attestations_path()).unwrap();
                let reopened = Daemon::open_with_executor(
                    config.clone(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                assert_eq!(
                    reopened
                        .handler
                        .context
                        .read()
                        .await
                        .query_rows
                        .values()
                        .next()
                        .unwrap()
                        .session_ref
                        .as_deref(),
                    Some("session-opaque")
                );
                assert_eq!(
                    reopened
                        .handler
                        .context
                        .read()
                        .await
                        .query_rows
                        .values()
                        .next()
                        .unwrap()
                        .model
                        .as_deref(),
                    Some("Provider/Model.Exact-CASE")
                );
                assert_eq!(
                    reopened
                        .handler
                        .query("query.job", Some(json!({"id": task_uuid})))
                        .await
                        .unwrap()["job"]["finalMessage"]["value"],
                    "{\"answer\":42}"
                );
                let repaired = fs::read_to_string(paths.attestations_path()).unwrap();
                assert_eq!(repaired.lines().count(), 1);
                let repaired: crate::witness::AttestationRecord =
                    serde_json::from_str(repaired.lines().next().unwrap()).unwrap();
                assert_eq!(repaired.payload["reconciledAfterRestart"], true);
                assert_eq!(repaired.payload["leaseEpoch"], 1);

                drop(reopened);
                let mut db = TaskDb::open(&paths.data_dir).await.unwrap();
                let projected = db
                    .get_row(Uuid::parse_str(admitted["task_uuid"].as_str().unwrap()).unwrap())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(projected.value("session_ref"), Some("session-opaque"));
                assert_eq!(projected.value("model"), Some("Provider/Model.Exact-CASE"));
                assert_eq!(projected.value("final_message"), Some("{\"answer\":42}"));
                drop(db);

                let deduplicated =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                assert_eq!(
                    fs::read_to_string(paths.attestations_path())
                        .unwrap()
                        .lines()
                        .count(),
                    1
                );
                drop(deduplicated);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_9_queries_and_trace_never_project_credential_values() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let credential = temp.path().join("provider-token");
                let credential_value = "wave-five-super-secret-value";
                fs::write(&credential, credential_value).unwrap();
                fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();

                let mut config = one_pool_config();
                config
                    .pools
                    .get_mut("slot")
                    .unwrap()
                    .credentials
                    .insert("provider-token".to_owned(), credential.clone());
                config.adapters.get_mut("shell").unwrap().trace = Some(AdapterTrace {
                    stream: ScrapeStream::Stdout,
                    framing: TraceFraming::JsonLines,
                });
                config.validate().unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                let watch_tail = daemon
                    .handler
                    .query("query.watch", Some(json!({})))
                    .await
                    .unwrap()["nextCursor"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let pool_change = daemon
                    .handler
                    .query(
                        "query.watch",
                        Some(json!({"after": watch_tail, "limit": 100})),
                    )
                    .await
                    .unwrap();
                assert!(pool_change["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|change| {
                        change["kind"] == "pool" && change["payload"]["update"] == "paused"
                    }));
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["/bin/true"],
                        "pool": "slot",
                        "priority": "high",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap();
                let proxy_attempt = daemon
                    .handler
                    .query(
                        "query.watch",
                        Some(json!({"method": "queue.resume", "params": {"all": true}})),
                    )
                    .await
                    .unwrap_err();
                assert_eq!(proxy_attempt.code, WireErrorCode::InvalidParams);
                assert!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .paused_pools
                        .contains("slot"),
                    "read-only query RPC changed queue state"
                );

                let responses = vec![
                    daemon
                        .handler
                        .query("query.jobs", Some(json!({})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.job", Some(json!({"id": task_uuid})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.log", Some(json!({"task": task_uuid})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.proof", Some(json!({"task": task_uuid})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.trace", Some(json!({"task": task_uuid})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.producers", Some(json!({})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.status", Some(json!({})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.render", Some(json!({"format": "json"})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.standup", Some(json!({})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.pools", Some(json!({})))
                        .await
                        .unwrap(),
                    daemon
                        .handler
                        .query("query.watch", Some(json!({})))
                        .await
                        .unwrap(),
                ];
                assert_eq!(
                    responses[0]["items"][0]["credentialNames"],
                    json!(["provider-token"])
                );
                assert_eq!(
                    responses[4]["generations"][0]["reason"],
                    "capture-not-retained-for-generation"
                );
                let encoded = serde_json::to_string(&responses).unwrap();
                assert!(!encoded.contains(credential_value));
                assert!(!encoded.contains(credential.to_string_lossy().as_ref()));
                assert_eq!(
                    fs::metadata(&credential).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_ack_precedes_scrape_and_shutdown_joins_attestation_writer() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let program = temp.path().join("checkpoint-agent");
                fs::write(
                    &program,
                    concat!(
                        "#!/bin/sh\n",
                        "printf '%s\\n' '{\"event\":{\"session_id\":\"blocked-attestation\",\"model\":\"Exact/Blocked\",\"usage\":{\"tokens\":3}}}'\n",
                        "printf '%s\\n' 'branch=shutdown-test' >&2\n"
                    ),
                )
                .unwrap();
                fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
                let mut config = one_pool_config();
                config
                    .adapters
                    .insert("blocked".to_owned(), structured_adapter(&program));
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                    .await
                    .unwrap();
                let handler = daemon.handler.clone();
                let attestation_path = paths.attestations_path();
                let lock = OpenOptions::new()
                    .create(true)
                    .read(true)
                    .append(true)
                    .open(&attestation_path)
                    .unwrap();
                lock.lock_exclusive().unwrap();

                let (shutdown, shutdown_rx) = watch::channel(false);
                let mut daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let admitted = handler
                    .enqueue(Some(json!({
                        "argv": ["work"],
                        "pool": "slot",
                        "adapter": "blocked",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let terminal = tokio::time::timeout(
                    Duration::from_secs(2),
                    handler.await_job(Some(json!({"task_uuid": admitted["task_uuid"]}))),
                )
                .await
                .expect("terminal witness acknowledgement waited on scrape")
                .unwrap();
                assert_eq!(terminal["verdict"], "pass");

                shutdown.send(true).unwrap();
                assert!(tokio::time::timeout(Duration::from_millis(50), &mut daemon_task)
                    .await
                    .is_err());
                fs2::FileExt::unlock(&lock).unwrap();
                tokio::time::timeout(Duration::from_secs(2), daemon_task)
                    .await
                    .expect("daemon did not join the post-ack writer")
                    .unwrap()
                    .unwrap();
                assert!(verify_attestations(&attestation_path).unwrap().ok);
            })
            .await;
    }

    #[test]
    fn recovery_resume_scrapes_before_executor_capture_truncation() {
        let temp = tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        let program = temp.path().join("agent");
        let mut config = one_pool_config();
        config
            .adapters
            .insert("resumable".to_owned(), structured_adapter(&program));
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap());
        let mut row = durable_row(Uuid::new_v4(), "resume-key", 2);
        row.adapter = "resumable".to_owned();
        row.argv = vec!["continue-work".to_owned()];
        row.attempt = 2;
        let capture_paths = executor.paths(&ExecutionIdentity {
            job_id: row.uuid,
            task_uuid: Some(row.uuid),
        });
        fs::create_dir_all(capture_paths.stdout.parent().unwrap()).unwrap();
        fs::create_dir_all(capture_paths.capture_generation.parent().unwrap()).unwrap();
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":1,"leaseEpoch":1}"#,
        )
        .unwrap();
        let original = b"{\"event\":{\"session_id\":\"resume-me\",\"model\":\"Exact/Model\",\"usage\":{\"input_tokens\":5}}}\n";
        fs::write(&capture_paths.stdout, original).unwrap();
        fs::write(&capture_paths.stderr, b"branch=recovery\n").unwrap();
        let action = RecoveryAction::RePresent {
            row: Box::new(row.clone()),
            trigger: crate::evidence::RetryTrigger::PoolReturn,
            previous_witness_seq: 1,
            previous_attempt: 1,
            previous_lease_epoch: 1,
        };
        let attestation_path = temp.path().join("attestations.jsonl");
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":0,"leaseEpoch":1}"#,
        )
        .unwrap();
        assert!(
            recovery_adapter_invocation(&config, &action, &row, &executor, &attestation_path)
                .unwrap_err()
                .to_string()
                .contains("does not match prior attempt")
        );
        assert_eq!(fs::read(&capture_paths.stdout).unwrap(), original);
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":1,"leaseEpoch":1}"#,
        )
        .unwrap();
        let blocked_attestation = temp.path().join("blocked-attestation");
        fs::create_dir(&blocked_attestation).unwrap();
        assert!(recovery_adapter_invocation(
            &config,
            &action,
            &row,
            &executor,
            &blocked_attestation,
        )
        .is_err());
        assert_eq!(fs::read(&capture_paths.stdout).unwrap(), original);

        let (invocation, captures) =
            recovery_adapter_invocation(&config, &action, &row, &executor, &attestation_path)
                .unwrap();
        assert_eq!(
            invocation.argv,
            [
                program.to_string_lossy().into_owned(),
                "--resume".to_owned(),
                "resume-me".to_owned(),
                "--model".to_owned(),
                "Exact/Model".to_owned(),
                "continue-work".to_owned(),
            ]
        );
        assert_eq!(
            captures.unwrap().captures["branch"],
            Value::String("recovery".to_owned())
        );
        assert_eq!(fs::read(&capture_paths.stdout).unwrap(), original);
        assert!(verified_adapter_attestation_captures(
            &attestation_path,
            row.uuid,
            &row.adapter,
            1,
            1,
        )
        .unwrap()
        .is_some());

        fs::write(
            &capture_paths.stdout,
            b"{\"event\":{\"model\":\"Exact/Model\"}}\n",
        )
        .unwrap();
        let missing_attestation = temp.path().join("missing-attestation.jsonl");
        assert!(matches!(
            recovery_adapter_invocation(&config, &action, &row, &executor, &missing_attestation),
            Err(DaemonError::Adapter(AdapterError::MissingCapture { .. }))
        ));
        assert_eq!(
            fs::read(&capture_paths.stdout).unwrap(),
            b"{\"event\":{\"model\":\"Exact/Model\"}}\n"
        );
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":1,"leaseEpoch":1}"#,
        )
        .unwrap();
        fs::write(&capture_paths.stdout, b"").unwrap();
        let (fallback, captures) =
            recovery_adapter_invocation(&config, &action, &row, &executor, &attestation_path)
                .unwrap();
        assert_eq!(fallback.argv[2], "resume-me");
        assert_eq!(fallback.argv[4], "Exact/Model");
        assert_eq!(captures.unwrap().captures["usage"]["input_tokens"], 5);
        let mut advisory_row = row;
        advisory_row.model = Some("Exact/Model".to_owned());
        let mut plan = empty_plan();
        plan.rows.push(crate::recovery::RecoveryRow {
            row: advisory_row.clone(),
            state: RecoveryRowState::Pending,
            labor_class: LaborClass::Fresh,
            guardrail_depth: 0,
        });
        plan.actions.push(action);
        plan.rows[0].row.session_ref = None;
        plan.rows[0].row.model = None;
        hydrate_represent_adapter_metadata(&mut plan, &config, &executor, &attestation_path)
            .unwrap();
        assert_eq!(plan.rows[0].row.session_ref.as_deref(), Some("resume-me"));
        assert_eq!(plan.rows[0].row.model.as_deref(), Some("Exact/Model"));
        let mut deleted_plan = empty_plan();
        let mut deleted_row = advisory_row.clone();
        deleted_row.session_ref = None;
        deleted_row.model = None;
        deleted_plan.rows.push(crate::recovery::RecoveryRow {
            row: deleted_row,
            state: RecoveryRowState::Deleted,
            labor_class: LaborClass::Fresh,
            guardrail_depth: 0,
        });
        fs::write(
            &capture_paths.capture_generation,
            r#"{"attempt":2,"leaseEpoch":2}"#,
        )
        .unwrap();
        fs::write(&capture_paths.stdout, original).unwrap();
        hydrate_completed_adapter_metadata(&mut deleted_plan, &config, &executor);
        assert_eq!(
            deleted_plan.rows[0].row.session_ref.as_deref(),
            Some("resume-me")
        );
        assert_eq!(
            deleted_plan.rows[0].row.model.as_deref(),
            Some("Exact/Model")
        );
        let mut adopted_plan = empty_plan();
        let mut adopted_row = advisory_row.clone();
        adopted_row.session_ref = None;
        adopted_row.model = None;
        adopted_plan.rows.push(crate::recovery::RecoveryRow {
            row: adopted_row,
            state: RecoveryRowState::AdoptedRunning,
            labor_class: LaborClass::Fresh,
            guardrail_depth: 0,
        });
        adopted_plan.actions.push(RecoveryAction::AdoptRunning {
            identity: RecoveryIdentity::Task(advisory_row.uuid),
            unit: executor.unit_name(&ExecutionIdentity {
                job_id: advisory_row.uuid,
                task_uuid: Some(advisory_row.uuid),
            }),
            invocation_id: "attempt-2-invocation".to_owned(),
            attempt: 2,
            lease_epoch: 2,
            labor_class: Some(LaborClass::Fresh),
        });
        hydrate_adopted_adapter_metadata(&mut adopted_plan, &attestation_path).unwrap();
        assert_eq!(
            adopted_plan.rows[0].row.session_ref.as_deref(),
            Some("resume-me")
        );
        assert_eq!(
            adopted_plan.rows[0].row.model.as_deref(),
            Some("Exact/Model")
        );
        assert!(recovered_model_is_advisory(
            &adopted_plan.rows[0].row,
            None,
            true,
        ));
        let recovered_job = Job {
            job_id: advisory_row.uuid,
            task_uuid: Some(advisory_row.uuid),
            row: advisory_row,
            invocation: fallback,
            labor_class: LaborClass::Fresh,
            state: JobState::Running,
            lease_id: None,
            adopted: false,
            adopted_invocation_id: None,
            model_is_advisory: true,
        };
        assert_eq!(canonical_job_model(&recovered_job), None);
    }

    #[test]
    fn job_barriers_are_deterministic_and_empty_drain_barriers_are_immediate() {
        let mut tracker = BarrierTracker::with_namespace(41);
        let barrier = tracker.register_job("task-1", 1);
        tracker.complete_job("task-1", json!({"verdict": "pass", "attempt": 1}));
        assert_eq!(tracker.retained_entry_count(), 0);
        assert_eq!(parse_job_barrier(&barrier).unwrap(), ("task-1", 1));
        assert_eq!(tracker.snapshot(Vec::new()), "barrier:drain:41:1");
        assert!(matches!(
            tracker.wait_barrier("barrier:drain:41:1").unwrap(),
            WaitRegistration::Ready(_)
        ));
        assert_eq!(
            BarrierTracker::with_namespace(42).snapshot(Vec::new()),
            "barrier:drain:42:1"
        );
    }

    #[test]
    fn fs2_completed_bookkeeping_is_bounded_and_terminal_parents_retire() {
        let mut tracker = BarrierTracker::with_namespace(7);
        for sequence in 0..10_000 {
            let stable = format!("task-{sequence}");
            tracker.register_job(&stable, 1);
            tracker.complete_job(&stable, json!({"attempt": 1, "sequence": sequence}));
        }
        assert_eq!(tracker.retained_entry_count(), 0);
        for sequence in 0..10_000 {
            tracker.snapshot([format!("still-running-{sequence}")]);
        }
        assert_eq!(
            tracker.retained_entry_count(),
            UNCLAIMED_DRAIN_BARRIER_LIMIT
        );

        for _ in 0..10_000 {
            let WaitRegistration::Pending(receiver) = tracker.wait_job("stuck-job") else {
                panic!("an active job wait must register");
            };
            drop(receiver);
        }
        tracker.register_job("prune-trigger", 1);
        assert!(
            tracker.job_waiters.is_empty(),
            "closed waiter senders are evicted on the next tracker operation"
        );

        let pending_barrier = tracker.snapshot(["stuck-job".to_owned()]);
        for _ in 0..10_000 {
            let WaitRegistration::Pending(receiver) =
                tracker.wait_barrier(&pending_barrier).unwrap()
            else {
                panic!("an incomplete drain barrier must register");
            };
            drop(receiver);
        }
        tracker.register_job("second-prune-trigger", 1);
        assert!(
            tracker
                .barriers
                .get(&pending_barrier)
                .unwrap()
                .waiters
                .is_empty(),
            "closed barrier waiter senders are evicted on the next tracker operation"
        );

        let mut guardrails = GuardrailState::new(GuardrailConfig::default()).unwrap();
        for sequence in 0..10_000 {
            let stable = format!("parent-{sequence}");
            guardrails.register_parent(
                stable.clone(),
                ParentInfo {
                    parent_uuid: stable.clone(),
                    depth: 0,
                    outstanding: 0,
                    no_enqueue: false,
                    terminal: false,
                },
            );
            guardrails.retire_parent(&stable);
        }
        assert_eq!(guardrails.parent_count(), 0);

        guardrails.register_parent(
            "parent-with-child",
            ParentInfo {
                parent_uuid: "parent-with-child".to_owned(),
                depth: 0,
                outstanding: 1,
                no_enqueue: false,
                terminal: false,
            },
        );
        guardrails.retire_parent("parent-with-child");
        assert!(guardrails.parent("parent-with-child").unwrap().terminal);
        guardrails
            .rollback_child_charge("parent-with-child")
            .unwrap();
        assert!(guardrails.parent("parent-with-child").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn query_pools_exposes_the_active_window_reset() {
        let temp = tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
            .with_systemd_run(temp.path().join("absent-systemd-run"))
            .with_unit_probe(ExitFileProbe);
        let mut config = window_pool_config();
        config.pools.get_mut("api").unwrap().usage_meter = Some(UsageMeterConfig {
            argv: vec!["meter-feeder".to_owned()],
            poll_interval_sec: 120,
            budget_class: MeterBudgetClass::Programmatic,
        });
        let meter_path = usage_meter_event_path(&paths.state_dir, "api");
        let daemon = Daemon::open_with_executor(config, paths, settings(), executor)
            .await
            .unwrap();
        let id = Uuid::new_v4();
        daemon
            .handler
            .context
            .write()
            .await
            .lease
            .admit(
                LeaseRequest {
                    job_id: id.to_string(),
                    // Explicit acquire/release reservations are held by the
                    // daemon itself. They vanish with its epoch on restart;
                    // unlike execution leases, they must never name a
                    // fictional job unit or be physically preempted.
                    unit: "tally-daemon.service".to_owned(),
                    pools: vec!["api".to_owned()],
                    priority: Priority::Medium,
                    admission_key: Some(format!("{id}:1")),
                    consumption_estimate: Some(40),
                    scheduling_group: LeaseSchedulingGroup::Standalone,
                },
                Utc::now(),
            )
            .unwrap();
        let pools = daemon
            .handler
            .query("query.pools", Some(json!({})))
            .await
            .unwrap();
        assert_eq!(pools["pools"][0]["consumptionUsed"], 40);
        assert_eq!(pools["pools"][0]["remainingBudget"], 60);
        assert!(pools["pools"][0]["resetAt"].as_str().is_some());

        fs::create_dir_all(meter_path.parent().unwrap()).unwrap();
        let observed_at = Utc::now();
        fs::write(
            &meter_path,
            serde_json::to_vec(&json!({
                "pool": "api",
                "budget_class": "programmatic",
                "utilization_pct": 80.0,
                "weekly_utilization_pct": 81.0,
                "reset_at": (observed_at + chrono::Duration::hours(1)).to_rfc3339(),
                "observed_at": observed_at.to_rfc3339(),
            }))
            .unwrap(),
        )
        .unwrap();
        let clamped = daemon
            .handler
            .query("query.pools", Some(json!({})))
            .await
            .unwrap();
        assert_eq!(clamped["pools"][0]["selfUtilizationPct"], 40.0);
        assert_eq!(clamped["pools"][0]["effectiveUtilizationPct"], 80.0);
        assert_eq!(clamped["pools"][0]["remainingBudget"], 20);
        assert_eq!(clamped["pools"][0]["signal"], "STOP");

        fs::write(
            &meter_path,
            serde_json::to_vec(&json!({
                "pool": "api",
                "budget_class": "programmatic",
                "utilization_pct": 10.0,
                "reset_at": (observed_at + chrono::Duration::hours(1)).to_rfc3339(),
                "observed_at": observed_at.to_rfc3339(),
            }))
            .unwrap(),
        )
        .unwrap();
        let cannot_grant = daemon
            .handler
            .query("query.pools", Some(json!({})))
            .await
            .unwrap();
        assert_eq!(cannot_grant["pools"][0]["effectiveUtilizationPct"], 40.0);
        assert_eq!(cannot_grant["pools"][0]["remainingBudget"], 60);

        fs::write(
            &meter_path,
            br#"{"pool":"wrong","budget_class":"programmatic","utilization_pct":99,"reset_at":"2999-01-01T00:00:00Z","observed_at":"2999-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let ignored = daemon
            .handler
            .query("query.pools", Some(json!({})))
            .await
            .unwrap();
        assert_eq!(ignored["pools"][0]["effectiveUtilizationPct"], 40.0);
        assert_eq!(ignored["pools"][0]["remainingBudget"], 60);
    }

    #[test]
    fn built_in_usage_feeder_routes_tokens_and_can_only_clamp_headroom_downward() {
        let temp = tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let mut config = window_pool_config();
        let captures = ScrapeResult {
            captures: BTreeMap::from([(
                "usage".to_owned(),
                json!({"input_tokens": 30, "output_tokens": 50}),
            )]),
        };
        assert!(
            feed_scraped_usage(&state_dir, &config.pools, &["api".to_owned()], &captures,)
                .is_empty()
        );
        let event = read_usage_meter(&state_dir, "api", 3600, Utc::now()).unwrap();
        assert_eq!(event.utilization_pct, 80.0);

        let projection = query_pools(&[PoolHeadroomFact {
            pool: "api".to_owned(),
            capacity: 1,
            held: 0,
            queued: 0,
            consumption: Some(WindowConsumptionFact {
                used: 40,
                cap: 100,
                reset_at: None,
            }),
            meter_utilization_pct: Some(event.utilization_pct),
            weekly_utilization_pct: None,
        }])
        .unwrap();
        assert_eq!(projection.pools[0].self_utilization_pct, 40.0);
        assert_eq!(projection.pools[0].effective_utilization_pct, 80.0);

        let path = usage_meter_event_path(&state_dir, "api");
        let low = ScrapeResult {
            captures: BTreeMap::from([("usage".to_owned(), json!({"total_tokens": 10}))]),
        };
        assert!(
            feed_scraped_usage(&state_dir, &config.pools, &["api".to_owned()], &low,).is_empty()
        );
        let low = read_usage_meter(&state_dir, "api", 3600, Utc::now()).unwrap();
        let projection = query_pools(&[PoolHeadroomFact {
            pool: "api".to_owned(),
            capacity: 1,
            held: 0,
            queued: 0,
            consumption: Some(WindowConsumptionFact {
                used: 40,
                cap: 100,
                reset_at: None,
            }),
            meter_utilization_pct: Some(low.utilization_pct),
            weekly_utilization_pct: None,
        }])
        .unwrap();
        assert_eq!(projection.pools[0].effective_utilization_pct, 40.0);

        let valid_bytes = fs::read(&path).unwrap();
        for malformed in [
            json!({"usage": {"total_tokens": "80"}}),
            json!({"usage": {"input_tokens": -1, "output_tokens": 4}}),
            json!({"usage": {"input_tokens": 0, "output_tokens": 0}}),
        ] {
            let captures = ScrapeResult {
                captures: malformed.as_object().unwrap().clone().into_iter().collect(),
            };
            assert!(
                feed_scraped_usage(&state_dir, &config.pools, &["api".to_owned()], &captures,)
                    .is_empty()
            );
            assert_eq!(fs::read(&path).unwrap(), valid_bytes);
        }

        config.pools.get_mut("api").unwrap().usage_meter = Some(UsageMeterConfig {
            argv: vec!["external-meter".to_owned()],
            poll_interval_sec: 120,
            budget_class: MeterBudgetClass::Programmatic,
        });
        fs::remove_file(&path).unwrap();
        assert!(
            feed_scraped_usage(&state_dir, &config.pools, &["api".to_owned()], &captures,)
                .is_empty()
        );
        assert!(
            !path.exists(),
            "an external meter must remain the sole authority"
        );

        let now = Utc::now();
        write_usage_meter(
            &state_dir,
            &UsageMeterObservation {
                pool: "api".to_owned(),
                budget_class: MeterBudgetClass::Programmatic,
                utilization_pct: 99.0,
                weekly_utilization_pct: None,
                observed_at: (now - chrono::Duration::seconds(121)).to_rfc3339(),
                reset_at: (now + chrono::Duration::hours(1)).to_rfc3339(),
            },
        )
        .unwrap();
        assert!(read_usage_meter(&state_dir, "api", 120, now).is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hard_preemption_witnesses_victim_before_replacement_runs() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut fast_settings = settings();
                fast_settings.yield_grace = Duration::from_millis(10);
                let mut daemon = Daemon::open_with_executor(
                    hard_preempt_config(),
                    paths.clone(),
                    fast_settings,
                    executor,
                )
                .await
                .unwrap();
                let low = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["sleep", "30"],
                        "pool": "slot",
                        "priority": "low",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let urgent = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "priority": "interrupt",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                assert_eq!(low["state"], "running");
                assert_eq!(urgent["state"], "queued");
                tokio::time::sleep(Duration::from_millis(20)).await;
                Daemon::tick_leases(daemon.handler.clone()).await.unwrap();

                let low_result = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": low["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(low_result["verdict"], "preempted");
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if daemon
                            .handler
                            .context
                            .read()
                            .await
                            .jobs
                            .get(&Uuid::parse_str(urgent["job_id"].as_str().unwrap()).unwrap())
                            .is_some_and(|job| job.state == JobState::Completed)
                        {
                            break;
                        }
                        let finished = daemon.completion_rx.recv().await.unwrap();
                        daemon.finish_job(finished).await.unwrap();
                    }
                })
                .await
                .unwrap();
                let urgent_result = tokio::time::timeout(
                    Duration::from_secs(2),
                    daemon
                        .handler
                        .await_job(Some(json!({"task_uuid": urgent["task_uuid"]}))),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(urgent_result["verdict"], "pass");
                let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                assert_eq!(records[0].verdict, Verdict::Preempted);
                assert_eq!(records[1].verdict, Verdict::Pass);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn interrupt_cooldown_waits_for_active_work_then_holds_the_pool() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let active = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["sleep", "0.12"],
                            "pool": "slot",
                            "priority": "low",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                let cooldown = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["sleep", "0.05"],
                            "pool": "slot",
                            "priority": "interrupt",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"],
                            "noEnqueue": true
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(active["state"], "running");
                assert_eq!(cooldown["state"], "queued");

                let active_result = client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": active["task_uuid"]})),
                    )
                    .await
                    .unwrap();
                assert_eq!(active_result["verdict"], "pass");
                let cooldown_result = client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": cooldown["task_uuid"]})),
                    )
                    .await
                    .unwrap();
                assert_eq!(cooldown_result["verdict"], "pass");

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 2);
                assert!(records.iter().all(|record| record.verdict == Verdict::Pass));
                assert_eq!(
                    records[0].task_uuid.as_deref(),
                    active["task_uuid"].as_str()
                );
                assert_eq!(
                    records[1].task_uuid.as_deref(),
                    cooldown["task_uuid"].as_str()
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_transport_loss_retains_the_lease_until_authoritative_completion() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let calls = Arc::new(AtomicUsize::new(0));
                let release = Arc::new(AtomicBool::new(false));
                let transport = RecoveringRemoteTransport {
                    calls: calls.clone(),
                    release: release.clone(),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_remote_transport(transport);
                let mut daemon = Daemon::open_with_executor(
                    remote_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["opaque-worker-command", "two words", "$HOME"],
                        "pool": "slot",
                        "executor": "worker",
                        "priority": "high",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                assert_eq!(admitted["state"], "running");

                tokio::time::timeout(Duration::from_secs(1), async {
                    while calls.load(Ordering::SeqCst) < 2 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.lease.engine().held_in_pool("slot").unwrap(), 1);
                    let job_id = Uuid::parse_str(admitted["job_id"].as_str().unwrap()).unwrap();
                    assert_eq!(context.jobs[&job_id].state, JobState::Running);
                }
                assert!(daemon.completion_rx.try_recv().is_err());
                let (_, witness_before) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(witness_before.is_empty());

                release.store(true, Ordering::Release);
                let finished =
                    tokio::time::timeout(Duration::from_secs(1), daemon.completion_rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                daemon.finish_job(finished).await.unwrap();
                let result = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": admitted["task_uuid"]})))
                    .await
                    .unwrap();
                assert_eq!(result["verdict"], "pass");
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .lease
                        .engine()
                        .held_in_pool("slot")
                        .unwrap(),
                    0
                );
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].executor.as_deref(), Some("worker"));
                assert_eq!(calls.load(Ordering::SeqCst), 2);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_restart_probes_and_adopts_remote_work_without_an_ensure() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let task_uuid = Uuid::new_v4();
                let mut row = durable_row(task_uuid, "restart-remote", 1);
                row.executor = Some("worker".to_owned());
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(row).unwrap(),
                )
                .unwrap();

                let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
                let transport = RestartRemoteTransport {
                    calls: calls.clone(),
                    attempt: 1,
                    lease_epoch: 1,
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_remote_transport(transport);
                let daemon = Daemon::open_with_executor(
                    remote_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                assert_eq!(daemon.initial_jobs.len(), 1);
                assert!(daemon.initial_jobs[0].adopted);
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .lease
                        .engine()
                        .held_in_pool("slot")
                        .unwrap(),
                    1
                );

                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let result = tokio::time::timeout(
                    Duration::from_secs(1),
                    client.call(
                        "queue.await_job",
                        Some(json!({"task_uuid": task_uuid.to_string()})),
                    ),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(result["verdict"], "pass");
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();

                let calls = calls.lock().unwrap();
                assert_eq!(calls.len(), 2);
                assert!(matches!(calls[0], RemoteExecutorRequest::Probe { .. }));
                assert!(matches!(calls[1], RemoteExecutorRequest::Adopt { .. }));
                assert!(!calls
                    .iter()
                    .any(|request| matches!(request, RemoteExecutorRequest::Ensure { .. })));
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].executor.as_deref(), Some("worker"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_joins_an_in_flight_lease_tick_before_releasing_the_lock() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let mut daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let notify_path = temp.path().join("notify.sock");
                let notify_socket = UnixDatagram::bind(&notify_path).unwrap();
                notify_socket
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                daemon.notifier = SystemdNotifier::with_socket(notify_path, None);
                let (tick_started_tx, mut tick_started_rx) = mpsc::unbounded_channel();
                let (release_tick_tx, release_tick_rx) = watch::channel(false);
                daemon.lease_tick_hook = Some(LeaseTickHook {
                    started: tick_started_tx,
                    release: release_tick_rx,
                });
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let mut daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));

                tokio::time::timeout(Duration::from_secs(1), tick_started_rx.recv())
                    .await
                    .expect("lease tick must start")
                    .expect("lease tick hook must remain open");
                shutdown_tx.send(true).unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_millis(50), &mut daemon_task)
                        .await
                        .is_err(),
                    "shutdown must join, not detach or abort, the in-flight lease tick"
                );
                assert!(
                    acquire_daemon_lock(&paths.state_dir).is_err(),
                    "the daemon lock must fence a replacement until the tick finishes"
                );
                let mut notifications = Vec::new();
                let mut buffer = [0_u8; 64];
                for _ in 0..2 {
                    let received = notify_socket.recv(&mut buffer).unwrap();
                    notifications
                        .push(std::str::from_utf8(&buffer[..received]).unwrap().to_owned());
                }
                assert_eq!(
                    notifications,
                    ["READY=1\nSTATUS=tally daemon ready", "STOPPING=1"]
                );

                release_tick_tx.send(true).unwrap();
                tokio::time::timeout(Duration::from_secs(2), &mut daemon_task)
                    .await
                    .expect("shutdown must finish after the tick")
                    .expect("daemon task must not panic")
                    .expect("daemon shutdown must succeed");
                drop(acquire_daemon_lock(&paths.state_dir).unwrap());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_replica_commit_does_not_stall_rpc_or_late_wait() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                drop(WitnessLedger::open(paths.witness_path()).unwrap());
                let epoch = bump_epoch(&paths.state_dir).unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let (commit_started_tx, commit_started_rx) = oneshot::channel();
                let release_commit = Arc::new(AtomicBool::new(false));
                let daemon = Daemon::build(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                    epoch,
                    empty_plan(),
                    Box::new(StallingCommitter {
                        started: Some(commit_started_tx),
                        release: release_commit.clone(),
                    }),
                )
                .unwrap();
                let context = daemon.handler.context.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));

                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let admitted = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["true"],
                            "pool": "slot",
                            "priority": "high",
                            "adapter": "shell",
                            "source": "orchestrator",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(admitted["task_uuid"], admitted["job_id"]);
                let durable_job_id = admitted["job_id"].as_str().unwrap();
                assert_eq!(
                    context
                        .read()
                        .await
                        .guardrails
                        .parent(durable_job_id)
                        .unwrap()
                        .parent_uuid,
                    durable_job_id
                );
                commit_started_rx.await.unwrap();

                tokio::time::timeout(
                    Duration::from_millis(250),
                    client.call("query.status", Some(json!({}))),
                )
                .await
                .expect("the socket must stay responsive while commit is stalled")
                .unwrap();

                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let complete = context
                            .read()
                            .await
                            .jobs
                            .values()
                            .all(|job| job.state == JobState::Completed);
                        if complete {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();

                let task_uuid = admitted["task_uuid"].as_str().unwrap();
                let waited = tokio::time::timeout(
                    Duration::from_millis(100),
                    client.call("queue.await_job", Some(json!({"task_uuid": task_uuid}))),
                )
                .await
                .expect("a late wait must resolve immediately")
                .unwrap();
                assert_eq!(waited["verdict"], "pass");

                let barrier = admitted["barrier"].as_str().unwrap();
                let barrier_result = client
                    .call("queue.await_barrier", Some(json!({"barrier": barrier})))
                    .await
                    .unwrap();
                assert_eq!(barrier_result["complete"], true);
                assert!(paths.events_dir().read_dir().unwrap().next().is_some());
                assert!(paths.witness_path().metadata().unwrap().len() > 0);

                release_commit.store(true, Ordering::Release);
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                assert!(!paths.socket.exists());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_never_detaches_a_stalled_replica_writer() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                drop(WitnessLedger::open(paths.witness_path()).unwrap());
                let epoch = bump_epoch(&paths.state_dir).unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let (commit_started_tx, commit_started_rx) = oneshot::channel();
                let release_commit = Arc::new(AtomicBool::new(false));
                let daemon = Daemon::build(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                    epoch,
                    empty_plan(),
                    Box::new(StallingCommitter {
                        started: Some(commit_started_tx),
                        release: release_commit.clone(),
                    }),
                )
                .unwrap();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["true"],
                            "pool": "slot",
                            "priority": "high",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                commit_started_rx.await.unwrap();
                shutdown_tx.send(true).unwrap();
                tokio::time::sleep(Duration::from_millis(1_100)).await;
                assert!(!daemon_task.is_finished());
                assert!(acquire_daemon_lock(&paths.state_dir).is_err());

                release_commit.store(true, Ordering::Release);
                tokio::time::timeout(Duration::from_secs(2), daemon_task)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                drop(acquire_daemon_lock(&paths.state_dir).unwrap());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restart_re_adopts_an_in_flight_multi_pool_job_with_the_complete_set() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                assert_eq!(bump_epoch(&paths.state_dir).unwrap(), 1);
                let mut row = durable_row(Uuid::new_v4(), "restart-multi-pool", 1);
                row.pools = vec!["slot".to_owned(), "zeta".to_owned()];
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(row.clone()).unwrap(),
                )
                .unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(RunningProbe {
                        attempt: 1,
                        lease_epoch: 1,
                    });

                let daemon = Daemon::open_with_executor(
                    two_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                assert_eq!(daemon.handler.context.read().await.epoch, 2);
                assert_eq!(daemon.initial_jobs.len(), 1);
                assert!(daemon.initial_jobs[0].adopted);
                assert_eq!(daemon.initial_jobs[0].row.pools, ["slot", "zeta"]);
                let context = daemon.handler.context.read().await;
                let recovered = &context.jobs[&row.uuid];
                assert!(recovered.adopted);
                assert_eq!(recovered.row.pools, ["slot", "zeta"]);
                assert!(recovered.lease_id.is_some());
                assert_eq!(context.lease.engine().held_in_pool("slot").unwrap(), 1);
                assert_eq!(context.lease.engine().held_in_pool("zeta").unwrap(), 1);
                assert_eq!(context.lease.engine().queue_len(), 0);
                drop(context);

                let lease_log =
                    fs::read_to_string(paths.state_dir.join(crate::lease::LEASE_EVENTS_FILE))
                        .unwrap();
                assert!(lease_log.contains(r#""pools":["slot","zeta"]"#));
                assert!(!lease_log.contains(r#""kind":"released""#));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restart_reconstructs_terminal_wait_without_reexecution() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let first = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                let first_context = first.handler.context.clone();
                let (first_shutdown, first_shutdown_rx) = watch::channel(false);
                let first_task = tokio::task::spawn_local(first.run_until(first_shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let admitted = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["true"],
                            "pool": "slot",
                            "priority": "high",
                            "adapter": "shell",
                            "source": "orchestrator",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                assert_eq!(admitted["job_id"], task_uuid);
                let barrier = admitted["barrier"].as_str().unwrap().to_owned();
                let first_result = client
                    .call("queue.await_job", Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(first_result["verdict"], "pass");
                let first_epoch = first_context.read().await.epoch;
                first_shutdown.send(true).unwrap();
                first_task.await.unwrap().unwrap();
                drop(client);
                drop(first_context);
                tokio::task::yield_now().await;

                let witness_before = fs::read(paths.witness_path()).unwrap();
                let exit_record = paths
                    .state_dir
                    .join(crate::executor::UNIT_EXIT_DIRECTORY)
                    .join(format!("{task_uuid}.json"));
                let exit_before = fs::read(&exit_record).unwrap();

                let second = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                assert_eq!(second.handler.context.read().await.epoch, first_epoch + 1);
                assert!(second.initial_jobs.is_empty());
                let (second_shutdown, second_shutdown_rx) = watch::channel(false);
                let second_task = tokio::task::spawn_local(second.run_until(second_shutdown_rx));
                let restarted_client = RpcClient::connect(&paths.socket).await.unwrap();
                let late = tokio::time::timeout(
                    Duration::from_millis(100),
                    restarted_client.call("queue.await_job", Some(json!({"task_uuid": task_uuid}))),
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(late["verdict"], "pass");
                let late_barrier = restarted_client
                    .call("queue.await_barrier", Some(json!({"barrier": barrier})))
                    .await
                    .unwrap();
                assert_eq!(late_barrier["complete"], true);
                second_shutdown.send(true).unwrap();
                second_task.await.unwrap().unwrap();

                assert_eq!(fs::read(paths.witness_path()).unwrap(), witness_before);
                assert_eq!(fs::read(exit_record).unwrap(), exit_before);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn restart_finishes_reuse_event_without_reexecution() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                prepare_paths(&paths).unwrap();
                let artifact_hash = format!("sha256:{}", "a".repeat(64));
                let original = durable_row(Uuid::new_v4(), "dedup:crash", 1);
                write_enqueue_event_atomic(
                    &paths.events_dir(),
                    &DurableEnqueueEvent::new(original.clone()).unwrap(),
                )
                .unwrap();
                let mut ledger = WitnessLedger::open(paths.witness_path()).unwrap();
                let pass = ledger
                    .append(WitnessBody {
                        task_uuid: Some(original.uuid.to_string()),
                        transition_timestamp: Utc::now()
                            .to_rfc3339_opts(SecondsFormat::Millis, true),
                        verdict: Verdict::Pass,
                        exit_code: 0,
                        artifact_content_hash: Some(artifact_hash.clone()),
                        gpu_seconds: Some(0.0),
                        wall_clock: 0.0,
                        attempt: 1,
                        lease_epoch: 1,
                        dedup_key: original.dedup_key.clone(),
                        payload_hash: original.payload_hash.clone(),
                        brief_hash: original.brief_hash.clone(),
                        orchestration: original.orchestration.clone(),
                        labor_class: LaborClass::Fresh,
                        trace_ref: None,
                        pools: Some(vec!["slot".to_owned()]),
                        executor: None,
                        charge: None,
                        model: None,
                        evidence_class: None,
                        manifest_hash: None,
                        completion: None,
                    })
                    .unwrap();
                drop(ledger);

                let reused = durable_row(Uuid::new_v4(), "dedup:crash", 1);
                let reuse_event = DurableEnqueueEvent::new_reuse_with_depth(
                    reused.clone(),
                    0,
                    pass.seq,
                    artifact_hash.clone(),
                )
                .unwrap();
                write_enqueue_event_atomic(&paths.events_dir(), &reuse_event).unwrap();

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                assert!(daemon.initial_jobs.is_empty());
                let waited = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": reused.uuid})))
                    .await
                    .unwrap();
                assert_eq!(waited["verdict"], "reused");
                let witness_after_repair = fs::read(paths.witness_path()).unwrap();
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 2);
                assert_eq!(records[1].verdict, Verdict::Reused);
                assert_eq!(records[1].labor_class, LaborClass::Reused);
                drop(daemon);

                let reopened = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                assert!(reopened.initial_jobs.is_empty());
                assert_eq!(
                    fs::read(paths.witness_path()).unwrap(),
                    witness_after_repair
                );
            })
            .await;
    }

    #[test]
    fn reuse_reconciliation_rejects_a_missing_prior_pass() {
        let temp = tempdir().unwrap();
        let events = temp.path().join("state/events");
        let witness = temp.path().join("data/witness.jsonl");
        fs::create_dir_all(events.parent().unwrap()).unwrap();
        let row = durable_row(Uuid::new_v4(), "dedup:corrupt", 1);
        let event = DurableEnqueueEvent::new_reuse_with_depth(
            row,
            0,
            99,
            format!("sha256:{}", "b".repeat(64)),
        )
        .unwrap();
        write_enqueue_event_atomic(&events, &event).unwrap();
        let durable = collect_durable_recovery_facts(&events, &witness).unwrap();
        let mut ledger = WitnessLedger::open(&witness).unwrap();
        assert!(reconcile_reuse_witnesses(&durable, &mut ledger)
            .unwrap_err()
            .to_string()
            .contains("references missing witness 99"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn singleton_query_and_dedup_are_live_through_the_daemon() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let artifact = temp.path().join("already-built.txt");
                fs::write(&artifact, b"stable artifact\n").unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                let epoch = daemon.handler.context.read().await.epoch;
                let duplicate = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await;
                assert!(matches!(
                    duplicate,
                    Err(DaemonError::Invalid(message)) if message.contains("already owns")
                ));
                assert_eq!(
                    fs::read_to_string(paths.state_dir.join(crate::lease::LEASE_EPOCH_FILE))
                        .unwrap()
                        .trim(),
                    epoch.to_string()
                );

                let context = daemon.handler.context.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let payload = json!({
                    "argv": ["true"],
                    "pool": "slot",
                    "priority": "high",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "stable-artifact",
                    "evidence": [
                        format!("artifact:{}", artifact.display()),
                        "exit:0"
                    ]
                });
                let first = client
                    .call("queue.enqueue", Some(payload.clone()))
                    .await
                    .unwrap();
                let task_uuid = first["task_uuid"].as_str().unwrap();
                let terminal = client
                    .call("queue.await_job", Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(terminal["verdict"], "pass");
                let (_, witness_before) = read_verified_records(&paths.witness_path()).unwrap();

                let status = client
                    .call("query.status", Some(json!({"pool": "slot"})))
                    .await
                    .unwrap();
                assert_eq!(status["protocolVersion"], 3);
                assert_eq!(status["pools"][0]["pool"], "slot");
                assert!(status["jobs"].as_array().unwrap().iter().any(|job| {
                    job["taskUuid"].as_str() == Some(task_uuid) && job["verdict"] == "pass"
                }));
                let pools = client.call("query.pools", Some(json!({}))).await.unwrap();
                assert_eq!(pools["pools"][0]["pool"], "slot");
                let render_text = client
                    .call("query.render", Some(json!({"format": "text"})))
                    .await
                    .unwrap();
                assert!(render_text
                    .as_str()
                    .is_some_and(|text| text.contains("\"protocolVersion\": 3")));
                let render_json = client
                    .call("query.render", Some(json!({"format": "json"})))
                    .await
                    .unwrap();
                assert_eq!(render_json["protocolVersion"], 3);

                let reused = client.call("queue.enqueue", Some(payload)).await.unwrap();
                assert_eq!(reused["state"], "reused");
                assert_eq!(reused["verdict"], "reused");
                let reused_uuid = reused["task_uuid"].as_str().unwrap().to_owned();
                assert_eq!(reused["job_id"], reused_uuid);
                let reused_wait = client
                    .call("queue.await_job", Some(json!({"task_uuid": reused_uuid})))
                    .await
                    .unwrap();
                assert_eq!(reused_wait["verdict"], "reused");
                let reused_barrier = client
                    .call(
                        "queue.await_barrier",
                        Some(json!({"barrier": reused["barrier"]})),
                    )
                    .await
                    .unwrap();
                assert_eq!(reused_barrier["complete"], true);
                let (report, witness_after) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                assert_eq!(witness_after.len(), witness_before.len() + 1);
                assert_eq!(witness_after.last().unwrap().verdict, Verdict::Reused);
                assert_eq!(
                    witness_after.last().unwrap().labor_class,
                    LaborClass::Reused
                );
                assert_eq!(context.read().await.jobs.len(), 2);
                let standup = client.call("query.standup", Some(json!({}))).await.unwrap();
                assert_eq!(standup["reused"], 1);
                let missing = client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": Uuid::new_v4().to_string()})),
                    )
                    .await
                    .unwrap_err();
                assert!(matches!(
                    missing,
                    crate::wire::WireIoError::Rpc(WireErrorCode::NotFound, _, _)
                ));

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                drop(client);
                drop(context);
                let reopened =
                    Daemon::open_with_executor(one_pool_config(), paths, settings(), executor)
                        .await
                        .unwrap();
                assert_eq!(reopened.handler.context.read().await.epoch, epoch + 1);
                assert!(reopened.initial_jobs.is_empty());
                let (reopened_shutdown, reopened_shutdown_rx) = watch::channel(false);
                let reopened_socket = reopened.handler.context.read().await.paths.socket.clone();
                let reopened_task =
                    tokio::task::spawn_local(reopened.run_until(reopened_shutdown_rx));
                let reopened_client = RpcClient::connect(&reopened_socket).await.unwrap();
                let late_reused = reopened_client
                    .call("queue.await_job", Some(json!({"task_uuid": reused_uuid})))
                    .await
                    .unwrap();
                assert_eq!(late_reused["verdict"], "reused");
                reopened_shutdown.send(true).unwrap();
                reopened_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn multi_pool_admission_is_atomic_and_any_conflict_or_gate_blocks() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    two_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();

                let held = daemon
                    .handler
                    .acquire(Some(json!({"pool": "slot"})))
                    .await
                    .unwrap();
                let held_lease = held["outcome"]["granted"]["leaseId"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": ["zeta", "slot"],
                        "priority": "low",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                assert_eq!(admitted["state"], "queued");
                let task_uuid = Uuid::parse_str(admitted["task_uuid"].as_str().unwrap()).unwrap();

                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].row.pools, ["slot", "zeta"]);
                    assert_eq!(context.lease.engine().held_in_pool("slot").unwrap(), 1);
                    assert_eq!(context.lease.engine().held_in_pool("zeta").unwrap(), 0);
                    assert_eq!(context.lease.engine().queued_in_pool("slot").unwrap(), 1);
                    assert_eq!(context.lease.engine().queued_in_pool("zeta").unwrap(), 1);
                }
                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].row.pools, ["slot", "zeta"]);
                let status = daemon
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                let projected = status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == task_uuid.to_string())
                    .unwrap();
                assert_eq!(projected["pool"], json!(["slot", "zeta"]));

                let paused = daemon
                    .handler
                    .pause(Some(json!({"pool": "zeta"})))
                    .await
                    .unwrap();
                assert_eq!(paused["affected"], 1);
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Paused);
                    assert_eq!(context.lease.engine().queue_len(), 0);
                }

                daemon
                    .handler
                    .resume(Some(json!({"pool": "zeta"})))
                    .await
                    .unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Queued);
                    assert_eq!(context.lease.engine().queued_in_pool("slot").unwrap(), 1);
                    assert_eq!(context.lease.engine().queued_in_pool("zeta").unwrap(), 1);
                }

                assert_eq!(daemon.handler.apply_pool_loss("zeta").await.unwrap(), 0);
                assert_eq!(daemon.handler.apply_pool_loss("slot").await.unwrap(), 0);
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Paused);
                    assert!(context.unreachable_paused_jobs.contains(&task_uuid));
                    assert_eq!(context.lease.engine().queue_len(), 0);
                }
                daemon.handler.apply_pool_return("zeta").await.unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Paused);
                    assert!(context.unreachable_paused_jobs.contains(&task_uuid));
                }
                daemon.handler.apply_pool_return("slot").await.unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(context.jobs[&task_uuid].state, JobState::Queued);
                    assert!(!context.unreachable_paused_jobs.contains(&task_uuid));
                    assert_eq!(context.lease.engine().queued_in_pool("slot").unwrap(), 1);
                    assert_eq!(context.lease.engine().queued_in_pool("zeta").unwrap(), 1);
                }
                let released = daemon
                    .handler
                    .release(Some(json!({"lease": held_lease})))
                    .await
                    .unwrap();
                assert_eq!(released["promoted"].as_array().unwrap().len(), 1);
                assert_eq!(released["promoted"][0]["pools"], json!(["slot", "zeta"]));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pause_withdraws_pending_lease_and_queued_cancel_is_durable() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let held = daemon
                    .handler
                    .acquire(Some(json!({"pool": "slot"})))
                    .await
                    .unwrap();
                let held_lease = held["outcome"]["granted"]["leaseId"]
                    .as_str()
                    .expect("granted lease id")
                    .to_owned();
                let admitted = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "priority": "low",
                        "adapter": "shell",
                        "source": "manual",
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                assert_eq!(admitted["state"], "queued");
                let task_uuid = admitted["task_uuid"].as_str().unwrap();
                let queued_status = daemon
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                assert!(queued_status["jobs"].as_array().unwrap().iter().any(|job| {
                    job["taskUuid"].as_str() == Some(task_uuid) && job["state"] == "queued"
                }));
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .lease
                        .engine()
                        .queue_len(),
                    1
                );
                let paused = daemon
                    .handler
                    .pause(Some(json!({"pool": "slot"})))
                    .await
                    .unwrap();
                assert_eq!(paused["affected"], 1);
                let paused_status = daemon
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                assert!(paused_status["jobs"].as_array().unwrap().iter().any(|job| {
                    job["taskUuid"].as_str() == Some(task_uuid) && job["state"] == "paused"
                }));
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .lease
                        .engine()
                        .queue_len(),
                    0
                );

                let cancelled = daemon
                    .handler
                    .cancel(Some(json!({"task_uuid": task_uuid, "force": false})))
                    .await
                    .unwrap();
                assert_eq!(cancelled["affected"], 1);
                assert_eq!(cancelled["was"], "paused");
                let waited = daemon
                    .handler
                    .await_job(Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(waited["verdict"], "cancelled");
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.last().unwrap().verdict, Verdict::Cancelled);

                let released = daemon
                    .handler
                    .release(Some(json!({"lease": held_lease})))
                    .await
                    .unwrap();
                assert!(released["promoted"].as_array().unwrap().is_empty());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn forced_cancel_response_implies_durable_cancelled_witness() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = DaemonPaths {
                    socket: temp.path().join("run/tally.sock"),
                    state_dir: temp.path().join("state"),
                    data_dir: temp.path().join("data"),
                };
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                // A concurrently forked child temporarily inherits this same open-file
                // description until exec applies CLOEXEC. Keep the equivalent duplicate
                // alive across shutdown so reopen cannot depend on last-close timing.
                let inherited_lock = daemon._state_lock.try_clone().unwrap();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let admitted = client
                    .call(
                        "queue.enqueue",
                        Some(json!({
                            "argv": ["sleep", "30"],
                            "pool": "slot",
                            "priority": "low",
                            "adapter": "shell",
                            "source": "manual",
                            "evidence": ["exit:0"]
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(admitted["state"], "running");
                let task_uuid = admitted["task_uuid"].as_str().unwrap().to_owned();
                let cancelled = client
                    .call(
                        "queue.cancel",
                        Some(json!({"task_uuid": task_uuid, "force": true})),
                    )
                    .await
                    .unwrap();
                assert_eq!(cancelled["affected"], 1);
                let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert!(report.ok);
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].verdict, Verdict::Cancelled);
                assert_eq!(records[0].task_uuid.as_deref(), Some(task_uuid.as_str()));

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
                drop(client);

                let reopened = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                drop(inherited_lock);
                assert!(reopened.initial_jobs.is_empty());
                let (second_shutdown, second_shutdown_rx) = watch::channel(false);
                let second_task = tokio::task::spawn_local(reopened.run_until(second_shutdown_rx));
                let restarted = RpcClient::connect(&paths.socket).await.unwrap();
                let late = restarted
                    .call("queue.await_job", Some(json!({"task_uuid": task_uuid})))
                    .await
                    .unwrap();
                assert_eq!(late["verdict"], "cancelled");
                second_shutdown.send(true).unwrap();
                second_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn panicking_producer_restarts_without_stopping_its_peer() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let (events_tx, mut events_rx) = mpsc::unbounded_channel();
                let panics = Rc::new(Cell::new(0_u32));
                let panics_for_factory = panics.clone();
                let panic_factory: SupervisedFactory = Rc::new(move || {
                    let panics = panics_for_factory.clone();
                    Box::pin(async move {
                        panics.set(panics.get() + 1);
                        panic!("producer fault injection");
                    })
                });
                let peer_runs = Rc::new(Cell::new(0_u32));
                let peer_for_factory = peer_runs.clone();
                let peer_factory: SupervisedFactory = Rc::new(move || {
                    let peer_runs = peer_for_factory.clone();
                    Box::pin(async move {
                        peer_runs.set(peer_runs.get() + 1);
                        std::future::pending::<()>().await;
                        Ok(())
                    })
                });
                let first = spawn_supervised(
                    SupervisedTask {
                        name: "faulty".to_owned(),
                        restart_delay: Duration::from_millis(1),
                        factory: panic_factory,
                    },
                    shutdown_rx.clone(),
                    events_tx.clone(),
                );
                let second = spawn_supervised(
                    SupervisedTask {
                        name: "peer".to_owned(),
                        restart_delay: Duration::from_millis(1),
                        factory: peer_factory,
                    },
                    shutdown_rx,
                    events_tx,
                );
                tokio::time::timeout(Duration::from_secs(1), async {
                    loop {
                        let _ = events_rx.recv().await;
                        if panics.get() >= 2 && peer_runs.get() == 1 {
                            break;
                        }
                    }
                })
                .await
                .unwrap();
                shutdown_tx.send(true).unwrap();
                first.await.unwrap();
                second.await.unwrap();
                assert!(panics.get() >= 2);
                assert_eq!(peer_runs.get(), 1);
            })
            .await;
    }

    #[test]
    fn sd_notify_ready_watchdog_and_stopping_are_datagrams() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("notify.sock");
        let socket = UnixDatagram::bind(&path).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let notifier = SystemdNotifier::with_socket(path, Some(Duration::from_secs(2)));
        for (send, expected) in [
            (
                SystemdNotifier::ready as fn(&SystemdNotifier) -> _,
                "READY=1\nSTATUS=tally daemon ready",
            ),
            (SystemdNotifier::watchdog, "WATCHDOG=1"),
            (SystemdNotifier::stopping, "STOPPING=1"),
        ] {
            send(&notifier).unwrap();
            let mut buffer = [0_u8; 128];
            let read = socket.recv(&mut buffer).unwrap();
            assert_eq!(&buffer[..read], expected.as_bytes());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_1_restart_reconstructs_lineage_two_attempts_log_and_proof() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let (paths, parent_uuid, child_uuid, parent_pass, _) =
                    seed_durable_query_fixture(temp.path());
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);

                // Open and drop one daemon before inspecting through the next
                // generation. This exercises lifecycle reload independently of
                // both the TaskChampion cache and the witness ledger.
                let first = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor.clone(),
                )
                .await
                .unwrap();
                drop(first);
                let restarted = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();

                let jobs = restarted
                    .handler
                    .query("query.jobs", Some(json!({})))
                    .await
                    .unwrap();
                assert_eq!(jobs["protocolVersion"], 3);
                assert_eq!(jobs["nextCursor"], Value::Null);
                assert_eq!(jobs["snapshot"]["history"]["complete"], true);
                let parent = jobs["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == parent_uuid.to_string())
                    .unwrap();
                assert_eq!(parent["terminalVerdict"], "pass");
                assert_eq!(parent["terminalAttempt"], 2);
                assert_eq!(parent["currentAttempt"], 2);
                assert_eq!(parent["leaseEpoch"], 2);
                assert_eq!(parent["evidenceResult"], "pass");
                assert_eq!(parent["lifecycleEvent"], "completed");
                assert_eq!(parent["childTaskUuids"], json!([child_uuid.to_string()]));
                assert_ne!(parent["liveState"], parent["terminalVerdict"]);
                assert_ne!(parent["rowStatus"], parent["evidenceResult"]);

                let job = restarted
                    .handler
                    .query("query.job", Some(json!({"id": parent_uuid.to_string()})))
                    .await
                    .unwrap();
                let attempts = job["attempts"].as_array().unwrap();
                assert_eq!(attempts.len(), 2);
                assert_eq!(attempts[0]["attempt"], 1);
                assert_eq!(attempts[0]["leaseEpoch"], 1);
                assert_eq!(attempts[0]["witnessRecords"][0]["verdict"], "preempted");
                assert_eq!(attempts[1]["attempt"], 2);
                assert_eq!(attempts[1]["leaseEpoch"], 2);
                assert_eq!(attempts[1]["evidenceResult"], "pass");

                let log = restarted
                    .handler
                    .query("query.log", Some(json!({"task": parent_uuid.to_string()})))
                    .await
                    .unwrap();
                assert!(log["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["attempt"] == 1 && event["event"] == "preempted"));
                assert!(log["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["attempt"] == 2 && event["event"] == "completed"));

                let proof = restarted
                    .handler
                    .query(
                        "query.proof",
                        Some(json!({"task": parent_uuid.to_string(), "attempt": 2})),
                    )
                    .await
                    .unwrap();
                assert_eq!(proof["status"], "verified");
                assert_eq!(
                    proof["witnessRecord"],
                    serde_json::to_value(parent_pass).unwrap()
                );
                assert_eq!(proof["evidence"]["observations"][0]["passed"], true);
                assert_eq!(proof["advisoryAttestations"].as_array().unwrap().len(), 1);

                let status = restarted
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                assert!(status["jobs"].as_array().unwrap().iter().any(|job| {
                    job["taskUuid"] == parent_uuid.to_string() && job["verdict"] == "pass"
                }));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acceptance_24_6_proof_matches_verified_record_and_reports_chain_head() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let (paths, parent_uuid, _, expected, expected_head) =
                    seed_durable_query_fixture(temp.path());
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon = Daemon::open_with_executor(
                    one_pool_config(),
                    paths.clone(),
                    settings(),
                    executor,
                )
                .await
                .unwrap();
                let proof = daemon
                    .handler
                    .query(
                        "query.proof",
                        Some(json!({"task": parent_uuid.to_string(), "attempt": 2})),
                    )
                    .await
                    .unwrap();
                let (_, disk_records) = read_verified_records(&paths.witness_path()).unwrap();
                let disk = disk_records
                    .iter()
                    .find(|record| {
                        record.task_uuid.as_deref() == Some(parent_uuid.to_string().as_str())
                            && record.attempt == 2
                    })
                    .unwrap();
                assert_eq!(disk, &expected);
                assert_eq!(
                    proof["witnessRecord"],
                    serde_json::to_value(disk).unwrap(),
                    "proof must preserve every verified WitnessRecord field"
                );
                assert_eq!(proof["ledger"]["verified"], true);
                assert_eq!(
                    proof["ledger"]["chainHead"],
                    json!({"seq": expected_head.seq, "hash": expected_head.hash})
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs1_concurrent_full_duplicate_creates_once_then_attaches_both_waiters() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                daemon
                    .handler
                    .pause(Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let context = daemon.handler.context.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let first_client = RpcClient::connect(&paths.socket).await.unwrap();
                let second_client = RpcClient::connect(&paths.socket).await.unwrap();
                let payload = fs1_full_payload("fs1-concurrent", &["true"], ["exit:0".to_owned()]);
                let mut metadata_variant = payload.clone();
                metadata_variant["priority"] = json!("low");
                metadata_variant["consumptionEstimate"] = json!(99);
                metadata_variant["parent"] = json!("00000000-0000-4000-8000-000000000044");
                metadata_variant["wait"] = json!(true);
                let (first, second) = tokio::join!(
                    first_client.call("queue.enqueue", Some(payload.clone())),
                    second_client.call("queue.enqueue", Some(metadata_variant))
                );
                let first = first.unwrap();
                let second = second.unwrap();
                let dispositions = [
                    first["disposition"].as_str(),
                    second["disposition"].as_str(),
                ];
                assert!(dispositions.contains(&Some("created")));
                assert!(dispositions.contains(&Some("attached")));
                assert_eq!(first["task_uuid"], second["task_uuid"]);
                assert_eq!(first["payloadHash"], second["payloadHash"]);
                assert_eq!(first["schemaVersion"], 1);
                assert_eq!(second["schemaVersion"], 1);
                assert_eq!(first["attempt"], 1);
                assert_eq!(second["attempt"], 1);
                assert_eq!(context.read().await.jobs.len(), 1);
                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(
                    events[0].row.payload_hash.as_deref(),
                    first["payloadHash"].as_str()
                );

                first_client
                    .call("queue.resume", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let wait_params = json!({"task_uuid": first["task_uuid"]});
                let (first_wait, second_wait) = tokio::join!(
                    first_client.call("queue.await_job", Some(wait_params.clone())),
                    second_client.call("queue.await_job", Some(wait_params))
                );
                let first_wait = first_wait.unwrap();
                let second_wait = second_wait.unwrap();
                assert_eq!(first_wait["verdict"], "pass");
                assert_eq!(first_wait, second_wait);
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 1);
                assert_eq!(
                    records[0].payload_hash.as_deref(),
                    first["payloadHash"].as_str()
                );

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs1_full_conflicts_fail_closed_for_running_queued_and_duplicate_live_rows() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let running = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-running-conflict",
                            &["sleep", "0.2"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap();
                assert_eq!(running["state"], "running");
                let running_error = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-running-conflict",
                            &["true"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap_err();
                let running_data = fs1_conflict(running_error);
                assert_eq!(running_data["existingTaskUuid"], running["task_uuid"]);
                assert_ne!(
                    running_data["payloadHash"],
                    running_data["existingPayloadHash"]
                );

                let queued = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-queued-conflict",
                            &["true"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap();
                assert_eq!(queued["state"], "queued");
                let queued_error = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-queued-conflict",
                            &["false"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap_err();
                let queued_data = fs1_conflict(queued_error);
                assert_eq!(queued_data["existingTaskUuid"], queued["task_uuid"]);

                client
                    .call("queue.pause", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let legacy = json!({
                    "argv": ["true"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "fs1-legacy-live-residue",
                    "evidence": ["exit:0"],
                });
                let legacy_one = client
                    .call("queue.enqueue", Some(legacy.clone()))
                    .await
                    .unwrap();
                let legacy_two = client
                    .call("queue.enqueue", Some(legacy.clone()))
                    .await
                    .unwrap();
                assert_eq!(legacy_one["disposition"], "created");
                assert_eq!(legacy_two["disposition"], "created");
                assert_ne!(legacy_one["task_uuid"], legacy_two["task_uuid"]);
                let residue_error = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-legacy-live-residue",
                            &["true"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap_err();
                let residue_data = fs1_conflict(residue_error);
                assert_eq!(residue_data["existing"].as_array().unwrap().len(), 2);
                assert_eq!(
                    read_acknowledged_events(&paths.events_dir()).unwrap().len(),
                    4
                );

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs1_full_terminal_pass_with_no_artifacts_reuses_purely_even_with_manifest() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let context = daemon.handler.context.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let mut payload =
                    fs1_full_payload("fs1-vacuous-reuse", &["true"], ["exit:0".to_owned()]);
                let manifest = temp.path().join("gates.json");
                fs::write(
                    &manifest,
                    r#"{"schemaVersion":1,"artifact":null,"gates":[{"id":"tests","status":"pass"}]}"#,
                )
                .unwrap();
                payload["gateManifest"] = json!({
                    "path": manifest,
                    "requiredGateIds": ["tests"],
                    "acceptancePolicy": "manual",
                });
                let created = client
                    .call("queue.enqueue", Some(payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(created["disposition"], "created");
                let terminal = fs1_wait(&client, &created).await;
                assert_eq!(terminal["verdict"], "pass");
                let (_, before) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(before.len(), 1);
                assert!(before[0].artifact_content_hash.is_none());

                let reused = client.call("queue.enqueue", Some(payload)).await.unwrap();
                assert_eq!(reused["disposition"], "reused");
                assert_eq!(reused["task_uuid"], created["task_uuid"]);
                assert_eq!(reused["verdict"], "pass");
                assert_eq!(reused["witnessSeq"], terminal["witness_seq"]);
                assert_eq!(reused["payloadHash"], created["payloadHash"]);
                assert_eq!(context.read().await.jobs.len(), 1);
                assert_eq!(
                    read_acknowledged_events(&paths.events_dir()).unwrap().len(),
                    1
                );
                let (_, after) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(after, before);

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs1_full_pass_rehashes_clean_and_discloses_every_rerun_reason() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let clean_path = temp.path().join("clean.txt");
                let drift_path = temp.path().join("drift.txt");
                let declared_path = temp.path().join("declared.txt");
                let unavailable_path = temp.path().join("unavailable.txt");
                for path in [&clean_path, &drift_path, &declared_path, &unavailable_path] {
                    fs::write(path, b"original\n").unwrap();
                }
                let declared_hash = hash_artifact_file(&declared_path).unwrap();

                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let clean_payload = fs1_full_payload(
                    "fs1-clean-artifact",
                    &["true"],
                    [
                        format!("artifact:{}", clean_path.display()),
                        "exit:0".to_owned(),
                    ],
                );
                let clean = client
                    .call("queue.enqueue", Some(clean_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &clean).await["verdict"], "pass");
                let clean_reused = client
                    .call("queue.enqueue", Some(clean_payload))
                    .await
                    .unwrap();
                assert_eq!(clean_reused["disposition"], "reused");

                let drift_payload = fs1_full_payload(
                    "fs1-artifact-drift",
                    &["true"],
                    [
                        format!("artifact:{}", drift_path.display()),
                        "exit:0".to_owned(),
                    ],
                );
                let drift = client
                    .call("queue.enqueue", Some(drift_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &drift).await["verdict"], "pass");
                fs::write(&drift_path, b"changed\n").unwrap();
                let drift_rerun = client
                    .call("queue.enqueue", Some(drift_payload))
                    .await
                    .unwrap();
                assert_eq!(drift_rerun["disposition"], "created");
                assert_eq!(drift_rerun["reusedRejected"], "artifact-drift");
                assert_eq!(fs1_wait(&client, &drift_rerun).await["verdict"], "pass");

                let declared_payload = fs1_full_payload(
                    "fs1-declared-mismatch",
                    &["true"],
                    [
                        format!("artifact:{}", declared_path.display()),
                        format!(
                            "hash:sha256:{}",
                            declared_hash.trim_start_matches("sha256:")
                        ),
                        "exit:0".to_owned(),
                    ],
                );
                let declared = client
                    .call("queue.enqueue", Some(declared_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &declared).await["verdict"], "pass");
                fs::write(&declared_path, b"changed\n").unwrap();
                let declared_rerun = client
                    .call("queue.enqueue", Some(declared_payload))
                    .await
                    .unwrap();
                assert_eq!(declared_rerun["disposition"], "created");
                assert_eq!(declared_rerun["reusedRejected"], "declared-hash-mismatch");
                assert_eq!(
                    fs1_wait(&client, &declared_rerun).await["verdict"],
                    "clean-exit-no-artifact"
                );

                let unavailable_payload = fs1_full_payload(
                    "fs1-artifact-unavailable",
                    &["true"],
                    [
                        format!("artifact:{}", unavailable_path.display()),
                        "exit:0".to_owned(),
                    ],
                );
                let unavailable = client
                    .call("queue.enqueue", Some(unavailable_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &unavailable).await["verdict"], "pass");
                fs::remove_file(&unavailable_path).unwrap();
                let unavailable_rerun = client
                    .call("queue.enqueue", Some(unavailable_payload))
                    .await
                    .unwrap();
                assert_eq!(unavailable_rerun["disposition"], "created");
                assert_eq!(unavailable_rerun["reusedRejected"], "artifact-unavailable");
                assert_eq!(
                    unavailable_rerun["errorDetail"],
                    unavailable_path.to_string_lossy().as_ref()
                );
                assert_eq!(
                    fs1_wait(&client, &unavailable_rerun).await["verdict"],
                    "clean-exit-no-artifact"
                );

                assert_eq!(
                    read_acknowledged_events(&paths.events_dir()).unwrap().len(),
                    7
                );
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records.len(), 7);

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs1_terminal_failure_is_memoized_retry_is_durable_and_pass_retry_is_invalid() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let attached_client = RpcClient::connect(&paths.socket).await.unwrap();
                let failed_payload =
                    fs1_full_payload("fs1-memoized-failure", &["false"], ["exit:0".to_owned()]);
                let created = client
                    .call("queue.enqueue", Some(failed_payload.clone()))
                    .await
                    .unwrap();
                let failed = fs1_wait(&client, &created).await;
                assert_eq!(failed["verdict"], "failed");
                let terminal = client
                    .call("queue.enqueue", Some(failed_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(terminal["disposition"], "terminal");
                assert_eq!(terminal["task_uuid"], created["task_uuid"]);
                assert_eq!(terminal["verdict"], "failed");
                assert_eq!(terminal["witnessSeq"], failed["witness_seq"]);
                assert_eq!(
                    read_acknowledged_events(&paths.events_dir()).unwrap().len(),
                    1
                );

                let terminal_conflict = client
                    .call(
                        "queue.enqueue",
                        Some(fs1_full_payload(
                            "fs1-memoized-failure",
                            &["true"],
                            ["exit:0".to_owned()],
                        )),
                    )
                    .await
                    .unwrap_err();
                let conflict_data = fs1_conflict(terminal_conflict);
                assert_eq!(conflict_data["existingTaskUuid"], created["task_uuid"]);

                client
                    .call("queue.pause", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let retry = client
                    .call(
                        "queue.retry",
                        Some(json!({"task_uuid": created["task_uuid"]})),
                    )
                    .await
                    .unwrap();
                assert_eq!(retry["schemaVersion"], 1);
                assert_eq!(retry["retried"], true);
                assert_eq!(retry["task_uuid"], created["task_uuid"]);
                assert_eq!(retry["attempt"], 2);
                assert_eq!(retry["payloadHash"], created["payloadHash"]);
                assert!(retry.get("disposition").is_none());
                let (_, before_retry_terminal) =
                    read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(before_retry_terminal.len(), 1);
                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].retries.len(), 1);
                assert_eq!(events[0].retries[0].attempt, 2);
                assert_eq!(
                    events[0].retries[0].previous_witness_seq,
                    failed["witness_seq"].as_u64().unwrap()
                );
                let original_attempt = client
                    .call(
                        "queue.await_job",
                        Some(json!({
                            "task_uuid": created["task_uuid"],
                            "attempt": 1
                        })),
                    )
                    .await
                    .unwrap();
                assert_eq!(original_attempt["attempt"], 1);
                assert_eq!(original_attempt["witness_seq"], failed["witness_seq"]);

                let attached = attached_client
                    .call("queue.enqueue", Some(failed_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(attached["disposition"], "attached");
                assert_eq!(attached["task_uuid"], created["task_uuid"]);
                assert_eq!(attached["attempt"], 2);
                client
                    .call("queue.resume", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let wait_params = json!({"task_uuid": created["task_uuid"]});
                let (retried_wait, attached_wait) = tokio::join!(
                    client.call("queue.await_job", Some(wait_params.clone())),
                    attached_client.call("queue.await_job", Some(wait_params))
                );
                let retried_wait = retried_wait.unwrap();
                assert_eq!(retried_wait, attached_wait.unwrap());
                assert_eq!(retried_wait["attempt"], 2);
                assert_eq!(retried_wait["verdict"], "failed");
                let latest_terminal = client
                    .call("queue.enqueue", Some(failed_payload))
                    .await
                    .unwrap();
                assert_eq!(latest_terminal["disposition"], "terminal");
                assert_eq!(latest_terminal["witnessSeq"], retried_wait["witness_seq"]);

                let passing_payload =
                    fs1_full_payload("fs1-pass-no-retry", &["true"], ["exit:0".to_owned()]);
                let passing = client
                    .call("queue.enqueue", Some(passing_payload))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &passing).await["verdict"], "pass");
                let pass_retry = client
                    .call(
                        "queue.retry",
                        Some(json!({"task_uuid": passing["task_uuid"]})),
                    )
                    .await
                    .unwrap_err();
                assert!(matches!(
                    pass_retry,
                    WireIoError::Rpc(WireErrorCode::InvalidParams, _, _)
                ));
                let missing_retry = client
                    .call(
                        "queue.retry",
                        Some(json!({"task_uuid": Uuid::new_v4().to_string()})),
                    )
                    .await
                    .unwrap_err();
                assert!(matches!(
                    missing_retry,
                    WireIoError::Rpc(WireErrorCode::InvalidParams, _, _)
                ));

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs1_explicit_retry_survives_restart_on_the_same_row_and_next_attempt() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();
                let payload =
                    fs1_full_payload("fs1-retry-restart", &["false"], ["exit:0".to_owned()]);
                let created = client
                    .call("queue.enqueue", Some(payload.clone()))
                    .await
                    .unwrap();
                let task_uuid = created["task_uuid"].as_str().unwrap().to_owned();
                assert_eq!(fs1_wait(&client, &created).await["attempt"], 1);
                client
                    .call("queue.pause", Some(json!({"pool": "slot", "all": false})))
                    .await
                    .unwrap();
                let retry = client
                    .call("queue.retry", Some(json!({"task_uuid": task_uuid.clone()})))
                    .await
                    .unwrap();
                assert_eq!(retry["attempt"], 2);
                assert_eq!(retry["state"], "paused");
                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();

                let restarted = fs1_daemon(&paths).await;
                let (restart_shutdown_tx, restart_shutdown_rx) = watch::channel(false);
                let restarted_task =
                    tokio::task::spawn_local(restarted.run_until(restart_shutdown_rx));
                let restarted_client = RpcClient::connect(&paths.socket).await.unwrap();
                let terminal = restarted_client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": task_uuid.clone()})),
                    )
                    .await
                    .unwrap();
                assert_eq!(terminal["task_uuid"], task_uuid);
                assert_eq!(terminal["attempt"], 2);
                assert_eq!(terminal["verdict"], "failed");
                let latest = restarted_client
                    .call("queue.enqueue", Some(payload))
                    .await
                    .unwrap();
                assert_eq!(latest["disposition"], "terminal");
                assert_eq!(latest["attempt"], 2);
                assert_eq!(latest["witnessSeq"], terminal["witness_seq"]);

                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].retries.len(), 1);
                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(
                    records
                        .iter()
                        .map(|record| record.attempt)
                        .collect::<Vec<_>>(),
                    [1, 2]
                );
                assert_eq!(records[0].task_uuid, records[1].task_uuid);
                assert_eq!(records[0].payload_hash, records[1].payload_hash);

                restart_shutdown_tx.send(true).unwrap();
                restarted_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs1_legacy_behavior_stays_pass_only_row_materializing_and_manifest_excluding() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let artifact = temp.path().join("legacy-artifact.txt");
                fs::write(&artifact, b"legacy\n").unwrap();
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let legacy = json!({
                    "argv": ["true"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "fs1-legacy-reuse",
                    "evidence": [
                        format!("artifact:{}", artifact.display()),
                        "exit:0"
                    ],
                });
                let first = client
                    .call("queue.enqueue", Some(legacy.clone()))
                    .await
                    .unwrap();
                assert_eq!(first["schemaVersion"], 1);
                assert_eq!(first["disposition"], "created");
                assert!(first.get("payloadHash").is_none());
                assert_eq!(fs1_wait(&client, &first).await["verdict"], "pass");
                let reused = client
                    .call("queue.enqueue", Some(legacy.clone()))
                    .await
                    .unwrap();
                assert_eq!(reused["disposition"], "reused");
                assert_eq!(reused["verdict"], "reused");
                assert_ne!(reused["task_uuid"], first["task_uuid"]);

                let manifest = temp.path().join("legacy-gates.json");
                fs::write(
                    &manifest,
                    r#"{"schemaVersion":1,"artifact":null,"gates":[{"id":"tests","status":"pass"}]}"#,
                )
                .unwrap();
                let manifest_payload = json!({
                    "argv": ["true"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "fs1-legacy-manifest",
                    "evidence": [
                        format!("artifact:{}", artifact.display()),
                        "exit:0"
                    ],
                    "gateManifest": {
                        "path": manifest,
                        "requiredGateIds": ["tests"],
                        "acceptancePolicy": "manual"
                    }
                });
                let manifest_first = client
                    .call("queue.enqueue", Some(manifest_payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(
                    fs1_wait(&client, &manifest_first).await["verdict"],
                    "pass"
                );
                let manifest_second = client
                    .call("queue.enqueue", Some(manifest_payload))
                    .await
                    .unwrap();
                assert_eq!(manifest_second["disposition"], "created");
                assert_ne!(manifest_second["task_uuid"], manifest_first["task_uuid"]);
                assert_eq!(
                    fs1_wait(&client, &manifest_second).await["verdict"],
                    "pass"
                );

                let failed_legacy = json!({
                    "argv": ["false"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "fs1-legacy-failure",
                    "evidence": ["exit:0"],
                });
                let failed_first = client
                    .call("queue.enqueue", Some(failed_legacy.clone()))
                    .await
                    .unwrap();
                assert_eq!(
                    fs1_wait(&client, &failed_first).await["verdict"],
                    "failed"
                );
                let failed_second = client
                    .call("queue.enqueue", Some(failed_legacy))
                    .await
                    .unwrap();
                assert_eq!(failed_second["disposition"], "created");
                assert_ne!(failed_second["task_uuid"], failed_first["task_uuid"]);
                assert_eq!(
                    fs1_wait(&client, &failed_second).await["verdict"],
                    "failed"
                );

                let mut unrecorded_legacy = json!({
                    "argv": ["true"],
                    "pool": "slot",
                    "adapter": "shell",
                    "source": "manual",
                    "dedupKey": "fs1-unrecorded-terminal",
                    "evidence": ["exit:0"],
                });
                let unrecorded = client
                    .call("queue.enqueue", Some(unrecorded_legacy.clone()))
                    .await
                    .unwrap();
                assert_eq!(fs1_wait(&client, &unrecorded).await["verdict"], "pass");
                unrecorded_legacy["submission"] = json!({"mode": "full"});
                let full_after_legacy = client
                    .call("queue.enqueue", Some(unrecorded_legacy))
                    .await
                    .unwrap();
                assert_eq!(full_after_legacy["disposition"], "created");
                assert_eq!(
                    full_after_legacy["reusedRejected"],
                    "payload-hash-unrecorded"
                );
                assert_ne!(full_after_legacy["task_uuid"], unrecorded["task_uuid"]);
                assert_eq!(
                    fs1_wait(&client, &full_after_legacy).await["verdict"],
                    "pass"
                );

                let (_, records) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(records[0].payload_hash, None);
                assert_eq!(records[1].verdict, Verdict::Reused);
                assert_eq!(records[1].payload_hash, None);

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs2_large_brief_and_provenance_round_trip_group_and_enforce_max_nodes() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let brief_source = temp.path().join("brief.json");
                let brief_document = json!({
                    "mission": "exercise the structured brief path",
                    "acceptance": ["brief is durable", "provenance is witnessed"],
                    "payload": "x".repeat(70 * 1024),
                });
                fs::write(
                    &brief_source,
                    serde_json::to_vec_pretty(&brief_document).unwrap(),
                )
                .unwrap();
                assert!(fs::metadata(&brief_source).unwrap().len() > 64 * 1024);

                let flow_run_id = Uuid::new_v4().to_string();
                let daemon = fs1_daemon(&paths).await;
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let client = RpcClient::connect(&paths.socket).await.unwrap();

                let mut payload = fs1_full_payload(
                    "flow:brief-round-trip:0",
                    &[
                        "sh",
                        "-c",
                        "test -n \"$TALLY_BRIEF\" && test -f \"$TALLY_BRIEF\"",
                    ],
                    ["exit:0".to_owned()],
                );
                payload["source"] = json!("orchestrator");
                payload["briefPath"] = json!(brief_source);
                payload["orchestration"] = json!({
                    "flowName": "brief-round-trip",
                    "flowRunId": flow_run_id,
                    "scriptHash": "sha256-script-generation",
                    "nodeOrdinal": 0,
                    "nodeLabel": "verify-brief",
                    "maxNodes": 1,
                    "selection": {
                        "selector": "pooled-fast",
                        "members": ["worker-a", "worker-b"]
                    }
                });
                let created = client
                    .call("queue.enqueue", Some(payload.clone()))
                    .await
                    .unwrap();
                assert_eq!(created["disposition"], "created");
                let task_uuid = Uuid::parse_str(created["task_uuid"].as_str().unwrap()).unwrap();
                assert_eq!(task_uuid.get_version_num(), 7);
                let brief_hash = created["payloadHash"]
                    .as_str()
                    .expect("full submission returns payloadHash")
                    .to_owned();
                let terminal = fs1_wait(&client, &created).await;
                assert_eq!(terminal["verdict"], "pass");

                let events = read_acknowledged_events(&paths.events_dir()).unwrap();
                assert_eq!(events.len(), 1);
                let durable_brief_hash = events[0].row.brief_hash.clone().unwrap();
                assert_ne!(durable_brief_hash, brief_hash);
                assert_eq!(
                    events[0].row.orchestration.as_ref().unwrap().as_value()["selection"]
                        ["members"],
                    json!(["worker-a", "worker-b"])
                );
                let stored_path =
                    brief::content_path(&paths.data_dir, &durable_brief_hash).unwrap();
                let stored = fs::read(&stored_path).unwrap();
                assert!(stored.len() > 64 * 1024);
                assert_eq!(
                    stored,
                    serde_json::to_vec(&brief_document).unwrap(),
                    "the daemon stores parsed canonical JSON, not source formatting"
                );
                assert_eq!(
                    fs::metadata(&stored_path).unwrap().permissions().mode() & 0o777,
                    0o600
                );

                let (_, witness) = read_verified_records(&paths.witness_path()).unwrap();
                assert_eq!(witness.len(), 1);
                assert_eq!(
                    witness[0].brief_hash.as_deref(),
                    Some(durable_brief_hash.as_str())
                );
                assert_eq!(
                    witness[0].orchestration.as_ref().unwrap().as_value()["nodeLabel"],
                    "verify-brief"
                );

                let grouped = client
                    .call("query.jobs", Some(json!({"flowRun": flow_run_id.clone()})))
                    .await
                    .unwrap();
                assert_eq!(grouped["items"].as_array().unwrap().len(), 1);
                assert_eq!(
                    grouped["items"][0]["briefHash"],
                    Value::String(durable_brief_hash.clone())
                );
                assert_eq!(
                    grouped["items"][0]["orchestration"]["flowRunId"],
                    flow_run_id
                );
                assert_eq!(
                    grouped["items"][0]["argv"],
                    json!([
                        "sh",
                        "-c",
                        "test -n \"$TALLY_BRIEF\" && test -f \"$TALLY_BRIEF\""
                    ])
                );
                let unrelated = client
                    .call(
                        "query.jobs",
                        Some(json!({"flowRun": Uuid::new_v4().to_string()})),
                    )
                    .await
                    .unwrap();
                assert!(unrelated["items"].as_array().unwrap().is_empty());

                let mut replay = payload.clone();
                replay["orchestration"]["maxNodes"] = json!(999);
                replay["orchestration"]["selection"]["members"] = json!(["worker-z"]);
                let reused = client.call("queue.enqueue", Some(replay)).await.unwrap();
                assert_eq!(reused["disposition"], "reused");
                assert_eq!(reused["task_uuid"], created["task_uuid"]);
                assert_eq!(reused["payloadHash"], created["payloadHash"]);

                let mut overflow = payload;
                overflow["dedupKey"] = json!("flow:brief-round-trip:1");
                overflow["orchestration"]["nodeOrdinal"] = json!(1);
                let overflow_error = client
                    .call("queue.enqueue", Some(overflow))
                    .await
                    .unwrap_err();
                match overflow_error {
                    WireIoError::Rpc(WireErrorCode::FlowNodeCap, _, Some(data)) => {
                        assert_eq!(data["flowRunId"], flow_run_id);
                        assert_eq!(data["maxNodes"], 1);
                        assert_eq!(data["existingNodes"], 1);
                    }
                    other => panic!("expected flow-node-cap, got {other:?}"),
                }

                shutdown_tx.send(true).unwrap();
                daemon_task.await.unwrap().unwrap();

                let restarted = fs1_daemon(&paths).await;
                let status = restarted
                    .handler
                    .query("query.status", Some(json!({})))
                    .await
                    .unwrap();
                let projection = status["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["taskUuid"] == created["task_uuid"])
                    .unwrap();
                assert_eq!(projection["briefHash"], durable_brief_hash);
                assert_eq!(
                    projection["orchestration"]["scriptHash"],
                    "sha256-script-generation"
                );
                drop(restarted);
                tokio::task::yield_now().await;

                fs::write(&stored_path, b"{}").unwrap();
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let error = match Daemon::open_with_executor(
                    one_pool_config(),
                    paths,
                    settings(),
                    executor,
                )
                .await
                {
                    Ok(_) => panic!("tampered durable brief unexpectedly survived restart"),
                    Err(error) => error,
                };
                assert!(error.to_string().contains("durable brief"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fs2_outstanding_converges_across_terminal_rollback_and_restart() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let temp = tempdir().unwrap();
                let paths = fs1_paths(temp.path());
                let mut config = one_pool_config();
                config.enqueue.fanout_cap = 1;
                config.pools.get_mut("slot").unwrap().credentials.insert(
                    "token".to_owned(),
                    PathBuf::from("/run/credentials/slot-token"),
                );
                config.pools.insert(
                    "flow".to_owned(),
                    PoolConfig {
                        resource: ResourceKind::BuildSlot,
                        predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                        ..PoolConfig::default()
                    },
                );
                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config.clone(), paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();

                let parent = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "flow",
                        "adapter": "shell",
                        "source": "manual",
                        "dedupKey": "fs2-parent",
                        "submission": {"mode": "full"},
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap();
                let parent_uuid = parent["task_uuid"].as_str().unwrap().to_owned();
                let child_payload = |key: &str| {
                    json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "orchestrator",
                        "dedupKey": key,
                        "submission": {"mode": "full"},
                        "callerJobId": parent_uuid,
                        "evidence": ["exit:0"]
                    })
                };
                let first = daemon
                    .handler
                    .enqueue(Some(child_payload("fs2-child-1")))
                    .await
                    .unwrap();
                {
                    let context = daemon.handler.context.read().await;
                    assert_eq!(
                        context.guardrails.parent(&parent_uuid).unwrap().outstanding,
                        1
                    );
                }
                let capped = daemon
                    .handler
                    .enqueue(Some(child_payload("fs2-child-at-cap")))
                    .await
                    .unwrap_err();
                assert_eq!(capped.code, WireErrorCode::InvalidParams);
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .guardrails
                        .parent(&parent_uuid)
                        .unwrap()
                        .outstanding,
                    1
                );

                let first_uuid = Uuid::parse_str(first["task_uuid"].as_str().unwrap()).unwrap();
                {
                    let mut context = daemon.handler.context.write().await;
                    finalize_forced_locked(
                        &mut context,
                        first_uuid,
                        Verdict::Cancelled,
                        false,
                        false,
                    )
                    .unwrap();
                    assert_eq!(
                        context.guardrails.parent(&parent_uuid).unwrap().outstanding,
                        0
                    );
                }

                let rollback = daemon
                    .handler
                    .enqueue(Some(json!({
                        "argv": ["true"],
                        "pool": "slot",
                        "adapter": "shell",
                        "source": "orchestrator",
                        "dedupKey": "fs2-rollback",
                        "callerJobId": parent_uuid,
                        "credentials": {"token": "/run/credentials/different-token"},
                        "evidence": ["exit:0"]
                    })))
                    .await
                    .unwrap_err();
                assert_eq!(rollback.code, WireErrorCode::InvalidParams);
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .guardrails
                        .parent(&parent_uuid)
                        .unwrap()
                        .outstanding,
                    0
                );

                let second = daemon
                    .handler
                    .enqueue(Some(child_payload("fs2-child-2")))
                    .await
                    .unwrap();
                assert_eq!(second["disposition"], "created");
                assert_eq!(
                    daemon
                        .handler
                        .context
                        .read()
                        .await
                        .guardrails
                        .parent(&parent_uuid)
                        .unwrap()
                        .outstanding,
                    1
                );
                drop(daemon);
                tokio::task::yield_now().await;

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let restarted = Daemon::open_with_executor(config, paths, settings(), executor)
                    .await
                    .unwrap();
                let second_uuid = Uuid::parse_str(second["task_uuid"].as_str().unwrap()).unwrap();
                let mut context = restarted.handler.context.write().await;
                assert_eq!(
                    context.guardrails.parent(&parent_uuid).unwrap().outstanding,
                    1
                );
                finalize_forced_locked(&mut context, second_uuid, Verdict::Cancelled, false, false)
                    .unwrap();
                assert_eq!(
                    context.guardrails.parent(&parent_uuid).unwrap().outstanding,
                    0
                );
            })
            .await;
    }

    #[test]
    fn daemon_paths_create_no_docs_or_deferred_scope() {
        let temp = tempdir().unwrap();
        let paths = DaemonPaths {
            socket: temp.path().join("run/tally.sock"),
            state_dir: temp.path().join("state"),
            data_dir: temp.path().join("data"),
        };
        prepare_paths(&paths).unwrap();
        assert!(paths.state_dir.is_dir());
        assert!(paths.data_dir.is_dir());
        assert!(paths.socket.parent().unwrap().is_dir());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
    }
}
