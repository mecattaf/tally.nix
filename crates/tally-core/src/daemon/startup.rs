//! Rebuild derived daemon state from canonical stores.
//!
//! The failure-stderr file and its high-water cursor are the one persisted
//! projection owned here; both are written only by replaying terminal witness
//! records over canonical captures.

use super::*;

pub(super) struct DaemonLockGuard {
    file: File,
}

impl DaemonLockGuard {
    pub(super) fn acquire(state_dir: &Path) -> Result<Self, DaemonError> {
        Ok(Self {
            file: acquire_daemon_lock(state_dir)?,
        })
    }

    #[cfg(test)]
    pub(super) fn file(&self) -> &File {
        &self.file
    }

    pub(super) fn unlock(&self) -> io::Result<()> {
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

/// How long any single startup phase may take before systemd gives up.
///
/// Deliberately the same 90 s the user manager's `DefaultTimeoutStartSec`
/// imposes on the whole of startup, so the number an operator sees in a failure
/// is the one they are used to. It measures one phase rather than all of them.
pub(super) const STARTUP_PHASE_BUDGET: Duration = Duration::from_secs(90);

/// How often the unit-facts loop renews the start timeout while it is making
/// progress.
///
/// The phase-boundary extension gives each phase one fresh budget, but the
/// unit-facts phase probes the executor once per non-terminal durable row —
/// O(event corpus) inside a single budget, ~90–95 s at the coordinator's ~25k
/// rows, so whether a restart succeeded was a coin flip on load. Renewing from
/// inside the loop makes the budget bound progress stalls: a daemon still
/// visiting rows keeps starting, one wedged on a single probe dies on the same
/// 90 s clock. The interval is a small fraction of the budget so a
/// slow-but-moving loop never comes near the deadline, while a fast startup
/// (under 10 s of unit facts) sends nothing extra.
pub(super) const UNIT_FACTS_EXTEND_INTERVAL: Duration = Duration::from_secs(10);

/// Turns the unit-facts loop's per-row progress callbacks into throttled
/// `EXTEND_TIMEOUT_USEC=` notifications.
pub(super) struct UnitFactsExtension {
    notifier: SystemdNotifier,
    interval: Duration,
    last: std::time::Instant,
    visited: usize,
    total: usize,
}

impl UnitFactsExtension {
    pub(super) fn new(notifier: SystemdNotifier, interval: Duration, total: usize) -> Self {
        Self {
            notifier,
            interval,
            // The phase-boundary extension for `unit-facts` was just sent, so
            // the first in-loop renewal is owed one interval from now, not
            // immediately.
            last: std::time::Instant::now(),
            visited: 0,
            total,
        }
    }

    /// Record one visited row; renew the start timeout if an interval passed.
    ///
    /// "Visited", not "probed": canonically terminal remote rows are counted
    /// too, because the loop advanced past them — the status line claims
    /// progress through the corpus, not a probe per row. A notification
    /// failure is not fatal here for the same reason it is not fatal at phase
    /// boundaries: `ready()` still reports a broken notify socket.
    pub(super) fn row_visited(&mut self) {
        self.visited += 1;
        if self.last.elapsed() < self.interval {
            return;
        }
        let _ = self.notifier.extend_start_timeout(
            STARTUP_PHASE_BUDGET,
            &format!(
                "starting: unit-facts ({}/{} rows)",
                self.visited, self.total
            ),
        );
        self.last = std::time::Instant::now();
    }
}

/// Per-phase startup accounting, and the systemd budget that pays for it.
///
/// Everything `Daemon::open` and the pre-`READY` half of `run_loop` do is
/// charged to `TimeoutStartSec` and never to `WatchdogSec`, because the service
/// watchdog is not armed until `READY=1`. At estate scale that is most of the
/// budget — measured at 61 s of 90 s on the coordinator — and none of it can be
/// attributed, because the daemon says nothing at all between `Starting` and
/// its first late-startup log line. This type is the answer to both halves of
/// that.
///
/// It tells systemd, at every phase boundary, that startup is still making
/// progress, via `EXTEND_TIMEOUT_USEC=`. A growing estate no longer walks into
/// a restart loop the operator only diagnoses afterwards; a phase that wedges
/// still fails, on the same clock, with `STATUS=` naming which one.
///
/// It tells the operator, in one line at `READY`, what each phase cost — the
/// durable artefact a later lane that adds startup work checks its own number
/// against, instead of a silent minute.
pub(super) struct StartupTimeline {
    notifier: SystemdNotifier,
    started: std::time::Instant,
    phase_started: std::time::Instant,
    current: &'static str,
    phases: Vec<(&'static str, Duration)>,
}

impl StartupTimeline {
    pub(super) fn begin(notifier: SystemdNotifier, first: &'static str) -> Self {
        let now = std::time::Instant::now();
        let timeline = Self {
            notifier,
            started: now,
            phase_started: now,
            current: first,
            phases: Vec::new(),
        };
        timeline.extend(first);
        timeline
    }

    /// Close the running phase and open `next`.
    pub(super) fn phase(&mut self, next: &'static str) {
        let now = std::time::Instant::now();
        self.phases
            .push((self.current, now.duration_since(self.phase_started)));
        self.current = next;
        self.phase_started = now;
        self.extend(next);
    }

    /// Close the last phase and render the report line for `READY`.
    pub(super) fn finish(mut self) -> String {
        let now = std::time::Instant::now();
        self.phases
            .push((self.current, now.duration_since(self.phase_started)));
        let total = now.duration_since(self.started);
        let mut line = format!(
            "startup complete in {:.3}s of a {}s per-phase budget",
            total.as_secs_f64(),
            STARTUP_PHASE_BUDGET.as_secs()
        );
        // Field-per-phase rather than prose: an operator watching the trend
        // needs to grep one phase out of the line, not read it.
        for (name, elapsed) in &self.phases {
            line.push_str(&format!(" {name}={:.3}s", elapsed.as_secs_f64()));
        }
        line
    }

    /// Every phase opened so far, the running one last.
    #[cfg(test)]
    pub(super) fn phase_names(&self) -> Vec<&'static str> {
        self.phases
            .iter()
            .map(|(name, _)| *name)
            .chain(std::iter::once(self.current))
            .collect()
    }

    fn extend(&self, phase: &str) {
        // A notification failure is not made fatal here. A broken notify socket
        // must not be able to fail startup any earlier than `ready()` does,
        // which would trade a measured budget problem for an unmeasured
        // availability one. `ready()` still reports.
        let _ = self
            .notifier
            .extend_start_timeout(STARTUP_PHASE_BUDGET, &format!("starting: {phase}"));
    }
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
        // The notifier is built first so every phase below is inside the
        // extended budget, not just the ones after the listener exists.
        let notifier = SystemdNotifier::from_environment()?;
        let mut timeline = StartupTimeline::begin(notifier.clone(), "prepare");
        config
            .validate()
            .map_err(|error| DaemonError::Invalid(error.to_string()))?;
        let executor = executor.with_remote_executors(config.executors.clone());
        let settings = settings.validate()?;
        prepare_paths(&paths)?;
        let state_lock = DaemonLockGuard::acquire(&paths.state_dir)?;
        timeline.phase("row-migration");
        // Preserve the clean-cut refusal: predecessor bytes are never parsed.
        // Once the final ledger is confirmed, migrate under this lock before
        // any acknowledged-event reader or recovery reconciliation can run.
        require_fresh_events_for_new_ledger(&paths)?;
        let witness_path = paths.witness_path();
        let mut witness_ledger = WitnessLedger::open(&witness_path)?;
        migrate_acknowledged_events(&paths.events_dir())?;
        let host_id = current_host_id()?;
        let epoch = bump_epoch(&paths.state_dir)?;
        timeline.phase("durable-facts");
        let mut durable = collect_durable_recovery_facts(&paths.events_dir(), &witness_path)?;
        if reconcile_reuse_witnesses(&paths, &durable, &mut witness_ledger)? {
            durable = collect_durable_recovery_facts(&paths.events_dir(), &witness_path)?;
        }
        timeline.phase("gcroots");
        {
            let _lock = lock_gcroot_registration(&paths)?;
            let horizon = parse_horizon(&config.retention.horizon)?;
            for (sequence, report) in reconcile_recent_roots(
                &paths.gcroots_dir(),
                durable.witness(),
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
        timeline.phase("unit-facts");
        let mut extension = UnitFactsExtension::new(
            notifier.clone(),
            UNIT_FACTS_EXTEND_INTERVAL,
            durable.events().len(),
        );
        let units =
            collect_local_unit_facts(&executor, &durable, || extension.row_visited()).await?;
        timeline.phase("recovery-plan");
        let facts = RecoveryFacts {
            durable,
            current_lease_epoch: epoch,
            units,
            rowless_units: BTreeMap::new(),
            triggers: RecoveryTriggers {
                confirmed_pool_returns: BTreeSet::new(),
                resource_returns: BTreeSet::new(),
                bounded_requeues: BTreeSet::new(),
            },
            advisory_return_attestations: Vec::new(),
        };
        let mut attestations = SharedAttestations::new(paths.attestations_path());
        if let Err(error) = attestations.ledger() {
            eprintln!("tally: advisory attestation ledger could not be opened: {error}");
        }
        let mut plan = recover(&facts, settings.recovery_policy)?;
        reconcile_retained_adapter_attestations(
            &plan,
            facts.durable.witness(),
            &config,
            &executor,
            &mut attestations,
        );
        hydrate_completed_adapter_metadata(&mut plan, &config, &executor, &mut attestations);
        hydrate_adopted_adapter_metadata(&mut plan, &mut attestations)?;
        hydrate_represent_adapter_metadata(&mut plan, &config, &executor, &mut attestations)?;

        timeline.phase("storage");
        let storage_data_dir = paths.data_dir.clone();
        let storage_state_dir = paths.state_dir.clone();
        let storage_config = config.storage.clone();
        let storage_completion_count = witness_ledger.head().seq;
        let storage = tokio::task::spawn_blocking(move || {
            StorageMonitor::open(
                storage_data_dir,
                storage_state_dir,
                storage_config,
                storage_completion_count,
            )
        })
        .await
        .map_err(|error| {
            DaemonError::Invalid(format!("initial storage sampler worker failed: {error}"))
        })?;
        let daemon = Self::build_locked(
            config,
            paths,
            settings,
            executor,
            host_id,
            epoch,
            plan,
            state_lock,
            storage,
            attestations,
            witness_ledger,
            facts.durable.witness().to_vec(),
            notifier,
            timeline,
        )?;
        daemon.handler.refresh_storage_now().await;
        Ok(daemon)
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
        state_lock: DaemonLockGuard,
        mut storage: StorageMonitor,
        mut attestations: SharedAttestations,
        witness_ledger: WitnessLedger,
        witness_records: Vec<crate::witness::WitnessRecord>,
        notifier: SystemdNotifier,
        mut timeline: StartupTimeline,
    ) -> Result<Self, DaemonError> {
        timeline.phase("lease-engine");
        validate_recovery_briefs(&plan, &paths.data_dir)?;
        let event_log = LeaseEventLog::in_state_dir(&paths.state_dir);
        let completed_witness = witness_records;
        let lease_engine = LeaseEngine::from_durable_with_aging_threshold(
            epoch,
            settings.yield_grace,
            Duration::from_secs(config.aging_threshold_sec),
            config.pools.clone(),
            event_log,
            &completed_witness,
            Utc::now(),
        )?;
        timeline.phase("failure-stderr");
        reconcile_failure_stderr(&completed_witness, &executor, &paths.state_dir)?;
        timeline.phase("install-jobs");
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
            witness: witness_ledger,
            witness_view: WitnessView::from_records(
                paths.witness_path(),
                completed_witness.clone(),
            ),
            derivation_store: Arc::new(NixStore::default()),
            jobs: HashMap::new(),
            aliases: HashMap::new(),
            lease_jobs: HashMap::new(),
            paused_pools: HashSet::new(),
            barriers: BarrierTracker::with_namespace(epoch),
            rows,
            guardrail_depths,
            query_rows: query_rows.into(),
            query_details: query_details.into(),
        };
        restore_completed_aliases(&mut context, &completed_witness)?;
        let initial_jobs =
            install_recovery_jobs(&mut context, &plan, &executor, &mut attestations)?;
        restore_guardrail_parents(&mut context, &plan)?;
        let job_tokens = restore_job_tokens(&context)?;

        if paths.socket.exists() {
            std::fs::remove_file(&paths.socket)
                .map_err(|source| io_error(&paths.socket, source))?;
        }
        let listener =
            UnixListener::bind(&paths.socket).map_err(|source| io_error(&paths.socket, source))?;
        let (completion_tx, completion_rx) = mpsc::unbounded_channel();
        let (fatal_tx, fatal_rx) = mpsc::unbounded_channel();
        let (execution_shutdown, execution_shutdown_rx) = watch::channel(false);
        let (execution_cancel, _) = broadcast::channel(64);
        let post_ack_tasks = Rc::new(RefCell::new(Vec::new()));
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
        for notice in storage.take_notices() {
            eprintln!("tally: {notice}");
        }
        if let Some(error) = storage.query_snapshot().monitor_error {
            eprintln!(
                "tally: initial storage monitor sample failed; new intake will be refused: {error}"
            );
        }
        for warning in storage.take_warnings() {
            log_storage_warning(&warning);
        }
        let handler = DaemonHandler {
            context: Rc::new(RwLock::new(context)),
            job_tokens: Rc::new(RefCell::new(job_tokens)),
            settings,
            executor,
            completion: completion_tx,
            journal: JournalEmitter::from_config(&config.journald),
            history: Rc::new(RefCell::new(LifecycleStore::open(&paths.data_dir)?)),
            changes: Rc::new(RefCell::new(changes)),
            storage: Rc::new(RefCell::new(StorageRuntime::new(storage))),
            storage_refresh: Rc::new(Mutex::new(())),
            trace_adapters: Rc::new(trace_adapters),
            pages: Rc::new(RefCell::new(PageCache::default())),
            execution_shutdown: execution_shutdown_rx,
            execution_cancel,
            fatal: fatal_tx,
            post_ack_tasks,
            settling_completions: Rc::new(RefCell::new(HashMap::new())),
            ingress_sweep: Rc::new(Mutex::new(())),
            tally_socket,
            brief_root: paths.data_dir.clone(),
            exec_attestations: config.attestations.exec.enable,
            attestations: Arc::new(std::sync::Mutex::new(attestations)),
            flow_lineage_cache: Rc::new(RefCell::new(None)),
            flow_membership_cache: Rc::new(RefCell::new(None)),
        };
        Ok(Self {
            _state_lock: state_lock,
            listener,
            handler,
            completion_rx,
            fatal_rx,
            notifier,
            startup: Some(timeline),
            initial_jobs,
            execution_shutdown,
            max_frame_bytes: config.max_frame_bytes,
            #[cfg(test)]
            lease_tick_hook: None,
            #[cfg(test)]
            connection_count_hook: None,
            #[cfg(test)]
            dispatch_stall_hook: None,
            #[cfg(test)]
            finish_job_hook: None,
            #[cfg(test)]
            startup_report_hook: None,
        })
    }
}

pub(super) fn prepare_paths(paths: &DaemonPaths) -> Result<(), DaemonError> {
    for path in [&paths.state_dir, &paths.data_dir] {
        if !path.is_absolute() {
            return Err(DaemonError::Invalid(format!(
                "daemon path must be absolute: {}",
                path.display()
            )));
        }
    }
    prepare_state_directory(&paths.state_dir)?;
    std::fs::create_dir_all(&paths.data_dir).map_err(|source| io_error(&paths.data_dir, source))?;
    let socket_parent = paths
        .socket
        .parent()
        .ok_or_else(|| DaemonError::Invalid("socket has no parent directory".to_owned()))?;
    std::fs::create_dir_all(socket_parent).map_err(|source| io_error(socket_parent, source))?;
    Ok(())
}

fn prepare_state_directory(path: &Path) -> Result<(), DaemonError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => return Ok(()),
        Ok(_) => {
            return Err(DaemonError::InvalidStateDirectory {
                path: path.to_owned(),
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(io_error(path, source)),
    }

    if let Err(source) = std::fs::create_dir_all(path) {
        if std::fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_dir()) {
            return Err(DaemonError::InvalidStateDirectory {
                path: path.to_owned(),
            });
        }
        return Err(io_error(path, source));
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_dir() {
        return Err(DaemonError::InvalidStateDirectory {
            path: path.to_owned(),
        });
    }
    Ok(())
}

pub(super) fn require_fresh_events_for_new_ledger(paths: &DaemonPaths) -> Result<(), DaemonError> {
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

pub(super) fn acquire_daemon_lock(state_dir: &Path) -> Result<File, DaemonError> {
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

/// Where the failure-stderr recovery pass records how far it has got.
pub(super) const FAILURE_STDERR_CURSOR_FILE: &str = "failure-stderr-cursor.json";

/// The high-water mark of the failure-stderr recovery pass.
///
/// Recovery of one terminal record is a one-shot: the capture it reads is final
/// by the time the record is terminal, so a second attempt on the same record
/// can only reach the same answer. Persisting how far the pass got is what
/// makes that true across restarts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FailureStderrCursor {
    schema_version: u32,
    /// Every terminal record at or below this witness sequence has had its
    /// failure-stderr recovery attempted and reached a definitive answer.
    reconciled_through_seq: u64,
}

/// What one record's recovery attempt settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureStderrOutcome {
    /// The projection exists now, or the generation says this attempt's
    /// captures are not the current ones. Either way, settled.
    Settled,
    /// The capture stream this record names does not exist and never will.
    /// The generation marker was written and fsynced before `systemd-run`
    /// created the unit, and `archive_current_capture` returns early when any
    /// of the capture set is missing — so an attempt that failed before its
    /// stderr stream existed leaves a generation marker that is never retired,
    /// and this pass read it as "recoverable" at every startup, forever.
    SourceAbsent,
    /// Something else went wrong: a contended capture lock, a permission
    /// failure, a malformed generation. That can succeed later, so the cursor
    /// does not advance past it.
    Deferred,
}

/// Attempt failure-stderr recovery for every terminal record not yet
/// reconciled.
///
/// Walking every terminal `Failed` record at every startup means hundreds of
/// probes per start, on an estate with real failure history, whose answer
/// cannot change — each one printing its own warning line, enough noise to bury
/// the genuine startup signal beside it, all of it charged to
/// `TimeoutStartSec`.
///
/// The cost is one-shot instead. The cursor advances only across a contiguous
/// run of definitive outcomes, so a transient failure — a lock this daemon lost
/// a race for — is retried on the next start rather than silently abandoned,
/// and the pass reports itself in one line with named fields instead of one
/// line per doomed probe.
pub(super) fn reconcile_failure_stderr(
    records: &[crate::witness::WitnessRecord],
    executor: &Executor,
    state_dir: &Path,
) -> Result<(), DaemonError> {
    let cursor_path = state_dir.join(FAILURE_STDERR_CURSOR_FILE);
    let previous = read_failure_stderr_cursor(&cursor_path)?;
    let mut examined = 0_usize;
    let mut recovered = 0_usize;
    let mut absent = 0_usize;
    let mut deferred = 0_usize;
    // Only advanced while every record so far settled: the cursor is a
    // contiguous high-water mark, not a maximum.
    let mut contiguous = previous;
    let mut deferred_seen = false;
    for record in records {
        if record.seq <= previous {
            continue;
        }
        if terminal_lifecycle_event(record.verdict, record.artifact_content_hash.is_some())
            != TallyEvent::Failed
        {
            if !deferred_seen {
                contiguous = contiguous.max(record.seq);
            }
            continue;
        }
        let Some(task_uuid) = record.task_uuid.as_deref() else {
            if !deferred_seen {
                contiguous = contiguous.max(record.seq);
            }
            continue;
        };
        let task_uuid = Uuid::parse_str(task_uuid).map_err(|_| {
            DaemonError::Invalid(format!(
                "terminal witness {} has invalid task UUID {task_uuid:?}",
                record.seq
            ))
        })?;
        let identity = ExecutionIdentity {
            job_id: task_uuid,
            task_uuid: Some(task_uuid),
            task_ref: record
                .orchestration
                .as_ref()
                .and_then(Orchestration::task_ref),
        };
        examined += 1;
        let outcome =
            match executor.persist_failure_stderr(&identity, record.attempt, record.lease_epoch) {
                Ok(Some(_)) => {
                    recovered += 1;
                    FailureStderrOutcome::Settled
                }
                Ok(None) => FailureStderrOutcome::Settled,
                Err(ExecutorError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    absent += 1;
                    FailureStderrOutcome::SourceAbsent
                }
                Err(error) => {
                    deferred += 1;
                    // Kept per-record, unlike the absent case: this one is both
                    // rare and actionable, and it is the only class a later
                    // start can still repair.
                    eprintln!(
                        "tally: failure-stderr recovery deferred for {task_uuid} \
                         attempt={} leaseEpoch={} seq={}: {error}",
                        record.attempt, record.lease_epoch, record.seq
                    );
                    FailureStderrOutcome::Deferred
                }
            };
        match outcome {
            FailureStderrOutcome::Deferred => deferred_seen = true,
            _ if !deferred_seen => contiguous = contiguous.max(record.seq),
            _ => {}
        }
    }
    if examined > 0 {
        // One line with named fields, not one line per probe. `absent` is
        // non-zero only for records this pass is seeing for the first time,
        // because it never sees them again.
        eprintln!(
            "tally: failure-stderr recovery examined={examined} recovered={recovered} \
             absent={absent} deferred={deferred} reconciledThroughSeq={contiguous}"
        );
    }
    if contiguous > previous {
        write_failure_stderr_cursor(&cursor_path, contiguous)?;
    }
    Ok(())
}

fn read_failure_stderr_cursor(path: &Path) -> Result<u64, DaemonError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => return Err(io_error(path, source)),
    };
    // A cursor this daemon cannot parse is treated as absent rather than
    // fatal: the worst outcome is one more full pass, which is exactly the
    // behaviour that preceded this file.
    match serde_json::from_slice::<FailureStderrCursor>(&bytes) {
        Ok(cursor) if cursor.schema_version == 1 => Ok(cursor.reconciled_through_seq),
        _ => {
            eprintln!(
                "tally: failure-stderr recovery cursor at {} is unreadable; reconciling from the \
                 start of the ledger",
                path.display()
            );
            Ok(0)
        }
    }
}

fn write_failure_stderr_cursor(path: &Path, through_seq: u64) -> Result<(), DaemonError> {
    let bytes = serde_json::to_vec(&FailureStderrCursor {
        schema_version: 1,
        reconciled_through_seq: through_seq,
    })
    .map_err(|error| DaemonError::Invalid(error.to_string()))?;
    crate::executor::replace_private_file(path, &bytes)
        .map_err(|error| DaemonError::Invalid(error.to_string()))
}

pub(super) fn reconcile_reuse_witnesses(
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
                        error: None,
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

pub(super) fn reuse_record_matches(
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

pub(super) fn recovery_query_rows(plan: &crate::recovery::RecoveryPlan) -> BTreeMap<Uuid, RowFact> {
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

pub(super) fn recovery_query_details(
    plan: &crate::recovery::RecoveryPlan,
) -> BTreeMap<Uuid, RowDetailFact> {
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

pub(super) fn hydrate_completed_adapter_metadata(
    plan: &mut crate::recovery::RecoveryPlan,
    config: &Config,
    executor: &Executor,
    attestations: &mut SharedAttestations,
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
            attestations,
            recovery.row.uuid,
            &recovery.row.adapter,
            recovery.row.attempt,
            recovery.row.lease_epoch,
        ) {
            Ok(Some(captures)) => {
                let adapter = config.adapters.get(&recovery.row.adapter).cloned();
                apply_adapter_metadata(&mut recovery.row, &captures, adapter.as_ref());
                match verified_adapter_attestation_usage(
                    attestations,
                    recovery.row.uuid,
                    &recovery.row.adapter,
                    recovery.row.attempt,
                    recovery.row.lease_epoch,
                ) {
                    Ok(Some((observed, accounting))) => {
                        recovery.row.usage = Some(observed);
                        recovery.row.usage_accounting = accounting;
                    }
                    Ok(None) => {}
                    Err(error) => eprintln!(
                        "tally: retained adapter usage for {} could not be read: {error}",
                        recovery.row.uuid
                    ),
                }
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
            task_ref: recovery
                .row
                .orchestration
                .as_ref()
                .and_then(Orchestration::task_ref),
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
                let adapter = config.adapters.get(&recovery.row.adapter).cloned();
                apply_adapter_metadata(&mut recovery.row, &captures, adapter.as_ref());
            }
            Err(error) => eprintln!(
                "tally: retained adapter metadata for {} could not be scraped: {error}",
                recovery.row.uuid
            ),
        }
    }
}

/// Re-apply the advisory metadata a retained capture or attestation carries
/// onto a recovered row.
///
/// The usage record is recomputed from the captures against the adapter's
/// current declared mapping rather than copied: the ledger holds the record as
/// it was normalized when the attempt ran, and this row is the live view. A
/// row whose adapter is gone from the configuration gets no usage statement at
/// all, because nothing is left to declare one.
pub(super) fn apply_adapter_metadata(
    row: &mut RowSeed,
    captures: &ScrapeResult,
    adapter: Option<&AdapterConfig>,
) {
    if let Some(adapter) = adapter {
        row.usage = Some(crate::usage::observe(adapter, captures));
        row.usage_accounting = None;
    }
    if let Ok(Some(session_ref)) = captures.session_ref() {
        row.session_ref = Some(session_ref.to_owned());
        row.record_session_launch_cwd();
    }
    if let Ok(Some(model)) = captures.model() {
        row.model = Some(model.to_owned());
    }
    if let Ok(Some(final_message)) = captures.final_message() {
        row.final_message = Some(final_message.to_owned());
    }
}

pub(super) fn hydrate_represent_adapter_metadata(
    plan: &mut crate::recovery::RecoveryPlan,
    config: &Config,
    executor: &Executor,
    attestations: &mut SharedAttestations,
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
                recovery_adapter_invocation(config, action, row, executor, attestations)?;
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
            recovery.row.record_session_launch_cwd();
        }
        if model.is_some() {
            recovery.row.model = model;
        }
        if final_message.is_some() {
            recovery.row.final_message = final_message;
        }
        // No usage statement here. These captures belong to the session being
        // resumed, not to the attempt this row is about to run; attributing
        // them to it would report a previous attempt's tokens as this one's.
    }
    Ok(())
}

pub(super) fn hydrate_adopted_adapter_metadata(
    plan: &mut crate::recovery::RecoveryPlan,
    attestations: &mut SharedAttestations,
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
            attestations,
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
            recovery.row.record_session_launch_cwd();
        }
        if let Some(model) = captures.model()? {
            recovery.row.model = Some(model.to_owned());
        }
        if let Some(final_message) = captures.final_message()? {
            recovery.row.final_message = Some(final_message.to_owned());
        }
        // Deliberately no usage statement: `captures` here is the newest
        // attestation from *before* this attempt, kept for resume identity.
        // The adopted attempt has not reported usage yet.
    }
    Ok(())
}

