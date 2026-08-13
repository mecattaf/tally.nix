use super::*;

use tokio::task::JoinSet;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
#[derive(Clone)]
pub(super) struct LeaseTickHook {
    pub(super) started: mpsc::UnboundedSender<()>,
    pub(super) release: watch::Receiver<bool>,
}

/// Hold `finish_job` open between its first job read and its second-phase
/// re-check.
///
/// The re-check exists because the whole scrape/capture/accounting stretch
/// between the two is awaited with the context lock dropped, so the job can
/// reach a terminal disposition and be retired (#395) underneath it. That
/// window is real but it is not something a test can time, so this makes it
/// enterable: the hook announces that phase one is done and blocks until the
/// test says go.
#[cfg(test)]
#[derive(Clone)]
pub(super) struct FinishJobHook {
    pub(super) entered: mpsc::UnboundedSender<()>,
    pub(super) release: watch::Receiver<bool>,
}

#[cfg(test)]
impl FinishJobHook {
    async fn between_phases(&self) {
        let mut release = self.release.clone();
        if *release.borrow() {
            return;
        }
        let _ = self.entered.send(());
        while !*release.borrow() {
            if release.changed().await.is_err() {
                break;
            }
        }
    }
}

/// Hold one `select!` arm's body open, the way a slow terminal transaction or a
/// lifecycle compaction holds it open on the estate. Nothing else in the loop
/// runs while a body is held, which is exactly the condition the watchdog
/// keepalive must survive.
///
/// `blocking` reproduces the shape that matters most: `WitnessLedger::append`
/// and `LifecycleLog::compact_if_over_limit` spend their time in `flock`,
/// `write_all` and `sync_all`, so they hold the single runtime thread rather
/// than parking on an `await`. A hook that only awaits cannot reach that class.
#[cfg(test)]
#[derive(Clone)]
pub(super) struct DispatchStallHook {
    pub(super) entered: mpsc::UnboundedSender<()>,
    pub(super) release: watch::Receiver<bool>,
    pub(super) blocking: Option<Duration>,
    pub(super) blocked: Rc<Cell<bool>>,
}

#[cfg(test)]
impl DispatchStallHook {
    async fn stall(&self) {
        let mut release = self.release.clone();
        if *release.borrow() {
            return;
        }
        if let Some(blocking) = self.blocking {
            if !self.blocked.replace(true) {
                let _ = self.entered.send(());
                std::thread::sleep(blocking);
            }
            return;
        }
        let _ = self.entered.send(());
        while !*release.borrow() {
            if release.changed().await.is_err() {
                break;
            }
        }
    }
}

