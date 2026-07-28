mod barriers;
mod notify;
mod replica;
mod rpc;
mod run;
mod startup;
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
use rpc::query::{feed_scraped_usage, query_row};
#[cfg(test)]
use rpc::query::{
    overlay_live_states, read_usage_meter, usage_meter_event_path, write_usage_meter,
    UsageMeterObservation,
};
#[cfg(test)]
use run::LeaseTickHook;
use run::{
    merge_selected_pool_returns, pool_representations, promoted_jobs, renderable_pool_return_rows,
    resume_paused_jobs_locked,
};
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
use tokio::task::{JoinHandle, LocalSet};

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

fn lease_wire(error: LeaseError) -> WireError {
    match error {
        LeaseError::UnknownPool(_)
        | LeaseError::InvalidRequest(_)
        | LeaseError::StaleEpoch { .. } => WireError::invalid(error.to_string()),
        LeaseError::NotFound(_) => WireError::not_found(error.to_string()),
        other => internal_wire(other),
    }
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

#[cfg(test)]
include!("tests.rs");