pub(super) fn reconcile_retained_adapter_attestations(
    plan: &crate::recovery::RecoveryPlan,
    witness: &[WitnessRecord],
    config: &Config,
    executor: &Executor,
    attestations: &mut SharedAttestations,
) {
    let existing = match adapter_attestation_keys(attestations) {
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
    let previous_lineage = plan
        .actions
        .iter()
        .filter_map(|action| match action {
            RecoveryAction::RePresent {
                row,
                previous_attempt,
                previous_usage_predecessor,
                ..
            } => Some((
                row.uuid,
                (*previous_attempt, previous_usage_predecessor.clone()),
            )),
            _ => None,
        })
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
        if existing.contains(&(task_uuid.to_string(), record.attempt, record.lease_epoch)) {
            continue;
        }
        let Some(adapter_config) = config.adapters.get(&row.adapter) else {
            continue;
        };
        let identity = ExecutionIdentity {
            job_id: task_uuid,
            task_uuid: Some(task_uuid),
            task_ref: row.orchestration.as_ref().and_then(Orchestration::task_ref),
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
            Ok(captures) => captures,
            Err(error) => {
                eprintln!(
                    "tally: retained adapter capture for {task_uuid} could not be scraped: {error}"
                );
                continue;
            }
        };
        let predecessor = previous_lineage
            .get(&task_uuid)
            .filter(|(attempt, _)| *attempt == record.attempt)
            .map(|(_, predecessor)| predecessor.clone())
            .unwrap_or_else(|| row.usage_predecessor.clone());
        let mode = if let Some(predecessor) = predecessor {
            UsageAccountingMode::Resume {
                predecessor: Some(predecessor),
            }
        } else if adapter_config.usage_counter_scope
            == crate::adapters::UsageCounterScope::SessionCumulative
            && (record.labor_class == LaborClass::Recovered || row.resumed_from.is_some())
        {
            // A pre-schema recovered or cross-task continuation has no bound
            // predecessor to trust. Preserve that resume as unavailable; a
            // fresh retry can also have attempt > 1 and must remain fresh.
            UsageAccountingMode::Resume { predecessor: None }
        } else {
            UsageAccountingMode::Fresh
        };
        let predecessor_payload = match attestations.usage_predecessor_payload(&mode) {
            Ok(payload) => payload,
            Err(error) => {
                eprintln!(
                    "tally: retained adapter predecessor for {task_uuid} could not be read: {error}"
                );
                None
            }
        };
        let usage_evidence = crate::usage::account_usage(
            &row.adapter,
            adapter_config,
            &captures,
            &mode,
            predecessor_payload.as_ref(),
        );
        if let Err(error) = attestations.ledger().and_then(|ledger| {
            ledger.append(json!({
                "kind": "adapter-scrape",
                "taskUuid": task_uuid.to_string(),
                "jobId": task_uuid.to_string(),
                "adapter": row.adapter,
                "attempt": record.attempt,
                "leaseEpoch": record.lease_epoch,
                "captures": captures.captures,
                "usage": usage_evidence.observed.clone(),
                "usageEvidence": usage_evidence,
                "usageAuthority": "advisory-only",
                "reconciledAfterRestart": true,
            }))
        }) {
            eprintln!(
                "tally: retained adapter attestation for {task_uuid} could not be appended: {error}"
            );
        }
    }
}