impl Daemon {
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
        if let Some(timeline) = self.startup.as_mut() {
            timeline.phase("initial-recovery");
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
        let mut lease_tick = tokio::time::interval(LEASE_TICK);
        lease_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let storage_poll_interval = self.handler.storage.borrow().poll_interval;
        let mut storage_tick = tokio::time::interval_at(
            tokio::time::Instant::now() + storage_poll_interval,
            storage_poll_interval,
        );
        storage_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut lease_ticks = JoinSet::new();
        let mut storage_samples = JoinSet::new();
        let mut connections = JoinSet::new();
        let max_connections = self.handler.settings.max_connections;
        #[cfg(test)]
        let connection_count_hook = self.connection_count_hook.clone();
        #[cfg(test)]
        let dispatch_stall_hook = self.dispatch_stall_hook.clone();
        let mut keepalive = None;
        // Written before `READY=1`, so the phase breakdown is in the journal
        // even for a start that then fails to notify (#379). This line is the
        // only record of where the pre-`READY` minute went; the journal is
        // otherwise silent from `Starting` to the first late-startup warning.
        if let Some(timeline) = self.startup.take() {
            let report = timeline.finish();
            #[cfg(test)]
            if let Some(hook) = &self.startup_report_hook {
                let _ = hook.send(report.clone());
            }
            eprintln!("tally: {report}");
        }
        let mut result = match self.notifier.ready() {
            Err(error) => Err(error),
            Ok(()) => {
                // READY=1 is what arms the service watchdog, so this is the
                // first instant a keepalive is owed and the last instant it
                // can be started from. Everything before it — the whole of
                // `Daemon::open`, including unit-fact collection and the
                // startup projection sweep — is covered by TimeoutStartSec
                // instead, and cannot miss a watchdog deadline.
                //
                // The keepalive lives on its own OS thread from here. It is
                // deliberately not a `select!` arm any more: an arm is only
                // polled when the loop comes back around to poll it, so any
                // one slow arm body used to hold the ping until systemd gave
                // up. What the loop still owes is proof that it came back
                // around, stamped below.
                keepalive = self.notifier.keepalive(self.handler.fatal.clone());
                let progress = keepalive.as_ref().map(WatchdogKeepalive::progress);
                loop {
                    if let Some(progress) = &progress {
                        progress.stamp();
                    }
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
                            #[cfg(test)]
                            if let Some(hook) = &dispatch_stall_hook {
                                hook.stall().await;
                            }
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
                        _ = storage_tick.tick() => {
                            self.handler.compact_lifecycle_if_needed().await;
                            if storage_samples.is_empty() {
                                let handler = self.handler.clone();
                                storage_samples.spawn_local(async move {
                                    handler.refresh_storage_now().await
                                });
                            }
                        }
                        Some(sampled) = storage_samples.join_next(), if !storage_samples.is_empty() => {
                            if let Err(error) = sampled {
                                eprintln!("tally: storage sampling task failed: {error}");
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
                }
            }
        };
        // The keepalive stops before the daemon announces STOPPING=1, so no
        // WATCHDOG=1 can follow that announcement.
        if let Some(mut keepalive) = keepalive.take() {
            keepalive.shutdown();
        }
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
        while let Some(sampled) = storage_samples.join_next().await {
            if let Err(error) = sampled {
                eprintln!("tally: storage sampling task failed during shutdown: {error}");
            }
        }
        connections.shutdown().await;
        let _ = self.execution_shutdown.send(true);
        // Advisory scrape attestations are outside the terminal fsync barrier,
        // but they still belong to this daemon lifetime. Join them while the
        // state lock is still exclusively owned.
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

    pub(super) async fn finish_job(&self, finished: ExecutionFinished) -> Result<(), DaemonError> {
        let job = {
            let context = self.handler.context.read().await;
            let Some(job) = context.jobs.get(&finished.job_id).cloned() else {
                // Retired: the job already reached a terminal disposition, most
                // often a forced cancel that raced this execution's own exit
                // (#395). Absence from the live map is the same fact the
                // `Completed` check below reports, so it takes the same exit.
                // An id the daemon never admitted is still an error.
                return if context.rows.contains_key(&finished.job_id)
                    || context.query_rows.contains_key(&finished.job_id)
                {
                    Ok(())
                } else {
                    Err(DaemonError::Invalid(format!(
                        "unknown completed job {}",
                        finished.job_id
                    )))
                };
            };
            if job.state == JobState::Completed
                || job.row.attempt != finished.attempt
                || job.row.lease_epoch != finished.lease_epoch
            {
                return Ok(());
            }
            job
        };
        // Stands in for the window this function really has: the lock is
        // dropped above and everything from here to the second-phase re-check
        // is awaited without it, so the job can be retired underneath us. A
        // test cannot time that window; this lets it hold the window open and
        // step into it deterministically (#395).
        #[cfg(test)]
        if let Some(hook) = &self.finish_job_hook {
            hook.between_phases().await;
        }
        let evidence_spec = parse_evidence_specs(&job.row.evidence)
            .map_err(|error| DaemonError::Invalid(error.to_string()))?;
        let scrape_capture = matches!(
            &finished.outcome,
            Some(Ok(outcome)) if outcome.captures_available
        );
        let effective_gate_manifest = effective_gate_manifest(&self.handler.executor, &job)?;
        let execution_host_id = match &finished.outcome {
            Some(Ok(outcome)) => outcome.host_id.clone(),
            _ => None,
        };
        // `finished.outcome` is matched by value below (its `record.termination`
        // and `record.evidence_gate` fields are partially moved out), so the
        // accounting sample the exit recorder measured has to be copied out
        // here while a shared reference is still enough. `UnitAccounting` is
        // `Copy` for exactly this: cheap to lift out before the move.
        let accounting = match &finished.outcome {
            Some(Ok(outcome)) => outcome.record.accounting,
            _ => None,
        };
        let terminal_error = match &finished.outcome {
            Some(Err(ExecutorError::InvalidRequest(validation_message))) => Some(TerminalError {
                code: EXECUTOR_VALIDATION_FAILURE_CODE.to_owned(),
                message: format!("execution request is invalid: {validation_message}"),
                details: Some(json!({
                    "validationMessage": validation_message,
                })),
            }),
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
            // A dispatch that never got the capture lock never ran the job, so
            // there is no execution for a gate manifest to describe. Evaluating
            // one would stamp a failed execution fact onto an attempt the agent
            // never started.
            (Some(_), Some(Err(error))) if undispatched_execution(error) => None,
            (Some(spec), Some(Err(error))) => {
                let reason = format!("executor failed: {error}");
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
                // The capture lock was busy for the whole deadline, so the unit
                // was never launched. Attributing that to the agent burns an
                // attempt, writes a `Failed` witness, and — with
                // `postFailureEvidence` on — posts a public failure receipt with
                // no evidence in it, for a daemon-side file-locking condition.
                // `Preempted` is the existing verdict for "the attempt did not
                // get to run": it is excluded from canonical GPU seconds, emits
                // a `Preempted` lifecycle event rather than a failure one, and
                // carries a `ResourceReturn` retry trigger.
                Some(Err(error @ ExecutorError::CaptureLockContended { .. })) => {
                    eprintln!(
                        "tally: capture lock for {} was contended, so the attempt never ran: {error}",
                        job.stable_key()
                    );
                    (Verdict::Preempted, 1, None, None, Vec::new())
                }
                Some(Err(error)) => {
                    eprintln!("tally: executor failed for {}: {error}", job.stable_key());
                    (Verdict::Failed, 1, None, None, Vec::new())
                }
            };
        let computed_verdict = canonical_verdict(evidence_verdict, semantic_completion.as_ref());
        let stderr_excerpt = if terminal_lifecycle_event(computed_verdict, artifact_hash.is_some())
            == TallyEvent::Failed
        {
            let executor = self.handler.executor.clone();
            let stable = job.stable_key();
            let identity = job.identity();
            let attempt = finished.attempt;
            let lease_epoch = finished.lease_epoch;
            tokio::task::spawn_blocking(move || {
                match executor.persist_failure_stderr(&identity, attempt, lease_epoch) {
                    Ok(Some(excerpt)) => Some(excerpt),
                    Ok(None) => {
                        eprintln!(
                            "tally: failure stderr for {stable} has no matching capture generation"
                        );
                        None
                    }
                    Err(error) => {
                        eprintln!("tally: could not persist failure stderr for {stable}: {error}");
                        None
                    }
                }
            })
            .await
            .unwrap_or_else(|error| {
                eprintln!(
                    "tally: failure stderr worker failed for {}: {error}",
                    job.stable_key()
                );
                None
            })
        } else {
            None
        };

        let (result, evidence, launches, auto_requeue) = {
            let mut context = self.handler.context.write().await;
            // Re-checked under the write lock, because everything between here
            // and the read above was awaited without it. `is_none_or`, not
            // `is_some_and`: a job retired while this ran (#395) is terminal,
            // and reading its absence as "still eligible" would append a second
            // canonical witness for one execution.
            if context.jobs.get(&finished.job_id).is_none_or(|job| {
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
            let gpu_pool_job = job.row.pools.iter().any(|pool| {
                context.lease.engine().declared_resource_kind(pool) == Some(ResourceKind::Vram)
            });
            let (charge, gpu_seconds) = accounting_witness_fields(accounting, gpu_pool_job);
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
                    gpu_seconds,
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
                    charge,
                    model: model.clone(),
                    evidence_class: job.row.evidence_class.clone(),
                    manifest_hash: job.row.manifest_hash.clone(),
                    completion: semantic_completion.clone(),
                    error: terminal_error.clone(),
                    result_revision: None,
                    authorship: None,
                    authorship_sessions: None,
                },
            )?;
            let result = JobResult {
                gpu_seconds: record.gpu_seconds,
                task_uuid: job.task_uuid.map(|uuid| uuid.to_string()),
                task_ref: job.task_ref(),
                job_id: job.job_id.to_string(),
                verdict,
                exit_code,
                artifact_content_hash: artifact_hash,
                attempt: job.row.attempt,
                lease_epoch: job.row.lease_epoch,
                witness_seq: record.seq,
                model,
                completion: semantic_completion,
                error: terminal_error,
                stderr_excerpt,
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
            // Terminal: the job leaves the live map (#395). Everything below
            // reads the `job` clone taken above, and the two verbs that can
            // still ask about a finished job read `context.rows` and
            // `context.query_rows`, which keep it.
            retire_job(&mut context, finished.job_id);
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
        // release, scrape, attestations, and journald are post-ack.
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

    pub(super) async fn tick_leases_at(
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

pub(super) fn promoted_jobs(context: &mut Context, grants: Vec<LeaseGrant>) -> Vec<Job> {
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
                && !job
                    .row
                    .pools
                    .iter()
                    .any(|pool| context.paused_pools.contains(pool))
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

pub(super) fn resume_paused_jobs_locked(
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
            || job
                .row
                .pools
                .iter()
                .any(|pool| context.paused_pools.contains(pool))
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

/// Did the executor fail before the job could start?
///
/// A contended capture lock is the one executor error that means "the daemon
/// could not prepare this dispatch", not "the work went wrong". It must not be
/// attributed to the agent, and there is no execution for a gate manifest to
/// describe.
const fn undispatched_execution(error: &ExecutorError) -> bool {
    matches!(error, ExecutorError::CaptureLockContended { .. })
}
