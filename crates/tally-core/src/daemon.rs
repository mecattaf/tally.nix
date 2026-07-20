use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
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

use crate::adapters::{AdapterEngine, AdapterError, AdapterInvocation, ScrapeResult};
use crate::config::{Config, PoolPredicate, Priority};
use crate::evidence::{
    parse_evidence_specs, probe_dedup, run_evidence_gate, RetryTrigger, RunOutcome,
};
use crate::executor::{
    ExecutionIdentity, ExecutionOutcome, ExecutionRequest, ExecutionTermination, Executor,
    ExecutorError, UnitLimits, Uuid,
};
use crate::journal::{EmitEvent, JournalEmitter, JournalEntry, TallyEvent};
use crate::lease::{
    bump_epoch, AdmitOutcome, LeaseBackend, LeaseEngine, LeaseError, LeaseEventLog, LeaseGrant,
    LeaseRequest, LocalLease, SystemdUnitLiveness,
};
use crate::producers::{
    acknowledged_ingress_ids, archive_ingress_claim, claim_ingress_files, read_ingress_payload,
    GhCliMutationSink, IngressOutcome, ProducerEngine, ReachabilityTransition,
};
use crate::query::{
    query_log, query_pools, query_render, query_standup, query_status, JobProjection, LogFilter,
    PoolHeadroomFact, RenderScope, RowFact, RowStatus, StandupOptions, WindowConsumptionFact,
};
use crate::recovery::{
    collect_durable_recovery_facts, collect_local_unit_facts, recover, DurableRecoveryFacts,
    RecoveryAction, RecoveryFacts, RecoveryIdentity, RecoveryPolicy, RecoveryRowState,
    RecoveryTriggers,
};
use crate::taskdb::{
    admits_durable_row, read_acknowledged_events, write_enqueue_event_atomic, AdmissionInput,
    DurableEnqueueEvent, EnqueueSource, RowSeed, TaskDb, TaskDbError,
};
use crate::wire::{
    serve_connection, EnqueuePayload, GuardrailConfig, GuardrailState, ParentInfo,
    ProducerDefaults, RequestFrame, RpcHandler, WireError, WireErrorCode, WireIoError,
};
use crate::witness::{
    append_attestation, read_verified_records, repair_attestation_tail, verify_attestations,
    AttestationRecord, LaborClass, Verdict, WitnessBody, WitnessError, WitnessLedger,
    WitnessRecord,
};

const LEASE_TICK: Duration = Duration::from_millis(100);
const MAX_RECENT_JOURNAL_ENTRIES: usize = 4_096;
const MAX_METER_EVENT_BYTES: u64 = 64 * 1024;

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
}

impl JobResult {
    fn value(&self) -> Value {
        json!({
            "task_uuid": self.task_uuid,
            "job_id": self.job_id,
            "verdict": self.verdict,
            "exit_code": self.exit_code,
            "artifact_content_hash": self.artifact_content_hash,
            "attempt": self.attempt,
            "lease_epoch": self.lease_epoch,
            "witness_seq": self.witness_seq,
        })
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
    job_results: HashMap<String, Value>,
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
        let barrier = format!("barrier:{stable_job_key}:{attempt}");
        let entry = self.barriers.entry(barrier.clone()).or_default();
        self.job_results.remove(stable_job_key);
        entry.results.remove(stable_job_key);
        entry.pending.insert(stable_job_key.to_owned());
        barrier
    }

    pub fn snapshot(&mut self, jobs: impl IntoIterator<Item = String>) -> String {
        self.next = self.next.saturating_add(1);
        let barrier = format!("barrier:drain:{}:{}", self.namespace, self.next);
        let mut entry = BarrierEntry::default();
        for job in jobs {
            if let Some(result) = self.job_results.get(&job) {
                entry.results.insert(job, result.clone());
            } else {
                entry.pending.insert(job);
            }
        }
        self.barriers.insert(barrier.clone(), entry);
        barrier
    }

    pub fn restore_job_result(&mut self, stable_job_key: String, value: Value) {
        self.complete_job(&stable_job_key, value);
    }