pub(super) fn adapter_attestation_keys(
    attestations: &mut SharedAttestations,
) -> Result<BTreeSet<(String, u32, u64)>, DaemonError> {
    let mut keys = BTreeSet::new();
    for record in attestations.ledger()?.records()? {
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

pub(super) fn verified_adapter_attestation_captures(
    attestations: &mut SharedAttestations,
    task_uuid: Uuid,
    adapter: &str,
    attempt: u32,
    lease_epoch: u64,
) -> Result<Option<ScrapeResult>, DaemonError> {
    let task_uuid = task_uuid.to_string();
    let mut selected = None;
    for record in attestations.ledger()?.records()? {
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

/// Latest raw observation and accounting result for one exact attempt.
/// Pre-schema payloads retain their raw `usage` projection and deliberately
/// return no accounting result; callers must not guess a fresh contract for
/// them.
pub(super) fn verified_adapter_attestation_usage(
    attestations: &mut SharedAttestations,
    task_uuid: Uuid,
    adapter: &str,
    attempt: u32,
    lease_epoch: u64,
) -> Result<
    Option<(
        crate::usage::UsageObservation,
        Option<crate::usage::UsageAccounting>,
    )>,
    DaemonError,
> {
    let task_uuid = task_uuid.to_string();
    let mut selected = None;
    for record in attestations.ledger()?.records()? {
        let payload = &record.payload;
        if payload.get("kind").and_then(Value::as_str) != Some("adapter-scrape")
            || payload.get("taskUuid").and_then(Value::as_str) != Some(task_uuid.as_str())
            || payload.get("adapter").and_then(Value::as_str) != Some(adapter)
            || payload.get("attempt").and_then(Value::as_u64) != Some(u64::from(attempt))
            || payload.get("leaseEpoch").and_then(Value::as_u64) != Some(lease_epoch)
        {
            continue;
        }
        if let Some(value) = payload.get("usageEvidence") {
            let evidence: crate::usage::UsageEvidence = serde_json::from_value(value.clone())
                .map_err(|error| {
                    DaemonError::Invalid(format!(
                        "adapter scrape attestation for {task_uuid} attempt {attempt} has invalid usageEvidence: {error}"
                    ))
                })?;
            if evidence.schema_version != crate::usage::USAGE_EVIDENCE_SCHEMA_VERSION {
                return Err(DaemonError::Invalid(format!(
                    "adapter scrape attestation for {task_uuid} attempt {attempt} has unsupported usageEvidence schema {}",
                    evidence.schema_version
                )));
            }
            selected = Some((evidence.observed, Some(evidence.accounting)));
        } else if let Some(value) = payload.get("usage") {
            let observed = serde_json::from_value(value.clone()).map_err(|error| {
                DaemonError::Invalid(format!(
                    "adapter scrape attestation for {task_uuid} attempt {attempt} has invalid legacy usage: {error}"
                ))
            })?;
            selected = Some((observed, None));
        }
    }
    Ok(selected)
}

pub(super) fn verified_latest_adapter_attestation_before(
    attestations: &mut SharedAttestations,
    task_uuid: Uuid,
    adapter: &str,
    before_attempt: u32,
) -> Result<Option<ScrapeResult>, DaemonError> {
    let task_uuid = task_uuid.to_string();
    let mut selected = None;
    for record in attestations.ledger()?.records()? {
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

pub(super) fn recovered_model_is_advisory(
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

pub(super) fn ensure_verified_resume_attestation(
    attestations: &mut SharedAttestations,
    row: &RowSeed,
    adapter: Option<&AdapterConfig>,
    attempt: u32,
    lease_epoch: u64,
    previous_usage_predecessor: Option<&crate::usage::UsagePredecessor>,
    captures: &ScrapeResult,
) -> Result<(), DaemonError> {
    if let Some(stored) = verified_adapter_attestation_captures(
        attestations,
        row.uuid,
        &row.adapter,
        attempt,
        lease_epoch,
    )? {
        if stored != *captures {
            return Err(DaemonError::Invalid(format!(
                "verified adapter scrape attestation for {} attempt {} disagrees with retained capture",
                row.uuid, attempt
            )));
        }
        return Ok(());
    }
    let adapter_config = adapter.ok_or_else(|| {
        DaemonError::Invalid(format!(
            "adapter {:?} is unavailable while synthesizing its resume checkpoint",
            row.adapter
        ))
    })?;
    let previous_mode = if let Some(predecessor) = previous_usage_predecessor {
        UsageAccountingMode::Resume {
            predecessor: Some(predecessor.clone()),
        }
    } else if adapter_config.usage_counter_scope
        == crate::adapters::UsageCounterScope::SessionCumulative
        && (attempt > 1 || row.resumed_from.is_some())
    {
        // A pre-schema recovered resume may have no durable predecessor. Keep
        // that unknown as a resume with an unavailable predecessor; calling it
        // fresh would charge the cumulative observation.
        UsageAccountingMode::Resume { predecessor: None }
    } else {
        UsageAccountingMode::Fresh
    };
    let predecessor_payload = attestations.usage_predecessor_payload(&previous_mode)?;
    let usage_evidence = crate::usage::account_usage(
        &row.adapter,
        adapter_config,
        captures,
        &previous_mode,
        predecessor_payload.as_ref(),
    );
    attestations.ledger()?.append(json!({
        "kind": "adapter-scrape",
        "taskUuid": row.uuid.to_string(),
        "jobId": row.uuid.to_string(),
        "adapter": row.adapter,
        "attempt": attempt,
        "leaseEpoch": lease_epoch,
        "captures": captures.captures,
        "usage": usage_evidence.observed,
        "usageEvidence": usage_evidence,
        "usageAuthority": "advisory-only",
        "recoveryCheckpoint": true,
    }))?;
    let stored = verified_adapter_attestation_captures(
        attestations,
        row.uuid,
        &row.adapter,
        attempt,
        lease_epoch,
    )?
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

pub(super) fn restore_completed_aliases(
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

pub(super) fn validate_recovery_briefs(
    plan: &crate::recovery::RecoveryPlan,
    data_dir: &Path,
) -> Result<(), DaemonError> {
    let mut hashes = BTreeMap::<&str, bool>::new();
    for recovery in &plan.rows {
        let Some(hash) = recovery.row.brief_hash.as_deref() else {
            continue;
        };
        let required = !matches!(
            recovery.state,
            crate::recovery::RecoveryRowState::Completed
                | crate::recovery::RecoveryRowState::Deleted
        );
        hashes
            .entry(hash)
            .and_modify(|existing| *existing |= required)
            .or_insert(required);
    }
    for (hash, required) in hashes {
        let path = brief::content_path(data_dir, hash)
            .map_err(|error| DaemonError::Invalid(error.to_string()))?;
        if !required && !path.exists() {
            continue;
        }
        brief::read_verified(&path, hash)
            .map_err(|error| DaemonError::Invalid(error.to_string()))?;
    }
    Ok(())
}

pub(super) fn restore_guardrail_parents(
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

pub(super) fn install_recovery_jobs(
    context: &mut Context,
    plan: &crate::recovery::RecoveryPlan,
    executor: &Executor,
    attestations: &mut SharedAttestations,
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
            attestations,
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
                row.record_session_launch_cwd();
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
            && job
                .row
                .pools
                .iter()
                .any(|pool| context.paused_pools.contains(pool))
        {
            job.state = JobState::Paused;
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

fn restore_job_tokens(context: &Context) -> Result<HashMap<String, Uuid>, DaemonError> {
    let mut restored = HashMap::new();
    for job in context.jobs.values().filter(|job| job.adopted) {
        let Some(job_token_hash) = &job.row.job_token_hash else {
            continue;
        };
        if job.row.executor.is_some() {
            return Err(DaemonError::Invalid(format!(
                "remote recovered job {} carries a local job token hash",
                job.stable_key()
            )));
        }
        if let Some(other) = restored.insert(job_token_hash.clone(), job.job_id) {
            return Err(DaemonError::Invalid(format!(
                "recovered jobs {other} and {} carry the same job token hash",
                job.stable_key()
            )));
        }
    }
    Ok(restored)
}

pub(super) fn recovery_action_already_installed(
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

pub(super) fn recovery_adapter_invocation(
    config: &Config,
    action: &RecoveryAction,
    row: &RowSeed,
    executor: &Executor,
    attestations: &mut SharedAttestations,
) -> Result<(AdapterInvocation, Option<ScrapeResult>), DaemonError> {
    let engine = AdapterEngine::new(&config.adapters);
    match action {
        RecoveryAction::RePresent {
            previous_attempt,
            previous_lease_epoch,
            previous_usage_predecessor,
            ..
        } => {
            let identity = ExecutionIdentity {
                job_id: row.uuid,
                task_uuid: Some(row.uuid),
                task_ref: row.orchestration.as_ref().and_then(Orchestration::task_ref),
            };
            let checkpoint = verified_adapter_attestation_captures(
                attestations,
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
                            attestations,
                            row,
                            config.adapters.get(&row.adapter),
                            *previous_attempt,
                            *previous_lease_epoch,
                            previous_usage_predecessor.as_ref(),
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
            let invocation = engine.resume_with_options_from(
                &row.adapter,
                &row.argv,
                &captures,
                &row.adapter_options,
                row.effective_cwd(),
                row.usage_predecessor.clone(),
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
                    engine.resume_with_options_from(
                        &row.adapter,
                        &row.argv,
                        &captures,
                        &row.adapter_options,
                        row.effective_cwd(),
                        row.usage_predecessor.clone(),
                    )?,
                    Some(captures),
                ))
            } else {
                Ok((
                    engine.launch_with_options(
                        &row.adapter,
                        &row.argv,
                        &row.adapter_options,
                        row.effective_cwd(),
                    )?,
                    None,
                ))
            }
        }
        RecoveryAction::AdoptRunning { labor_class, .. }
        | RecoveryAction::ReconcileExit { labor_class, .. } => {
            let adapter = engine.adapter(&row.adapter)?;
            let usage_accounting = if let Some(predecessor) = row.usage_predecessor.clone() {
                UsageAccountingMode::Resume {
                    predecessor: Some(predecessor),
                }
            } else if adapter.usage_counter_scope
                == crate::adapters::UsageCounterScope::SessionCumulative
                && (row.resumed_from.is_some()
                    || labor_class.as_ref() == Some(&LaborClass::Recovered))
            {
                // Old running rows cannot name a predecessor. The recovery
                // action's labor class and explicit continuation marker are
                // independent resume facts; attempt number is not.
                UsageAccountingMode::Resume { predecessor: None }
            } else {
                UsageAccountingMode::Fresh
            };
            Ok((
                AdapterInvocation {
                    argv: row.argv.clone(),
                    env: BTreeMap::new(),
                    hardening: adapter.hardening,
                    extra_writable_paths: adapter.extra_writable_paths.clone(),
                    yield_hook: None,
                    usage_accounting,
                },
                None,
            ))
        }
        _ => Err(DaemonError::Invalid(
            "non-executable recovery action reached adapter rendering".to_owned(),
        )),
    }
}

pub(super) fn task_recovery_action(action: &RecoveryAction) -> Option<(Uuid, bool, bool)> {
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

pub(super) fn recovery_expected_invocation_id(action: &RecoveryAction) -> Option<String> {
    match action {
        RecoveryAction::AdoptRunning { invocation_id, .. } => Some(invocation_id.clone()),
        RecoveryAction::ReconcileExit { record, .. } => Some(record.invocation_id.clone()),
        _ => None,
    }
}
