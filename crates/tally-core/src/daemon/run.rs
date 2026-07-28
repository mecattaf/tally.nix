use super::*;

use tokio::task::JoinSet;

#[cfg(test)]
#[derive(Clone)]
pub(super) struct LeaseTickHook {
    pub(super) started: mpsc::UnboundedSender<()>,
    pub(super) release: watch::Receiver<bool>,
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

    pub(super) async fn finish_job(&self, finished: ExecutionFinished) -> Result<(), DaemonError> {
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

pub(super) fn pool_representations(
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

pub(super) fn renderable_pool_return_rows(
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

pub(super) fn merge_selected_pool_returns(
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