    fn complete_job(&mut self, stable_job_key: &str, value: Value) {
        self.job_results
            .insert(stable_job_key.to_owned(), value.clone());
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
            if let Some(entry) = self.barriers.get_mut(&barrier) {
                let result = barrier_value(&barrier, &entry.results);
                for waiter in std::mem::take(&mut entry.waiters) {
                    let _ = waiter.send(result.clone());
                }
            }
        }
    }

    fn wait_job(&mut self, stable_job_key: &str) -> WaitRegistration {
        if let Some(result) = self.job_results.get(stable_job_key) {
            return WaitRegistration::Ready(result.clone());
        }
        let (sender, receiver) = oneshot::channel();
        self.job_waiters
            .entry(stable_job_key.to_owned())
            .or_default()
            .push(sender);
        WaitRegistration::Pending(receiver)
    }

    fn wait_barrier(&mut self, barrier: &str) -> Result<WaitRegistration, WireError> {
        let entry = self
            .barriers
            .get_mut(barrier)
            .ok_or_else(|| WireError::not_found(format!("unknown barrier {barrier}")))?;
        if entry.pending.is_empty() {
            return Ok(WaitRegistration::Ready(barrier_value(
                barrier,
                &entry.results,
            )));
        }
        let (sender, receiver) = oneshot::channel();
        entry.waiters.push(sender);
        Ok(WaitRegistration::Pending(receiver))
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
    query_rows: BTreeMap<Uuid, RowFact>,
    recent_journal: Vec<JournalEntry>,
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
                    if row.session_ref.is_some() || row.model.is_some() {
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
    execution_shutdown: watch::Receiver<bool>,
    execution_cancel: broadcast::Sender<Uuid>,
    fatal: mpsc::UnboundedSender<DaemonError>,
    post_ack_tasks: Rc<RefCell<Vec<JoinHandle<()>>>>,
    pool_transition_tasks: Rc<RefCell<Vec<PoolTransitionTask>>>,
    ingress_sweep: Rc<Mutex<()>>,
    pool_transition_sweep: Rc<Mutex<()>>,
    gh_program: PathBuf,
    tally_socket: String,
}

impl RpcHandler for DaemonHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
        Box::pin(async move {
            match request.method.as_str() {
                "queue.enqueue" => self.enqueue(request.params).await,
                "queue.await_job" => self.await_job(request.params).await,
                "queue.await_barrier" => self.await_barrier(request.params).await,
                "queue.drain" => self.drain().await,
                "queue.pause" => self.pause(request.params).await,
                "queue.resume" => self.resume(request.params).await,
                "queue.cancel" => self.cancel(request.params).await,
                "__producer.pool-transition" => self.pool_transition(request.params).await,
                "lease.acquire" => self.acquire(request.params).await,
                "lease.release" => self.release(request.params).await,
                "lease.status" => self.lease_status(request.params).await,
                "query.status" | "query.log" | "query.render" | "query.standup" | "query.pools" => {
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

    async fn enqueue(&self, params: Option<Value>) -> Result<Value, WireError> {
        let payload: EnqueuePayload = decode_params(params)?;
        self.enqueue_payload(payload, None).await
    }

    async fn enqueue_payload(
        &self,
        payload: EnqueuePayload,
        ingress_id: Option<String>,
    ) -> Result<Value, WireError> {
        let caller_job_id = payload.caller_job_id.clone();
        let requested_adapter = payload
            .adapter
            .clone()
            .unwrap_or_else(|| "shell".to_owned());
        let mut context = self.context.write().await;
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
        };
        let mut resolved = context.guardrails.validate_enqueue(payload, &defaults)?;
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
                    rollback_child_charge(&mut context, caller_job_id.as_deref())?;
                    return Err(WireError::invalid(format!(
                        "credential {name:?} has conflicting pool and enqueue sources"
                    )));
                }
                resolved.credentials.entry(name).or_insert(source);
            }
        }
        let invocation = match AdapterEngine::new(&context.config.adapters)
            .launch(&resolved.adapter, &resolved.argv)
        {
            Ok(invocation) => invocation,
            Err(error) => {
                rollback_child_charge(&mut context, caller_job_id.as_deref())?;
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
            rollback_child_charge(&mut context, caller_job_id.as_deref())?;
            return Err(internal_wire(
                "RPC admissions must always have a durable recovery row",
            ));
        }
        let job_id = Uuid::new_v4();
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
            model: None,
            cwd: None,
            dedup_key: resolved.dedup_key.clone(),
            session_ref: None,
            lease_epoch: epoch,
            attempt: 1,
            argv: resolved.argv,
            evidence: resolved.evidence,
            parent_uuid,
            consumption_estimate: resolved.consumption_estimate,
            runtime_max_sec: resolved.runtime_max_sec,
            no_enqueue: resolved.no_enqueue,
            credentials: resolved.credentials,
            gh_origin: resolved.gh_origin,
            evidence_class: resolved.evidence_class,
            manifest_hash: resolved.manifest_hash.map(Value::String),
        };
        if let Err(error) = row.validate() {
            rollback_child_charge(&mut context, caller_job_id.as_deref())?;
            return Err(WireError::invalid(error.to_string()));
        }

        if let Some(dedup_key) = row.dedup_key.clone() {
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
                    rollback_child_charge(&mut context, caller_job_id.as_deref())?;
                    return Err(internal_wire(error));
                }
                Err(error) => {
                    rollback_child_charge(&mut context, caller_job_id.as_deref())?;
                    return Err(internal_wire(format!("dedup worker failed: {error}")));
                }
            };
            if dedup.hit {
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
                        rollback_child_charge(&mut context, caller_job_id.as_deref())?;
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
                    rollback_child_charge(&mut context, caller_job_id.as_deref())?;
                    if matches!(&error, TaskDbError::InvalidEvent { .. }) {
                        return Err(WireError::invalid(error.to_string()));
                    }
                    return Err(internal_wire(error));
                }
                let record = match context.witness.append(WitnessBody {
                    task_uuid: task_uuid.map(|uuid| uuid.to_string()),
                    transition_timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                    verdict: Verdict::Reused,
                    exit_code: 0,
                    artifact_content_hash: Some(artifact_hash.clone()),
                    gpu_seconds: None,
                    wall_clock: 0.0,
                    attempt: row.attempt,
                    lease_epoch: row.lease_epoch,
                    dedup_key: row.dedup_key.clone(),
                    labor_class: LaborClass::Reused,
                    trace_ref: None,
                    pools: Some(row.pools.clone()),
                    executor: row.executor.clone(),
                    charge: None,
                    model: row.model.clone(),
                    evidence_class: row.evidence_class.clone(),
                    manifest_hash: row.manifest_hash.clone(),
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
                };
                context.barriers.complete_job(&stable_key, result.value());
                context.aliases.insert(job_id.to_string(), job_id);
                context.aliases.insert(stable_key.clone(), job_id);
                context.guardrails.register_parent(
                    job_id.to_string(),
                    ParentInfo {
                        parent_uuid: stable_key.clone(),
                        depth: resolved.depth,
                        children: 0,
                        no_enqueue: row.no_enqueue,
                    },
                );
                context
                    .query_rows
                    .insert(row_uuid, query_row(&row, RowStatus::Completed));
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
            rollback_child_charge(&mut context, caller_job_id.as_deref())?;
            return Err(lease_wire(error));
        }

        if task_uuid.is_some() {
            let event = match DurableEnqueueEvent::new_with_depth(row.clone(), resolved.depth)
                .and_then(|event| event.with_ingress_id(ingress_id))
            {
                Ok(event) => event,
                Err(error) => {
                    rollback_child_charge(&mut context, caller_job_id.as_deref())?;
                    return Err(WireError::invalid(error.to_string()));
                }
            };
            let events_dir = context.paths.events_dir();
            if let Err(error) = write_enqueue_event_atomic(&events_dir, &event) {
                let renamed = events_dir.join(format!("{}.enqueue.json", event.event_id));
                if renamed.exists() {
                    return Err(self.fail_stop(error.into()));
                }
                rollback_child_charge(&mut context, caller_job_id.as_deref())?;
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
                children: 0,
                no_enqueue: row.no_enqueue,
            },
        );
        if task_uuid.is_some() {
            context
                .query_rows
                .insert(row_uuid, query_row(&row, RowStatus::Pending));
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
        Ok(json!({
            "task_uuid": task_uuid.map(|uuid| uuid.to_string()),
            "job_id": job_id.to_string(),
            "barrier": barrier,
            "state": state_name(job.state),
        }))
    }

    async fn await_job(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            #[serde(default)]
            task_uuid: Option<String>,
            #[serde(default)]
            job_id: Option<String>,
        }
        let params: Params = decode_params(params)?;
        let presented = match (params.task_uuid, params.job_id) {
            (Some(task_uuid), None) => task_uuid,
            (None, Some(job_id)) => job_id,
            _ => {
                return Err(WireError::invalid(
                    "provide exactly one of task_uuid or job_id",
                ));
            }
        };
        let registration = {
            let mut context = self.context.write().await;
            let stable = context
                .aliases
                .get(&presented)
                .map(ToString::to_string)
                .ok_or_else(|| WireError::not_found(format!("job {presented} was not found")))?;
            context.barriers.wait_job(&stable)
        };
        await_registration(registration).await
    }

    async fn await_barrier(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            barrier: String,
        }
        let params: Params = decode_params(params)?;
        let registration = self
            .context
            .write()
            .await
            .barriers
            .wait_barrier(&params.barrier)?;
        await_registration(registration).await
    }

    async fn drain(&self) -> Result<Value, WireError> {
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
            let payload = match read_ingress_payload(&claim) {
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
        Ok(json!({"paused": pools, "affected": queued.len()}))
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
        Ok(json!({"resumed": pools}))
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
        for row in &represented_rows {
            context
                .query_rows
                .insert(row.uuid, query_row(row, RowStatus::Pending));
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
            let evidence = json!({
                "taskUuid": row.uuid.to_string(),
                "witnessSeq": result.witness_seq,
                "verdict": result.verdict,
                "exitCode": result.exit_code,
                "artifactContentHash": result.artifact_content_hash,
            });
            let mut retry_delay = Duration::from_secs(1);
            loop {
                let registry = registry.clone();
                let events_dir = events_dir.clone();
                let state_dir = state_dir.clone();
                let gh_program = gh_program.clone();
                let origin = origin.clone();
                let completion_id = completion_id.clone();
                let evidence = evidence.clone();
                let verdict = result.verdict;
                let completed = tokio::task::spawn_blocking(move || {
                    let engine = ProducerEngine::new(&registry, events_dir, state_dir);
                    let mut sink = GhCliMutationSink::with_program(gh_program);
                    engine.complete_gh_once(
                        &origin,
                        &completion_id,
                        verdict,
                        Some(evidence),
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
            let (adapters, attestation_path) = {
                let context = handler.context.read().await;
                (
                    context.config.adapters.clone(),
                    context.paths.attestations_path(),
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
                Ok::<_, String>((captures, attestation_error))
            })
            .await;

            let (captures, attestation_error) = match scraped {
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
            {
                let mut context = handler.context.write().await;
                if let Some(stored) = context.jobs.get_mut(&enriched.job_id) {
                    stored.row.session_ref.clone_from(&enriched.row.session_ref);
                    stored.row.model.clone_from(&enriched.row.model);
                }
                if let Some(task_uuid) = enriched.task_uuid {
                    if let Some(row) = context.query_rows.get_mut(&task_uuid) {
                        row.session_ref.clone_from(&enriched.row.session_ref);
                        row.model.clone_from(&enriched.row.model);
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
        let (rows, journal, witness_path, live_states) = {
            let context = self.context.read().await;
            (
                context.query_rows.values().cloned().collect::<Vec<_>>(),
                context.recent_journal.clone(),
                context.paths.witness_path(),
                context
                    .jobs
                    .values()
                    .filter(|job| job.state != JobState::Completed)
                    .map(|job| (job.stable_key(), state_name(job.state).to_owned()))
                    .collect::<HashMap<_, _>>(),
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
                #[serde(deny_unknown_fields)]
                struct Params {
                    #[serde(default)]
                    task: Option<String>,
                    #[serde(default)]
                    session: Option<String>,
                    #[serde(default)]
                    event: Option<TallyEvent>,
                    #[serde(default)]
                    source: Option<String>,
                    #[serde(default)]
                    since: Option<String>,
                }
                let params: Params = decode_params(params)?;
                serde_json::to_value(
                    query_log(
                        &rows,
                        &journal,
                        &witness,
                        &LogFilter {
                            task: params.task,
                            session: params.session,
                            event: params.event,
                            source: params.source,
                            since: params.since,
                        },
                    )
                    .map_err(query_wire)?,
                )
                .map_err(internal_wire)
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
        let executor = self.executor.clone();
        let completion = self.completion.clone();
        let request = execution_request(&job, self.settings.unit_limits, &self.tally_socket);
        let execution_target = job.row.executor.clone();
        let evidence = job.row.evidence.clone();
        let mut shutdown = self.execution_shutdown.clone();
        let mut cancellation = self.execution_cancel.subscribe();
        tokio::task::spawn_local(async move {
            let started = Instant::now();
            let execution = async {
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
        let journal = self.journal.clone();
        let context = self.context.clone();
        tokio::task::spawn_local(async move {
            tokio::task::yield_now().await;
            match journal.emit(event) {
                Ok(fields) => {
                    let timestamp = Utc::now().timestamp_micros();
                    let mut context = context.write().await;
                    context.recent_journal.push(JournalEntry {
                        fields,
                        realtime_us: u64::try_from(timestamp).ok(),
                    });
                    let excess = context
                        .recent_journal
                        .len()
                        .saturating_sub(MAX_RECENT_JOURNAL_ENTRIES);
                    if excess > 0 {
                        context.recent_journal.drain(..excess);
                    }
                }
                Err(error) => {
                    eprintln!("tally: journald emission failed outside ack barrier: {error}");
                }
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

fn rollback_child_charge(
    context: &mut Context,
    caller_job_id: Option<&str>,
) -> Result<(), WireError> {
    if let Some(caller_job_id) = caller_job_id {
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
                PoolPredicate::WindowedConsumption(window) => {
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
            let meter = pool.usage_meter.as_ref().and_then(|meter| {
                read_usage_meter(
                    &context.paths.state_dir,
                    &name,
                    meter.poll_interval_sec,
                    now,
                )
            });
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

#[derive(Debug, Deserialize)]
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
    poll_interval_sec: u64,
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
    let freshness_sec = poll_interval_sec.checked_mul(2)?;
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

async fn await_registration(registration: WaitRegistration) -> Result<Value, WireError> {
    match registration {
        WaitRegistration::Ready(value) => Ok(value),
        WaitRegistration::Pending(receiver) => receiver
            .await
            .map_err(|_| internal_wire("daemon stopped while waiting")),
    }
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
    LeaseRequest {
        job_id: job.job_id.to_string(),
        unit,
        pools: job.row.pools.clone(),
        priority: job.row.priority,
        admission_key: Some(format!("{}:{}", job.stable_key(), job.row.attempt)),
        consumption_estimate: job.row.consumption_estimate,
    }
}

fn execution_request(job: &Job, limits: UnitLimits, tally_socket: &str) -> ExecutionRequest {
    ExecutionRequest {
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
        cwd: job.row.cwd.clone(),
        credentials: job.row.credentials.clone(),
        limits,
        runtime_max_sec: job.row.runtime_max_sec,
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
    event.job_id = Some(job.job_id.to_string());
    event.parent = job.row.parent_uuid.map(|uuid| uuid.to_string());
    event.pools = Some(job.row.pools.clone());
    event.executor = job.row.executor.clone();
    event
}

fn canonical_job_model(job: &Job) -> Option<String> {
    if job.model_is_advisory {
        None
    } else {
        job.row.model.clone()
    }
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
        labor_class: job.labor_class,
        trace_ref: None,
        pools: Some(job.row.pools.clone()),
        executor: job.row.executor.clone(),
        charge: None,
        model: canonical_job_model(job),
        evidence_class: job.row.evidence_class.clone(),
        manifest_hash: job.row.manifest_hash.clone(),
    }
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
    };
    context
        .barriers
        .complete_job(&job.stable_key(), result.value());
    let stored = context.jobs.get_mut(&job_id).expect("job exists");
    stored.state = JobState::Completed;
    if release_lease {
        stored.lease_id = None;
    }
    if let Some(task_uuid) = job.task_uuid {
        if let Some(row) = context.query_rows.get_mut(&task_uuid) {
            row.status = if verdict == Verdict::Cancelled {
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
        launches,
        scrape_capture,
    }))
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
            .filter(|recovery| recovery.row.session_ref.is_some() || recovery.row.model.is_some())
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
        let event_log = LeaseEventLog::in_state_dir(&paths.state_dir);
        let lease_engine = LeaseEngine::from_durable(
            epoch,
            settings.yield_grace,
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
            query_rows,
            recent_journal: Vec::new(),
        };
        restore_completed_results(&mut context, completed_witness)?;
        let initial_jobs = install_recovery_jobs(&mut context, &plan, &executor)?;

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
        let handler = DaemonHandler {
            context: Rc::new(RwLock::new(context)),
            settings,
            executor,
            completion: completion_tx,
            commits: commit_tx,
            journal: JournalEmitter::from_config(&config.journald),
            execution_shutdown: execution_shutdown_rx,
            execution_cancel,
            fatal: fatal_tx,
            post_ack_tasks,
            pool_transition_tasks,
            ingress_sweep: Rc::new(Mutex::new(())),
            pool_transition_sweep: Rc::new(Mutex::new(())),
            gh_program: PathBuf::from("gh"),
            tally_socket,
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
                                    connections.push(tokio::task::spawn_local(async move {
                                        if let Err(error) = serve_connection(stream, &handler).await {
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
        let (computed_verdict, exit_code, artifact_hash) = match finished.outcome {
            None => {
                return Err(DaemonError::Invalid(format!(
                    "job {} stopped without a terminal witness",
                    job.stable_key()
                )))
            }
            Some(Ok(outcome)) => match outcome.termination {
                ExecutionTermination::RuntimeExceeded => (Verdict::RuntimeExceeded, 1, None),
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
                    (gate.verdict, code, gate.artifact_hash)
                }
                ExecutionTermination::Signaled { .. }
                | ExecutionTermination::ServiceFailed { .. } => (Verdict::Failed, 1, None),
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
                (Verdict::Failed, 1, None)
            }
        };

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
                labor_class: job.labor_class,
                trace_ref: None,
                pools: Some(job.row.pools.clone()),
                executor: job.row.executor.clone(),
                charge: None,
                model: canonical_job_model(&job),
                evidence_class: job.row.evidence_class.clone(),
                manifest_hash: job.row.manifest_hash.clone(),
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
            };
            let stable = job.stable_key();
            context.barriers.complete_job(&stable, result.value());
            let stored = context.jobs.get_mut(&finished.job_id).expect("job exists");
            stored.state = JobState::Completed;
            if let Some(task_uuid) = job.task_uuid {
                if let Some(row) = context.query_rows.get_mut(&task_uuid) {
                    row.status = RowStatus::Completed;
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
                    labor_class: LaborClass::Reused,
                    trace_ref: None,
                    pools: Some(event.row.pools.clone()),
                    executor: event.row.executor.clone(),
                    charge: None,
                    model: event.row.model.clone(),
                    evidence_class: event.row.evidence_class.clone(),
                    manifest_hash: event.row.manifest_hash.clone(),
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
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (uuid, session_ref, model) in updates {
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
        status,
        priority: priority_name(row.priority).to_owned(),
        pools: Some(row.pools.clone()),
        executor: row.executor.clone(),
        source: Some(source_name(row.source).to_owned()),
        session_ref: row.session_ref.clone(),
        attempt: row.attempt,
        model: row.model.clone(),
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

fn restore_completed_results(
    context: &mut Context,
    records: Vec<crate::witness::WitnessRecord>,
) -> Result<(), DaemonError> {
    for record in records {
        let Some(task_uuid) = record.task_uuid.clone() else {
            continue;
        };
        let value = JobResult {
            task_uuid: Some(task_uuid.clone()),
            job_id: task_uuid.clone(),
            verdict: record.verdict,
            exit_code: record.exit_code,
            artifact_content_hash: record.artifact_content_hash,
            attempt: record.attempt,
            lease_epoch: record.lease_epoch,
            witness_seq: record.seq,
        }
        .value();
        context.barriers.register_job(&task_uuid, record.attempt);
        context
            .barriers
            .restore_job_result(task_uuid.clone(), value);
        let uuid = Uuid::parse_str(&task_uuid)
            .map_err(|_| DaemonError::Invalid(format!("invalid witnessed UUID {task_uuid}")))?;
        context.aliases.insert(task_uuid, uuid);
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
                children: child_counts.get(&task_uuid).copied().unwrap_or(0),
                no_enqueue: job.row.no_enqueue,
            },
        );
        if let Some(row) = context.query_rows.get_mut(&task_uuid) {
            row.session_ref.clone_from(&job.row.session_ref);
            row.model.clone_from(&job.row.model);
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
        || existing.row.adapter != candidate.adapter
        || existing.row.argv != candidate.argv
        || existing.row.dedup_key != candidate.dedup_key
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
            let invocation = engine.resume(&row.adapter, &row.argv, &captures)?;
            Ok((invocation, Some(captures)))
        }
        RecoveryAction::QueueExisting { .. } => Ok((engine.launch(&row.adapter, &row.argv)?, None)),
        RecoveryAction::AdoptRunning { .. } | RecoveryAction::ReconcileExit { .. } => Ok((
            AdapterInvocation {
                argv: row.argv.clone(),
                env: BTreeMap::new(),
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
    use crate::adapters::{AdapterConfig, ScrapeCapture, ScrapeMode, ScrapeStream};
    use crate::config::{
        CoResidencyPredicate, ExecutionTargetConfig, JournaldConfig, MeterBudgetClass, PoolConfig,
        PoolPredicate, ResourceKind, SshExecutorConfig, UsageMeterConfig,
    };
    use crate::evidence::RetryPolicy;
    use crate::executor::{
        read_exit_record, write_exit_record, ExecutionPaths, LocalUnitFact, LocalUnitProbe,
        LocalUnitState, RemoteCapture, RemoteCompletion, RemoteExecutorReply,
        RemoteExecutorRequest, RemoteExecutorResult, RemoteTransport, RemoteTransportError,
        UnitExitRecord, REMOTE_EXECUTOR_PROTOCOL_VERSION, UNIT_EXIT_SCHEMA_VERSION,
    };
    use crate::producers::{GhObservation, ProducerConfig, ProducerEngine, ReachabilityTransition};
    use crate::recovery::RecoveryPlan;
    use crate::taskdb::GhOrigin;
    use crate::wire::RpcClient;

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
                    result: Box::new(RemoteExecutorResult::Completion(RemoteCompletion {
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
                    })),
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
                        RemoteExecutorResult::Completion(RemoteCompletion {
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
                        })
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
            ]),
            yield_hook: Some(vec![
                "tally".to_owned(),
                "lease".to_owned(),
                "status".to_owned(),
            ]),
            env: BTreeMap::from([("CUSTOM_AGENT_MODE".to_owned(), "batch".to_owned())]),
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
            dedup_key: Some(dedup_key.to_owned()),
            session_ref: None,
            lease_epoch,
            attempt: 1,
            argv: vec!["true".to_owned()],
            evidence: vec!["exit:0".to_owned()],
            parent_uuid: None,
            consumption_estimate: None,
            runtime_max_sec: None,
            no_enqueue: false,
            credentials: BTreeMap::new(),
            gh_origin: None,
            evidence_class: None,
            manifest_hash: None,
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
    fn terminal_witness_beats_a_stale_live_query_snapshot() {
        let projection = |witness_seq: Option<u64>| JobProjection {
            anchor: "job-1".to_owned(),
            task_uuid: Some("job-1".to_owned()),
            description: None,
            pools: Some(vec!["slot".to_owned()]),
            executor: None,
            source: Some("manual".to_owned()),
            session_ref: None,
            model: None,
            state: "completed".to_owned(),
            verdict: witness_seq.map(|_| Verdict::Pass),
            gpu_seconds: None,
            canonical_gpu_seconds: None,
            last_event_at: None,
            witness_seq,
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

                let drained = daemon.handler.drain().await.unwrap();
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

                let repaired = daemon.handler.drain().await.unwrap();
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
                let transient_error = daemon.handler.drain().await.unwrap_err();
                assert_eq!(transient_error.code, WireErrorCode::Internal);
                assert!(transient_claim.path.exists());
                assert!(!paths.events_dir().join("rejected/transient.json").exists());
                fs::set_permissions(&transient_claim.path, fs::Permissions::from_mode(0o600))
                    .unwrap();
                let retried = daemon.handler.drain().await.unwrap();
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
                let daemon_context = daemon.handler.context.clone();
                let (shutdown_tx, shutdown_rx) = watch::channel(false);
                let daemon_task = tokio::task::spawn_local(daemon.run_until(shutdown_rx));
                let mut client = RpcClient::connect(&paths.socket).await.unwrap();

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
                    .as_array()
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
                let context = daemon_context.read().await;
                assert!(context.recent_journal.iter().any(|entry| {
                    entry.fields.task_uuid == task_uuid
                        && entry.fields.pools.as_deref()
                            == Some(["slot".to_owned(), "zeta".to_owned()].as_slice())
                }));
                drop(context);

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

                let mut config = one_pool_config();
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
                        yield_hook: None,
                        env: BTreeMap::new(),
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
                assert_eq!(records.len(), 1);
                assert_eq!(records[0].verdict, Verdict::PoolVanished);
                assert_eq!(records[0].attempt, 1);

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
                assert_eq!(records.len(), 2);
                assert_eq!(records[1].verdict, Verdict::Pass);
                assert_eq!(records[1].attempt, 2);
                assert_eq!(records[1].labor_class, LaborClass::Recovered);
                daemon.handler.drain_post_ack_tasks().await;
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
                let mut client = RpcClient::connect(&paths.socket).await.unwrap();
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
                        labor_class: LaborClass::Fresh,
                        trace_ref: None,
                        pools: Some(vec!["slot".to_owned()]),
                        executor: None,
                        charge: None,
                        model: None,
                        evidence_class: None,
                        manifest_hash: None,
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
                        "sources": ["notifications"],
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
                row.gh_origin = Some(GhOrigin {
                    producer: "github".to_owned(),
                    source: "notifications".to_owned(),
                    item_id: "item-1".to_owned(),
                    actor: "contributor".to_owned(),
                    self_actor: "tally-bot".to_owned(),
                    actor_exclude: "self".to_owned(),
                });
                let result = JobResult {
                    task_uuid: Some(row.uuid.to_string()),
                    job_id: row.uuid.to_string(),
                    verdict: Verdict::Pass,
                    exit_code: 0,
                    artifact_content_hash: Some("sha256:artifact".to_owned()),
                    attempt: 1,
                    lease_epoch: 1,
                    witness_seq: 9,
                };
                daemon
                    .handler
                    .complete_gh_post_ack(row.clone(), result.clone());
                daemon
                    .handler
                    .complete_gh_post_ack(row.clone(), result.clone());
                daemon.handler.drain_post_ack_tasks().await;

                assert_eq!(fs::read(&calls).unwrap(), b"xxx");
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
                let evidence: Value = serde_json::from_str(
                    comment["variables"]["body"]
                        .as_str()
                        .unwrap()
                        .split_once('\n')
                        .unwrap()
                        .1,
                )
                .unwrap();
                assert_eq!(evidence["producer"], "github");
                assert_eq!(evidence["source"], "notifications");
                assert_eq!(evidence["itemId"], "item-1");
                assert_eq!(evidence["state"], "COMPLETED");
                assert_eq!(evidence["evidence"]["taskUuid"], row.uuid.to_string());
                assert_eq!(evidence["evidence"]["witnessSeq"], 9);
                assert_eq!(evidence["evidence"]["verdict"], "pass");

                let mut failed = result;
                failed.witness_seq = 10;
                failed.verdict = Verdict::Failed;
                daemon.handler.complete_gh_post_ack(row, failed);
                daemon.handler.drain_post_ack_tasks().await;
                assert_eq!(fs::read(calls).unwrap(), b"xxx");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn every_in_scope_enqueuing_producer_converges_on_the_one_daemon_path() {
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
                        "sources": ["notifications"],
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
                        &GhObservation {
                            source: "notifications".to_owned(),
                            item_id: "PR-live-producer".to_owned(),
                            actor: "contributor".to_owned(),
                            self_actor: "tally-bot".to_owned(),
                        },
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

                let executor = Executor::new(&paths.state_dir, std::env::current_exe().unwrap())
                    .with_systemd_run(temp.path().join("absent-systemd-run"))
                    .with_unit_probe(ExitFileProbe);
                let daemon =
                    Daemon::open_with_executor(config, paths.clone(), settings(), executor)
                        .await
                        .unwrap();
                daemon
                    .handler
                    .pause(Some(json!({"all": true})))
                    .await
                    .unwrap();
                let drained = daemon.handler.drain().await.unwrap();
                assert_eq!(drained["enqueued"], 6);
                assert_eq!(drained["rejected"], 0);

                let context = daemon.handler.context.read().await;
                for (source, expected) in [
                    (EnqueueSource::Calendar, 1),
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
                drop(context);
                assert!(crate::taskdb::read_acknowledged_events(&paths.events_dir())
                    .unwrap()
                    .iter()
                    .all(|event| event.ingress_id.is_some()));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn custom_adapter_launch_scrapes_post_ack_and_usage_is_attestation_only() {
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
                        "printf '%s\\n' '{\"event\":{\"session_id\":\"session-opaque\",\"model\":\"Provider/Model.Exact-CASE\",\"usage\":{\"input_tokens\":999999}}}'\n",
                        "printf '%s\\n' 'branch=adapter-test' >&2\n",
                        "sleep 0.1\n"
                    ),
                )
                .unwrap();
                fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
                let mut config = one_pool_config();
                config
                    .adapters
                    .insert("from-nix".to_owned(), structured_adapter(&program));
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

    #[tokio::test(flavor = "current_thread")]
    async fn late_job_and_barrier_waits_are_immediate() {
        let mut tracker = BarrierTracker::with_namespace(41);
        let barrier = tracker.register_job("task-1", 1);
        tracker.complete_job("task-1", json!({"verdict": "pass"}));
        assert!(matches!(
            tracker.wait_job("task-1"),
            WaitRegistration::Ready(_)
        ));
        assert!(matches!(
            tracker.wait_barrier(&barrier).unwrap(),
            WaitRegistration::Ready(_)
        ));
        assert_eq!(tracker.snapshot(Vec::new()), "barrier:drain:41:1");
        assert_eq!(
            BarrierTracker::with_namespace(42).snapshot(Vec::new()),
            "barrier:drain:42:1"
        );
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
                let mut client = RpcClient::connect(&paths.socket).await.unwrap();
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
                let mut client = RpcClient::connect(&paths.socket).await.unwrap();
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

                let mut client = RpcClient::connect(&paths.socket).await.unwrap();
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
                let mut client = RpcClient::connect(&paths.socket).await.unwrap();
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
    async fn restart_rebuilds_cache_and_preserves_terminal_wait_without_reexecution() {
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
                let mut client = RpcClient::connect(&paths.socket).await.unwrap();
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
                let mut restarted_client = RpcClient::connect(&paths.socket).await.unwrap();
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
                        labor_class: LaborClass::Fresh,
                        trace_ref: None,
                        pools: Some(vec!["slot".to_owned()]),
                        executor: None,
                        charge: None,
                        model: None,
                        evidence_class: None,
                        manifest_hash: None,
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
                let mut client = RpcClient::connect(&paths.socket).await.unwrap();
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
                assert_eq!(status["protocolVersion"], 1);
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
                    .is_some_and(|text| text.contains("\"protocolVersion\": 1")));
                let render_json = client
                    .call("query.render", Some(json!({"format": "json"})))
                    .await
                    .unwrap();
                assert_eq!(render_json["protocolVersion"], 1);

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
                let mut reopened_client = RpcClient::connect(&reopened_socket).await.unwrap();
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
                let mut client = RpcClient::connect(&paths.socket).await.unwrap();
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
                let mut restarted = RpcClient::connect(&paths.socket).await.unwrap();
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
