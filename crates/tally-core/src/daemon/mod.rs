mod barriers;
mod notify;
mod replica;
mod supervise;

pub use barriers::BarrierTracker;
pub use notify::SystemdNotifier;
pub use supervise::{
    spawn_supervised, SupervisedFactory, SupervisedFuture, SupervisedTask, SupervisionEvent,
};

#[cfg(test)]
pub(crate) use barriers::WaitRegistration;
pub(crate) use barriers::{await_registration, parse_job_barrier, single_job_barrier_value};
pub(crate) use notify::watchdog_tick;
pub(crate) use replica::{spawn_commit_worker, CommitCommand, ReplicaCommitter, TaskDbCommitter};

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
    evaluate_completion, ExecutionFact, ExecutionStatus, GateManifestSpec, GateSummaryStatus,
    SemanticCompletion,
};
use crate::config::{Config, GitAiConfig, PoolPredicate, Priority};
use crate::evidence::{
    parse_evidence_specs, probe_dedup, probe_full_pass, run_evidence_gate, CheckOutcome,
    DedupMissReason, RetryTrigger, RunOutcome,
};
use crate::exec_attestation::ExecAttestationContext;
use crate::executor::{
    ExecutionIdentity, ExecutionOutcome, ExecutionRequest, ExecutionTermination, Executor,
    ExecutorError, UnitLimits, Uuid,
};
use crate::git_ai::GitAiExecution;
use crate::history::{HistoryError, LifecycleStore};
use crate::journal::{EmitEvent, JournalEmitter, JournalEntry, TallyEvent};
use crate::lease::{
    bump_epoch, AdmitOutcome, LeaseBackend, LeaseEngine, LeaseError, LeaseEventLog, LeaseGrant,
    LeaseRequest, LeaseSchedulingGroup, LocalLease, SystemdUnitLiveness,
};
use crate::nix_store::{DerivationAvailability, NixStore};
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
use crate::retention::{
    acquire_registration_lock, gcroots_lock_path, parse_horizon, reconcile_recent_roots,
    register_record_roots, GcRootsLock, RetentionError,
};
use crate::taskdb::{
    admits_durable_row, migrate_acknowledged_events, read_acknowledged_events,
    update_enqueue_event_atomic, write_enqueue_event_atomic, AdmissionInput, AdmissionOrigin,
    DurableEnqueueEvent, DurableRetry, EnqueueSource, RowSeed, TaskDb, TaskDbError,
};
use crate::trace::{query_trace, trace_availability, TraceError, TraceLane};
use crate::watch::{ChangeError, ChangeKind, ChangeStore};
use crate::wire::{
    canonical_payload_hash, serve_connection_with_limits, EnqueuePayload, GuardrailConfig,
    GuardrailState, ParentInfo, ProducerDefaults, RequestFrame, RpcHandler, SubmissionMode,
    WireError, WireErrorCode, WireIoError,
};
use crate::witness::{
    append_attestation, current_host_id, read_verified_attestations, read_verified_records,
    repair_attestation_tail, verify_attestations, AttestationRecord, Derivation, LaborClass,
    Verdict, WitnessBody, WitnessError, WitnessLedger, WitnessRecord,
};

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
    host_id: String,
    epoch: u64,
    lease: LocalLease<SystemdUnitLiveness>,
    guardrails: GuardrailState,
    witness: WitnessLedger,
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
    query_rows: BTreeMap<Uuid, RowFact>,
    query_details: BTreeMap<Uuid, RowDetailFact>,
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
    git_ai: GitAiConfig,
    exec_attestations: bool,
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
                if full_mode && !resolved.credentials.contains_key(&name) {
                    rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                    return Err(WireError::invalid(format!(
                        "full-mode enqueue omitted credential {name:?} required by pool {pool:?}"
                    )));
                }
                if !full_mode {
                    resolved.credentials.entry(name).or_insert(source);
                }
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
            row_version: crate::taskdb::CURRENT_ROW_VERSION,
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
            drv: resolved.drv,
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

        if full_mode {
            if let (Some(orchestration), Some(dedup_key), Some(payload_hash)) = (
                row.orchestration.as_ref(),
                row.dedup_key.as_deref(),
                row.payload_hash.as_deref(),
            ) {
                if let Some(node_ordinal) = orchestration.node_ordinal() {
                    let conflicts = context
                        .rows
                        .values()
                        .filter(|existing| existing.dedup_key.as_deref() != Some(dedup_key))
                        .filter(|existing| {
                            existing.orchestration.as_ref().is_some_and(|recorded| {
                                recorded.flow_run_id() == orchestration.flow_run_id()
                                    && recorded.node_ordinal() == Some(node_ordinal)
                            })
                        })
                        .map(|existing| DedupConflictCandidate {
                            task_uuid: existing.uuid.to_string(),
                            payload_hash: existing.payload_hash.clone(),
                            orchestration: existing.orchestration.clone(),
                        })
                        .collect::<Vec<_>>();
                    if !conflicts.is_empty() {
                        rollback_child_charge(
                            &mut context,
                            caller_job_id.as_deref(),
                            child_charged,
                        )?;
                        return Err(dedup_conflict(dedup_key, payload_hash, conflicts));
                    }
                }
            }
        }

        if let Some(drv) = row.drv.clone() {
            if let Some(existing) = latest_witness_for_task(&context.paths.witness_path(), job_id)?
            {
                if full_mode
                    && existing.payload_hash == row.payload_hash
                    && existing.drv.as_ref() == Some(&drv)
                {
                    return full_terminal_response(
                        &existing,
                        row.payload_hash
                            .as_deref()
                            .expect("full drv rows carry a payload hash"),
                        "terminal",
                    );
                }
                rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                return Err(WireError::invalid(format!(
                    "drv seed task UUID {job_id} already has witness seq {}",
                    existing.seq
                )));
            }

            let probe_drv = drv.clone();
            let derivation_store = context.derivation_store.clone();
            drop(context);
            let substitution = tokio::task::spawn_blocking(move || {
                derivation_store.outputs_available_or_substitutable(&probe_drv)
            })
            .await;
            context = self.context.write().await;
            let substituted = match substitution {
                Ok(Ok(substituted)) => substituted,
                Ok(Err(error)) => {
                    eprintln!(
                        "tally: drv substitution probe failed for {}: {error}",
                        drv.drv_path
                    );
                    false
                }
                Err(error) => {
                    eprintln!(
                        "tally: drv substitution worker failed for {}: {error}",
                        drv.drv_path
                    );
                    false
                }
            };
            if context.jobs.contains_key(&job_id) || context.query_rows.contains_key(&job_id) {
                rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                return Err(WireError::invalid(format!(
                    "task UUID {job_id} was admitted while its drv substitution was checked"
                )));
            }
            if let Some(existing) = latest_witness_for_task(&context.paths.witness_path(), job_id)?
            {
                if full_mode
                    && existing.payload_hash == row.payload_hash
                    && existing.drv.as_ref() == Some(&drv)
                {
                    return full_terminal_response(
                        &existing,
                        row.payload_hash
                            .as_deref()
                            .expect("full drv rows carry a payload hash"),
                        "terminal",
                    );
                }
                rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                return Err(WireError::invalid(format!(
                    "drv seed task UUID {job_id} gained witness seq {} while substitution was checked",
                    existing.seq
                )));
            }
            if substituted {
                if let Err(error) =
                    store_admitted_brief(&context.paths, &row, prepared_brief.as_ref())
                {
                    rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                    return Err(error);
                }
                rollback_child_charge(&mut context, caller_job_id.as_deref(), child_charged)?;
                let record =
                    append_context_witness(&mut context, substituted_witness(&row, drv.clone()))
                        .map_err(|error| self.fail_stop(error.into()))?;
                let mut response = json!({
                    "schemaVersion": 1,
                    "disposition": "substituted",
                    "task_uuid": job_id.to_string(),
                    "taskUuid": job_id.to_string(),
                    "job_id": job_id.to_string(),
                    "state": "substituted",
                    "status": "substituted",
                    "verdict": Verdict::Substituted,
                    "exit_code": 0,
                    "dedup_key": row.dedup_key,
                    "store_paths": record.store_paths,
                    "storePaths": record.store_paths,
                    "drv": record.drv,
                    "witness_lsn": record.seq,
                    "witnessSeq": record.seq,
                    "attempt": 1,
                    "lease_epoch": 1,
                });
                if let Some(payload_hash) = &row.payload_hash {
                    response["payloadHash"] = Value::String(payload_hash.clone());
                }
                return Ok(response);
            }
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
                    let probe_substituted = row.drv.is_some();
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
                                && (record.verdict == Verdict::Pass
                                    || (probe_substituted
                                        && record.verdict == Verdict::Substituted)))
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
                    if governing.verdict != Verdict::Pass
                        && !(row.drv.is_some() && governing.verdict == Verdict::Substituted)
                    {
                        return full_terminal_response(&governing, payload_hash, "terminal");
                    }
                    let pass_probe = pass_probe.expect(
                        "matching successful governing records are evidence-probed in the worker",
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
                        Some(DedupMissReason::StorePathInvalid(path)) => {
                            reused_rejected = Some("store-path-invalid");
                            reuse_error_detail = Some(path.to_string_lossy().into_owned());
                        }
                        Some(DedupMissReason::WitnessStorePathsMismatch) => {
                            reused_rejected = Some("store-path-drift");
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
                    let matched_witness_seq = dedup
                        .matched_witness_seq
                        .expect("a dedup hit always carries a matched witness");
                    let event = match DurableEnqueueEvent::new_reuse_with_depth(
                        row.clone(),
                        resolved.depth,
                        matched_witness_seq,
                        dedup.artifact_hash.clone(),
                        dedup.store_paths.clone(),
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
                    let record = match append_context_witness(
                        &mut context,
                        WitnessBody {
                            task_uuid: task_uuid.map(|uuid| uuid.to_string()),
                            transition_timestamp: Utc::now()
                                .to_rfc3339_opts(SecondsFormat::Millis, true),
                            verdict: Verdict::Reused,
                            exit_code: 0,
                            artifact_content_hash: dedup.artifact_hash.clone(),
                            store_paths: dedup.store_paths.clone(),
                            drv: row.drv.clone(),
                            gpu_seconds: None,
                            wall_clock: 0.0,
                            attempt: row.attempt,
                            lease_epoch: row.lease_epoch,
                            dedup_key: row.dedup_key.clone(),
                            payload_hash: row.payload_hash.clone(),
                            brief_hash: row.brief_hash.clone(),
                            origin: row
                                .origin
                                .clone()
                                .expect("canonical row carries admission origin"),
                            orchestration: row.orchestration.clone(),
                            labor_class: LaborClass::Reused,
                            trace_ref: None,
                            pools: row.pools.clone(),
                            executor: row.executor.clone(),
                            host_id: None,
                            charge: None,
                            model: row.model.clone(),
                            evidence_class: row.evidence_class.clone(),
                            manifest_hash: row.manifest_hash.clone(),
                            completion: None,
                            result_revision: None,
                            authorship: None,
                            authorship_sessions: None,
                        },
                    ) {
                        Ok(record) => record,
                        Err(error) => return Err(self.fail_stop(error.into())),
                    };
                    let result = JobResult {
                        task_uuid: task_uuid.map(|uuid| uuid.to_string()),
                        job_id: job_id.to_string(),
                        verdict: Verdict::Reused,
                        exit_code: 0,
                        artifact_content_hash: dedup.artifact_hash.clone(),
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
                        "store_paths": dedup.store_paths,
                        "storePaths": dedup.store_paths,
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
            let current_attempt = current
                .map(|job| job.row.attempt)
                .or_else(|| context.rows.get(&job_id).map(|row| row.attempt));
            let resolved_attempt = match (requested_attempt, current_attempt) {
                (Some(requested), Some(current)) if requested < current => Some(current),
                (Some(requested), _) => Some(requested),
                (None, current) => current,
            };
            if current.is_some_and(|job| {
                job.state != JobState::Completed
                    && resolved_attempt.is_none_or(|attempt| job.row.attempt == attempt)
            }) {
                (Some(context.barriers.wait_job(&stable)), None)
            } else {
                (
                    None,
                    Some((context.paths.witness_path(), stable, resolved_attempt)),
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
            #[serde(default)]
            task_uuid: Option<String>,
            #[serde(default, alias = "flowRunId")]
            flow_run_id: Option<String>,
            #[serde(default)]
            force: bool,
        }
        let params: Params = decode_params(params)?;
        match (params.task_uuid, params.flow_run_id) {
            (Some(task_uuid), None) => self.cancel_one(&task_uuid, params.force).await,
            (None, Some(flow_run_id)) => self.cancel_flow(&flow_run_id).await,
            _ => Err(WireError::invalid(
                "provide exactly one of task_uuid or flow_run_id",
            )),
        }
    }

    async fn cancel_flow(&self, flow_run_id: &str) -> Result<Value, WireError> {
        Uuid::parse_str(flow_run_id)
            .map_err(|_| WireError::invalid("flow_run_id must be a UUID"))?;
        let mut task_uuids = {
            let context = self.context.read().await;
            context
                .jobs
                .values()
                .filter(|job| job.state != JobState::Completed)
                .filter(|job| {
                    job.row
                        .orchestration
                        .as_ref()
                        .is_some_and(|orchestration| orchestration.flow_run_id() == flow_run_id)
                })
                .map(Job::stable_key)
                .collect::<Vec<_>>()
        };
        task_uuids.sort();
        let mut affected = 0_u64;
        let mut results = Vec::with_capacity(task_uuids.len());
        for task_uuid in task_uuids {
            let result = self.cancel_one(&task_uuid, true).await?;
            affected = affected
                .saturating_add(result.get("affected").and_then(Value::as_u64).unwrap_or(0));
            results.push(result);
        }
        Ok(json!({
            "ok": true,
            "affected": affected,
            "flow_run_id": flow_run_id,
            "flowRunId": flow_run_id,
            "results": results,
        }))
    }

    async fn cancel_one(&self, task_uuid: &str, force: bool) -> Result<Value, WireError> {
        let mut context = self.context.write().await;
        let job = find_job(&context, task_uuid)?.clone();
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
        if job.state == JobState::Running && !force {
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
            &self.git_ai,
            self.exec_attestations,
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
                && !context
                    .query_rows
                    .get(&existing.uuid)
                    .is_some_and(|projection| projection.status == RowStatus::Deleted)
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
    git_ai_config: &GitAiConfig,
    exec_attestations: bool,
) -> Result<ExecutionRequest, ExecutorError> {
    let brief_path = job.row.brief_hash.as_deref().map(|hash| {
        brief::content_path(brief_root, hash)
            .expect("validated durable briefHash always derives a content path")
    });
    let gate_manifest = effective_gate_manifest(executor, job)?;
    let git_ai = git_ai_config.enable.then(|| {
        let mut attributes = BTreeMap::from([
            ("taskUuid".to_owned(), job.stable_key()),
            ("attempt".to_owned(), job.row.attempt.to_string()),
            ("leaseEpoch".to_owned(), job.row.lease_epoch.to_string()),
            ("adapter".to_owned(), job.row.adapter.clone()),
        ]);
        if let Some(orchestration) = &job.row.orchestration {
            attributes.insert(
                "flowRunId".to_owned(),
                orchestration.flow_run_id().to_owned(),
            );
            if let Some(node_ordinal) = orchestration
                .as_value()
                .get("nodeOrdinal")
                .and_then(Value::as_u64)
            {
                attributes.insert("nodeOrdinal".to_owned(), node_ordinal.to_string());
            }
        }
        GitAiExecution {
            config: git_ai_config.clone(),
            attributes,
            expected_session: job.row.session_ref.clone(),
            expected_model: canonical_job_model(job),
        }
    });
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
        git_ai,
        exec_attestation: exec_attestations.then(|| ExecAttestationContext {
            adapter: job.row.adapter.clone(),
            executor: job.row.executor.clone(),
            payload_hash: job.row.payload_hash.clone(),
            brief_hash: job.row.brief_hash.clone(),
            evidence: job.row.evidence.clone(),
        }),
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
        "store_paths": record.store_paths,
        "storePaths": record.store_paths,
        "drv": record.drv,
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

fn latest_witness_for_task(
    witness_path: &Path,
    task_uuid: Uuid,
) -> Result<Option<WitnessRecord>, WireError> {
    let (report, records) = read_verified_records(witness_path).map_err(internal_wire)?;
    if !report.ok {
        return Err(internal_wire(
            "witness verification failed while checking drv seed identity",
        ));
    }
    let task_uuid = task_uuid.to_string();
    Ok(records
        .into_iter()
        .filter(|record| record.task_uuid.as_deref() == Some(task_uuid.as_str()))
        .max_by_key(|record| record.seq))
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

fn log_gcroot_registration_failures(record: &WitnessRecord, paths: &DaemonPaths) {
    let report = register_record_roots(&paths.gcroots_dir(), record, &NixStore::default());
    for failure in report.failures {
        eprintln!(
            "tally: gcroot registration failed for witness {} path {} link {}: {}",
            record.seq,
            failure.target.display(),
            failure.link.display(),
            failure.reason
        );
    }
}

fn lock_gcroot_registration(paths: &DaemonPaths) -> Result<GcRootsLock, WitnessError> {
    acquire_registration_lock(&paths.gcroots_dir()).map_err(|source| WitnessError::Io {
        path: gcroots_lock_path(&paths.gcroots_dir()),
        source,
    })
}

fn append_daemon_witness(
    ledger: &mut WitnessLedger,
    paths: &DaemonPaths,
    body: WitnessBody,
) -> Result<WitnessRecord, WitnessError> {
    let _lock = lock_gcroot_registration(paths)?;
    let record = ledger.append(body)?;
    log_gcroot_registration_failures(&record, paths);
    Ok(record)
}

fn append_context_witness(
    context: &mut Context,
    body: WitnessBody,
) -> Result<WitnessRecord, WitnessError> {
    let _lock = lock_gcroot_registration(&context.paths)?;
    let record = context.witness.append(body)?;
    log_gcroot_registration_failures(&record, &context.paths);
    Ok(record)
}

fn forced_witness(job: &Job, verdict: Verdict, host_id: Option<String>) -> WitnessBody {
    WitnessBody {
        task_uuid: job.task_uuid.map(|uuid| uuid.to_string()),
        transition_timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        verdict,
        exit_code: if verdict == Verdict::Cancelled { 0 } else { 1 },
        artifact_content_hash: None,
        store_paths: None,
        drv: job.row.drv.clone(),
        gpu_seconds: None,
        wall_clock: 0.0,
        attempt: job.row.attempt,
        lease_epoch: job.row.lease_epoch,
        dedup_key: job.row.dedup_key.clone(),
        payload_hash: job.row.payload_hash.clone(),
        brief_hash: job.row.brief_hash.clone(),
        origin: job
            .row
            .origin
            .clone()
            .expect("canonical row carries admission origin"),
        orchestration: job.row.orchestration.clone(),
        labor_class: job.labor_class,
        trace_ref: None,
        pools: job.row.pools.clone(),
        executor: job.row.executor.clone(),
        host_id,
        charge: None,
        model: canonical_job_model(job),
        evidence_class: job.row.evidence_class.clone(),
        manifest_hash: job.row.manifest_hash.clone(),
        completion: None,
        result_revision: None,
        authorship: None,
        authorship_sessions: None,
    }
}

fn substituted_witness(row: &RowSeed, drv: Derivation) -> WitnessBody {
    WitnessBody {
        task_uuid: Some(row.uuid.to_string()),
        transition_timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        verdict: Verdict::Substituted,
        exit_code: 0,
        artifact_content_hash: None,
        store_paths: Some(drv.output_paths()),
        drv: Some(drv),
        gpu_seconds: None,
        wall_clock: 0.0,
        attempt: 1,
        lease_epoch: 1,
        dedup_key: row.dedup_key.clone(),
        payload_hash: row.payload_hash.clone(),
        brief_hash: row.brief_hash.clone(),
        origin: row
            .origin
            .clone()
            .expect("canonical row carries admission origin"),
        orchestration: row.orchestration.clone(),
        labor_class: LaborClass::Substituted,
        trace_ref: None,
        pools: row.pools.clone(),
        executor: row.executor.clone(),
        host_id: None,
        charge: None,
        model: row.model.clone(),
        evidence_class: row.evidence_class.clone(),
        manifest_hash: row.manifest_hash.clone(),
        completion: None,
        result_revision: None,
        authorship: None,
        authorship_sessions: None,
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
    let host_id = (job.state == JobState::Running && job.row.executor.is_none())
        .then(|| context.host_id.clone());
    let record = append_context_witness(context, forced_witness(&job, verdict, host_id))?;
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
        && completion.is_some_and(|completion| {
            completion.execution.status == ExecutionStatus::Failure
                || completion.gates.status == GateSummaryStatus::Fail
        })
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

struct DaemonLockGuard {
    file: File,
}

impl DaemonLockGuard {
    fn acquire(state_dir: &Path) -> Result<Self, DaemonError> {
        Ok(Self {
            file: acquire_daemon_lock(state_dir)?,
        })
    }

    fn file(&self) -> &File {
        &self.file
    }

    fn unlock(&self) -> io::Result<()> {
        FileExt::unlock(&self.file)
    }
}

impl Drop for DaemonLockGuard {
    fn drop(&mut self) {
        // flock follows the open-file description across fork. A concurrent
        // child can therefore retain the lock after this process closes its
        // descriptor but before exec applies CLOEXEC. Explicit unlock makes
        // every daemon lifetime release its fence immediately, including a
        // successful open that is dropped before entering the run loop.
        let _ = self.unlock();
    }
}

pub struct Daemon {
    _state_lock: DaemonLockGuard,
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
    #[cfg(test)]
    connection_count_hook: Option<mpsc::UnboundedSender<usize>>,
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
        let state_lock = DaemonLockGuard::acquire(&paths.state_dir)?;
        // Preserve the clean-cut refusal: predecessor bytes are never parsed.
        // Once the final ledger is confirmed, migrate under this lock before
        // any acknowledged-event reader or recovery reconciliation can run.
        require_fresh_events_for_new_ledger(&paths)?;
        let witness_path = paths.witness_path();
        let mut witness_ledger = WitnessLedger::open(&witness_path)?;
        migrate_acknowledged_events(&paths.events_dir())?;
        let host_id = current_host_id()?;
        let epoch = bump_epoch(&paths.state_dir)?;
        reconcile_pool_loss_intents(&paths, &executor, &mut witness_ledger, &host_id).await?;
        let mut durable = collect_durable_recovery_facts(&paths.events_dir(), &witness_path)?;
        if reconcile_reuse_witnesses(&paths, &durable, &mut witness_ledger)? {
            durable = collect_durable_recovery_facts(&paths.events_dir(), &witness_path)?;
        }
        {
            let _lock = lock_gcroot_registration(&paths)?;
            let horizon = parse_horizon(&config.retention.horizon)?;
            let (verification, records) = read_verified_records(&witness_path)?;
            if !verification.ok {
                return Err(DaemonError::Invalid(
                    "witness verification failed during GC-root reconciliation".to_owned(),
                ));
            }
            for (sequence, report) in reconcile_recent_roots(
                &paths.gcroots_dir(),
                &records,
                Utc::now(),
                horizon,
                &NixStore::default(),
            )? {
                for failure in report.failures {
                    eprintln!(
                        "tally: gcroot registration failed for witness {sequence} path {} link {}: {}",
                        failure.target.display(),
                        failure.link.display(),
                        failure.reason
                    );
                }
            }
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
        hydrate_completed_adapter_metadata(
            &mut plan,
            &config,
            &executor,
            &paths.attestations_path(),
        );
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
            config, paths, settings, executor, host_id, epoch, plan, committer, state_lock,
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
        let state_lock = DaemonLockGuard::acquire(&paths.state_dir)?;
        let host_id = current_host_id()?;
        Self::build_locked(
            config, paths, settings, executor, host_id, epoch, plan, committer, state_lock,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_locked(
        config: Config,
        paths: DaemonPaths,
        settings: DaemonSettings,
        executor: Executor,
        host_id: String,
        epoch: u64,
        plan: crate::recovery::RecoveryPlan,
        committer: Box<dyn ReplicaCommitter>,
        state_lock: DaemonLockGuard,
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
            host_id,
            epoch,
            lease: LocalLease::new(lease_engine, SystemdUnitLiveness::default()),
            guardrails: GuardrailState::new(GuardrailConfig {
                depth_cap: config.enqueue.depth_cap,
                fanout_cap: config.enqueue.fanout_cap,
                require_dedup_key: config.enqueue.require_dedup_key,
            })
            .map_err(|error| DaemonError::Invalid(error.message))?,
            witness: WitnessLedger::open(paths.witness_path())?,
            derivation_store: Arc::new(NixStore::default()),
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
            git_ai: config.git_ai.clone(),
            exec_attestations: config.attestations.exec.enable,
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
            #[cfg(test)]
            connection_count_hook: None,
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
        let worker_lock =
            self._state_lock.file().try_clone().map_err(|error| {
                DaemonError::Invalid(format!("cannot clone daemon lock: {error}"))
            })?;
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
        let mut connections = JoinSet::new();
        let max_connections = self.handler.settings.max_connections;
        #[cfg(test)]
        let connection_count_hook = self.connection_count_hook.clone();
        let mut result = if let Some(error) = startup_error {
            Err(error)
        } else {
            match self.notifier.ready() {
                Err(error) => Err(error),
                Ok(()) => loop {
                    tokio::select! {
                        accepted = self.listener.accept(), if connections.len() < max_connections => {
                            match accepted {
                                Ok((stream, _)) => {
                                    let handler = self.handler.clone();
                                    let max_frame_bytes = self.max_frame_bytes;
                                    connections.spawn_local(async move {
                                        if let Err(error) = serve_connection_with_limits(
                                            stream,
                                            handler,
                                            max_frame_bytes,
                                            Some(RPC_IDLE_TIMEOUT),
                                        )
                                        .await
                                        {
                                            eprintln!("tally: RPC connection failed: {error}");
                                        }
                                    });
                                    #[cfg(test)]
                                    if let Some(hook) = &connection_count_hook {
                                        let _ = hook.send(connections.len());
                                    }
                                }
                                Err(source) if retryable_accept_error(&source) => {
                                    eprintln!(
                                        "tally: RPC accept failed, retrying after {} ms: {source}",
                                        ACCEPT_ERROR_BACKOFF.as_millis()
                                    );
                                    tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                                }
                                Err(source) => break Err(io_error(&socket_path, source)),
                            }
                        }
                        Some(joined) = connections.join_next(), if !connections.is_empty() => {
                            if let Err(error) = joined {
                                eprintln!("tally: RPC connection task failed: {error}");
                            }
                            #[cfg(test)]
                            if let Some(hook) = &connection_count_hook {
                                let _ = hook.send(connections.len());
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
        connections.shutdown().await;
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
        if let Err(source) = self._state_lock.unlock() {
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
        let (result_revision, authorship, authorship_sessions) = match &finished.outcome {
            Some(Ok(outcome)) => (
                outcome.result_revision.clone(),
                outcome.authorship.clone(),
                outcome.authorship_sessions.clone(),
            ),
            _ => (None, None, None),
        };
        let execution_host_id = match &finished.outcome {
            Some(Ok(outcome)) => outcome.host_id.clone(),
            _ => None,
        };
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
                let reason = match error {
                    ExecutorError::GitAiRequired(reason) => reason.clone(),
                    _ => format!("executor failed: {error}"),
                };
                let execution = ExecutionFact::failed(reason);
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
        let (evidence_verdict, exit_code, artifact_hash, store_paths, evidence_checks) =
            match finished.outcome {
                None => {
                    return Err(DaemonError::Invalid(format!(
                        "job {} stopped without a terminal witness",
                        job.stable_key()
                    )))
                }
                Some(Ok(outcome)) => match outcome.termination {
                    ExecutionTermination::RuntimeExceeded => {
                        (Verdict::RuntimeExceeded, 1, None, None, Vec::new())
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
                        (
                            gate.verdict,
                            code,
                            gate.artifact_hash,
                            gate.store_paths,
                            gate.checks,
                        )
                    }
                    ExecutionTermination::Signaled { .. }
                    | ExecutionTermination::ServiceFailed { .. } => {
                        (Verdict::Failed, 1, None, None, Vec::new())
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
                    (Verdict::Failed, 1, None, None, Vec::new())
                }
            };
        let computed_verdict = canonical_verdict(evidence_verdict, semantic_completion.as_ref());

        let (result, evidence, launches, auto_requeue) = {
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
            let host_id = if job.row.executor.is_none() {
                Some(context.host_id.clone())
            } else {
                execution_host_id.clone()
            };
            let record = append_context_witness(
                &mut context,
                WitnessBody {
                    task_uuid: job.task_uuid.map(|uuid| uuid.to_string()),
                    transition_timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                    verdict,
                    exit_code,
                    artifact_content_hash: artifact_hash.clone(),
                    store_paths: store_paths.clone(),
                    drv: job.row.drv.clone(),
                    gpu_seconds: None,
                    wall_clock: finished.elapsed.as_secs_f64(),
                    attempt: job.row.attempt,
                    lease_epoch: job.row.lease_epoch,
                    dedup_key: job.row.dedup_key.clone(),
                    payload_hash: job.row.payload_hash.clone(),
                    brief_hash: job.row.brief_hash.clone(),
                    origin: job
                        .row
                        .origin
                        .clone()
                        .expect("canonical row carries admission origin"),
                    orchestration: job.row.orchestration.clone(),
                    labor_class: job.labor_class,
                    trace_ref: None,
                    pools: job.row.pools.clone(),
                    executor: job.row.executor.clone(),
                    host_id,
                    charge: None,
                    model: model.clone(),
                    evidence_class: job.row.evidence_class.clone(),
                    manifest_hash: job.row.manifest_hash.clone(),
                    completion: semantic_completion.clone(),
                    result_revision: result_revision.clone(),
                    authorship: authorship.clone(),
                    authorship_sessions: authorship_sessions.clone(),
                },
            )?;
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
            let auto_requeue = verdict == Verdict::RuntimeExceeded
                && self
                    .handler
                    .settings
                    .recovery_policy
                    .retry
                    .auto_bounded_requeue
                && job.row.attempt < self.handler.settings.recovery_policy.max_attempts;
            if !auto_requeue {
                context.barriers.complete_job(&stable, result.value());
            }
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
            (result, evidence, launches, auto_requeue)
        };

        // Ordinary waiters become runnable immediately after the only terminal ack
        // dependency: the witness fsync above. An automatic bounded requeue holds
        // the same stable waiter until the replacement attempt is terminal. Lease
        // release, scrape, attestations, replica commit, and journald are post-ack.
        tokio::task::yield_now().await;
        let stable = job.stable_key();
        let terminal_value = result.value();
        self.handler.complete_terminal_post_ack(TerminalWork {
            job,
            result,
            evidence,
            evidence_checks,
            launches,
            scrape_capture,
        });
        if auto_requeue {
            if let Err(error) = self
                .handler
                .retry_job(Some(json!({"task_uuid": stable})))
                .await
            {
                self.handler
                    .context
                    .write()
                    .await
                    .barriers
                    .complete_job(&stable, terminal_value);
                return Err(DaemonError::Invalid(format!(
                    "automatic bounded requeue for job {stable} failed: {}",
                    error.message
                )));
            }
        }
        Ok(())
    }

    async fn tick_leases(handler: DaemonHandler) -> Result<(), DaemonError> {
        Self::tick_leases_at(handler, Utc::now()).await
    }

    async fn tick_leases_at(
        handler: DaemonHandler,
        now: chrono::DateTime<Utc>,
    ) -> Result<(), DaemonError> {
        let mut context = handler.context.write().await;
        let mut launches = retry_unleased_jobs(&mut context, &handler.executor);
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
            .commit_preemptions(&reclaimed, now)?;
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
    host_id: &str,
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
        let execution_host_id = job.row.executor.is_none().then(|| host_id.to_owned());
        records.push(append_daemon_witness(
            ledger,
            paths,
            forced_witness(&job, Verdict::PoolVanished, execution_host_id),
        )?);
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

fn require_fresh_events_for_new_ledger(paths: &DaemonPaths) -> Result<(), DaemonError> {
    // The final-schema daemon creates this file and fsyncs its parent before it
    // can admit an event, so its existence is the durable cutover marker.
    if paths.witness_path().exists() {
        return Ok(());
    }
    let events_dir = paths.events_dir();
    if !events_dir.exists() {
        return Ok(());
    }
    let mut entries =
        std::fs::read_dir(&events_dir).map_err(|source| io_error(&events_dir, source))?;
    if entries
        .next()
        .transpose()
        .map_err(|source| io_error(&events_dir, source))?
        .is_none()
    {
        return Ok(());
    }
    Err(DaemonError::OldFormatEvents {
        path: events_dir.clone(),
        archive: PathBuf::from(format!(
            "{}.pre-{}",
            events_dir.display(),
            Utc::now().format("%Y-%m-%d")
        )),
    })
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
    paths: &DaemonPaths,
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
            || matched.artifact_content_hash != reuse.artifact_content_hash
            || matched.store_paths != reuse.store_paths
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
                append_daemon_witness(
                    ledger,
                    paths,
                    WitnessBody {
                        task_uuid: Some(task_uuid),
                        transition_timestamp: Utc::now()
                            .to_rfc3339_opts(SecondsFormat::Millis, true),
                        verdict: Verdict::Reused,
                        exit_code: 0,
                        artifact_content_hash: reuse.artifact_content_hash.clone(),
                        store_paths: reuse.store_paths.clone(),
                        drv: event.row.drv.clone(),
                        gpu_seconds: None,
                        wall_clock: 0.0,
                        attempt: event.row.attempt,
                        lease_epoch: event.row.lease_epoch,
                        dedup_key: event.row.dedup_key.clone(),
                        payload_hash: event.row.payload_hash.clone(),
                        brief_hash: event.row.brief_hash.clone(),
                        origin: event
                            .row
                            .origin
                            .clone()
                            .expect("canonical row carries admission origin"),
                        orchestration: event.row.orchestration.clone(),
                        labor_class: LaborClass::Reused,
                        trace_ref: None,
                        pools: event.row.pools.clone(),
                        executor: event.row.executor.clone(),
                        host_id: None,
                        charge: None,
                        model: event.row.model.clone(),
                        evidence_class: event.row.evidence_class.clone(),
                        manifest_hash: event.row.manifest_hash.clone(),
                        completion: None,
                        result_revision: None,
                        authorship: None,
                        authorship_sessions: None,
                    },
                )?;
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
        && record.artifact_content_hash == reuse.artifact_content_hash
        && record.store_paths == reuse.store_paths
        && record.drv == event.row.drv
        && record.attempt == event.row.attempt
        && record.lease_epoch == event.row.lease_epoch
        && record.dedup_key == event.row.dedup_key
        && record.payload_hash == event.row.payload_hash
        && record.brief_hash == event.row.brief_hash
        && record.orchestration == event.row.orchestration
        && record.labor_class == LaborClass::Reused
        && record.pools == event.row.pools
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
    attestation_path: &Path,
) {
    let engine = AdapterEngine::new(&config.adapters);
    for recovery in &mut plan.rows {
        if !matches!(
            recovery.state,
            RecoveryRowState::Completed | RecoveryRowState::Deleted
        ) {
            continue;
        }
        match verified_adapter_attestation_captures(
            attestation_path,
            recovery.row.uuid,
            &recovery.row.adapter,
            recovery.row.attempt,
            recovery.row.lease_epoch,
        ) {
            Ok(Some(captures)) => {
                apply_adapter_metadata(&mut recovery.row, &captures);
                continue;
            }
            Ok(None) => {}
            Err(error) => eprintln!(
                "tally: retained adapter attestation for {} could not be read: {error}",
                recovery.row.uuid
            ),
        }
        if config
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
            Ok(captures) => apply_adapter_metadata(&mut recovery.row, &captures),
            Err(error) => eprintln!(
                "tally: retained adapter metadata for {} could not be scraped: {error}",
                recovery.row.uuid
            ),
        }
    }
}

fn apply_adapter_metadata(row: &mut RowSeed, captures: &ScrapeResult) {
    if let Ok(Some(session_ref)) = captures.session_ref() {
        row.session_ref = Some(session_ref.to_owned());
    }
    if let Ok(Some(model)) = captures.model() {
        row.model = Some(model.to_owned());
    }
    if let Ok(Some(final_message)) = captures.final_message() {
        row.final_message = Some(final_message.to_owned());
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

#[cfg(test)]
include!("tests.rs");
