use super::*;

pub(super) struct TerminalWork {
    pub(super) job: Job,
    pub(super) result: JobResult,
    pub(super) evidence: String,
    pub(super) evidence_checks: Vec<CheckOutcome>,
    pub(super) launches: Vec<Job>,
    pub(super) scrape_capture: bool,
}

pub(super) struct PreparedExecution {
    pub(super) job_token: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct GhTerminalWork {
    pub(super) row: RowSeed,
    pub(super) result: JobResult,
}

impl DaemonHandler {
    pub(super) fn post_storage_warning_receipts(
        &self,
        origin: Option<crate::taskdb::GhOrigin>,
        warnings: &[crate::storage::ActiveStorageWarning],
    ) {
        let Some(origin) = origin else {
            return;
        };
        for warning in warnings {
            let receipt_key = StorageReceiptKey {
                producer: origin.producer.clone(),
                source: origin.source.clone(),
                item_id: origin.node_id.clone(),
                warning_sequence: warning.warning_sequence,
            };
            if !self.storage_receipts.borrow_mut().insert(receipt_key) {
                continue;
            }
            let warning = warning.clone();
            let handler = self.clone();
            let origin = origin.clone();
            let task = tokio::task::spawn_local(async move {
                let (registry, events_dir, state_dir, data_dir, gh_program, mut shutdown) = {
                    let context = handler.context.read().await;
                    (
                        context.config.producers.clone(),
                        context.paths.events_dir(),
                        context.paths.state_dir.clone(),
                        context.paths.data_dir.clone(),
                        handler.gh_program.clone(),
                        handler.execution_shutdown.clone(),
                    )
                };
                let attestations = Arc::clone(&handler.attestations);
                let orphan_hint = orphan_listing_command(&state_dir);
                let mut retry_delay = Duration::from_secs(1);
                loop {
                    let registry = registry.clone();
                    let events_dir = events_dir.clone();
                    let state_dir = state_dir.clone();
                    let data_dir = data_dir.clone();
                    let gh_program = gh_program.clone();
                    let origin = origin.clone();
                    let warning = warning.clone();
                    let warning_sequence = warning.warning_sequence;
                    let attestations = Arc::clone(&attestations);
                    let posted = tokio::task::spawn_blocking(move || {
                        let engine =
                            ProducerEngine::new(&registry, events_dir, &state_dir, data_dir);
                        let mut sink =
                            GhCliMutationSink::with_program(gh_program).with_state_dir(&state_dir);
                        let outcome =
                            engine.project_storage_warning(&origin, &warning, &mut sink)?;
                        witness_projection_outcome(&attestations, &outcome, None, None);
                        Ok::<_, ProducerError>(outcome)
                    })
                    .await;
                    match posted {
                        Ok(Ok(GhProjectionOutcome::Settled { .. })) => break,
                        Ok(Ok(GhProjectionOutcome::Orphaned { record })) => {
                            eprintln!(
                                "tally: GitHub storage-warning receipt {warning_sequence} is \
                                 orphaned and will not retry: {}. List every orphaned projection \
                                 with: {orphan_hint}",
                                record.detail
                            );
                            break;
                        }
                        Ok(Err(error)) => eprintln!(
                            "tally: GitHub storage-warning receipt {} failed and will retry: {error}",
                            warning_sequence
                        ),
                        Err(error) => eprintln!(
                            "tally: GitHub storage-warning worker {} failed and will retry: {error}",
                            warning_sequence
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
    }

    pub(super) fn complete_terminal_post_ack(&self, work: TerminalWork) {
        self.revoke_job_token(&work.job);
        self.complete_gh_post_ack(work.job.row.clone(), work.result.clone());
        for check in &work.evidence_checks {
            self.emit_post_ack(evidence_event(&work.job, check));
        }
        self.emit_scraped_completion(work.job, work.result, work.evidence, work.scrape_capture);
        for job in work.launches {
            self.spawn_execution(job);
        }
    }

    pub(super) fn revoke_job_token(&self, job: &Job) {
        if let Some(job_token_hash) = &job.row.job_token_hash {
            if self
                .job_tokens
                .borrow_mut()
                .remove(job_token_hash)
                .is_some_and(|job_id| job_id != job.job_id)
            {
                let error = DaemonError::Invalid(format!(
                    "job token hash for {} was registered to another job",
                    job.stable_key()
                ));
                eprintln!("tally: {error}");
                let _ = self.fatal.send(error);
            }
        }
    }

    pub(super) fn complete_gh_post_ack(&self, row: RowSeed, result: JobResult) {
        let Some(origin) = row.gh_origin.clone() else {
            return;
        };
        let handler = self.clone();
        let task = tokio::task::spawn_local(async move {
            let (registry, events_dir, state_dir, data_dir, gh_program, mut shutdown) = {
                let context = handler.context.read().await;
                (
                    context.config.producers.clone(),
                    context.paths.events_dir(),
                    context.paths.state_dir.clone(),
                    context.paths.data_dir.clone(),
                    handler.gh_program.clone(),
                    handler.execution_shutdown.clone(),
                )
            };
            let attestations = Arc::clone(&handler.attestations);
            let orphan_hint = orphan_listing_command(&state_dir);
            let task_uuid = row.uuid.to_string();
            let completion_id = gh_completion_id(row.uuid, result.attempt, result.witness_seq);
            let mut evidence = json!({
                "taskUuid": row.uuid.to_string(),
                "witnessSeq": result.witness_seq,
                "verdict": result.verdict,
                "exitCode": result.exit_code,
                "artifactContentHash": result.artifact_content_hash,
                "adapter": row.adapter,
                "model": result.model,
            });
            if let Some(task_ref) = &result.task_ref {
                evidence["taskRef"] = Value::String(task_ref.to_string());
            }
            if let Some(completion) = &result.completion {
                evidence["completion"] = serde_json::to_value(completion)
                    .expect("semantic completion always serializes");
            }
            if let Some(stderr) = &result.stderr_excerpt {
                evidence["stderrTail"] = Value::String(stderr.text.clone());
                evidence["stderrTruncated"] = Value::Bool(stderr.truncated);
            }
            let mut retry_delay = Duration::from_secs(1);
            loop {
                let registry = registry.clone();
                let events_dir = events_dir.clone();
                let state_dir = state_dir.clone();
                let data_dir = data_dir.clone();
                let gh_program = gh_program.clone();
                let origin = origin.clone();
                let completion_id = completion_id.clone();
                let task_uuid = task_uuid.clone();
                let evidence = evidence.clone();
                let semantic_completion = result.completion.clone();
                let verdict = result.verdict;
                let attempt = result.attempt;
                let lease_epoch = result.lease_epoch;
                let attestations = Arc::clone(&attestations);
                let completed = tokio::task::spawn_blocking(move || {
                    let engine = ProducerEngine::new(&registry, events_dir, &state_dir, data_dir);
                    let mut sink =
                        GhCliMutationSink::with_program(gh_program).with_state_dir(&state_dir);
                    let outcome = engine.project_gh_completion(
                        GhCompletionProjection {
                            origin: &origin,
                            completion_id: &completion_id,
                            task_uuid: Some(&task_uuid),
                            verdict,
                            evidence: Some(evidence),
                            completion: semantic_completion,
                        },
                        &mut sink,
                    )?;
                    witness_projection_outcome(
                        &attestations,
                        &outcome,
                        Some(attempt),
                        Some(lease_epoch),
                    );
                    Ok::<_, ProducerError>(outcome)
                })
                .await;
                match completed {
                    Ok(Ok(GhProjectionOutcome::Settled { .. })) => break,
                    Ok(Ok(GhProjectionOutcome::Orphaned { record })) => {
                        // The completion is settled and witnessed; only the
                        // forge-side projection is lost, and no retry can
                        // change that while the producer is absent.
                        eprintln!(
                            "tally: post-ack GitHub COMPLETED mutation for {} is orphaned and \
                             will not retry: {}. List every orphaned projection with: \
                             {orphan_hint}",
                            row.uuid, record.detail
                        );
                        break;
                    }
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

    pub(super) fn emit_scraped_completion(
        &self,
        job: Job,
        result: JobResult,
        evidence: String,
        scrape_capture: bool,
    ) {
        if !scrape_capture {
            // No capture is available for this attempt -- usage and the
            // scraped half of context_window genuinely have nothing to read.
            // A config-declared ceiling depends on none of that, so it is
            // still checked here rather than silently narrowing the promise
            // `crate::occupancy`'s doc makes ("the config ceiling still
            // needs checking, because it depends on nothing scraped").
            let handler = self.clone();
            let task = tokio::task::spawn_local(async move {
                let mut job = job;
                job.row.context_window = {
                    let context = handler.context.read().await;
                    context
                        .config
                        .adapters
                        .get(&job.row.adapter)
                        .and_then(|adapter| occupancy::context_window(adapter, None))
                };
                if job.row.context_window.is_some() {
                    // Not written back to `context.jobs`: this runs post-ack,
                    // by which point the job is terminal and retired out of the
                    // live map (#395). The query fact is where every reader of
                    // a finished job's occupancy looks anyway.
                    let mut context = handler.context.write().await;
                    if let Some(detail) = job
                        .task_uuid
                        .and_then(|task_uuid| context.query_details.get_mut(&task_uuid))
                    {
                        detail.context_window = job.row.context_window;
                    }
                }
                handler.emit_post_ack(completed_event(&job, &result, evidence));
            });
            let mut tasks = self.post_ack_tasks.borrow_mut();
            tasks.retain(|task| !task.is_finished());
            tasks.push(task);
            return;
        }
        let handler = self.clone();
        let task = tokio::task::spawn_local(async move {
            let (adapters, state_dir, pools) = {
                let context = handler.context.read().await;
                (
                    context.config.adapters.clone(),
                    context.paths.state_dir.clone(),
                    context.config.pools.clone(),
                )
            };
            let attestations = Arc::clone(&handler.attestations);
            let scrape_configured = adapters
                .get(&job.row.adapter)
                .is_some_and(|adapter| !adapter.scrape.is_empty());
            if !scrape_configured {
                // An adapter with no captures at all declared no usage scrape.
                // Record that as a value so a later reader is not left to infer
                // it from a missing key. A config-declared context window does
                // not depend on a scrape, so it is still checked here.
                let mut job = job;
                job.row.usage = Some(UsageObservation::NotDeclared);
                job.row.context_window = adapters
                    .get(&job.row.adapter)
                    .and_then(|adapter| occupancy::context_window(adapter, None));
                {
                    // Post-ack, so the job is terminal and already retired out
                    // of `context.jobs` (#395); the query fact is the durable
                    // home of a finished job's observations.
                    let mut context = handler.context.write().await;
                    if let Some(detail) = job
                        .task_uuid
                        .and_then(|task_uuid| context.query_details.get_mut(&task_uuid))
                    {
                        detail.usage.clone_from(&job.row.usage);
                        detail.context_window = job.row.context_window;
                    }
                }
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
                // Normalization runs against the adapter's own declared key
                // mapping, so a harness the tree has never seen is a config
                // entry rather than a Rust change. The three states are
                // decided here, once, and every later reader sees the same
                // one.
                let usage = adapters
                    .get(&adapter)
                    .map_or(UsageObservation::NotDeclared, |config| {
                        crate::usage::observe(config, &captures)
                    });
                // Occupancy reads the same captures usage was normalized
                // from, but through its own narrower resolution -- it is not
                // derived from `usage`, which keeps a session-lifetime
                // roll-up under a spend meaning. See `crate::occupancy`.
                let context_tokens = adapters
                    .get(&adapter)
                    .and_then(|config| occupancy::context_tokens(config, &captures));
                let context_window = adapters
                    .get(&adapter)
                    .and_then(|config| occupancy::context_window(config, Some(&captures)));
                let attestation_error = if captures.captures.is_empty() {
                    None
                } else {
                    attestations
                        .lock()
                        .expect("attestation ledger lock poisoned")
                        .ledger()
                        .and_then(|ledger| {
                            ledger.append(json!({
                                "kind": "adapter-scrape",
                                "taskUuid": stable_key,
                                "jobId": job_id,
                                "adapter": adapter,
                                "attempt": attempt,
                                "leaseEpoch": lease_epoch,
                                "captures": captures.captures.clone(),
                                "usage": usage.clone(),
                                "usageAuthority": "advisory-only",
                            }))
                        })
                        .err()
                        .map(|error| error.to_string())
                };
                let meter_errors = feed_scraped_usage(&state_dir, &pools, &leased_pools, &usage);
                Ok::<_, String>((
                    captures,
                    usage,
                    context_tokens,
                    context_window,
                    attestation_error,
                    meter_errors,
                ))
            })
            .await;

            let (captures, usage, context_tokens, context_window, attestation_error, meter_errors) =
                match scraped {
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
                enriched.row.record_session_launch_cwd();
            }
            if let Ok(Some(model)) = captures.model() {
                enriched.row.model = Some(model.to_owned());
            }
            if let Ok(Some(final_message)) = captures.final_message() {
                enriched.row.final_message = Some(final_message.to_owned());
            }
            // Unlike the three string captures, a usage observation is
            // recorded even when it is an absence: a scraped attempt that
            // carried no usage is a different fact from an attempt nobody
            // scraped, and only recording the value keeps them apart.
            enriched.row.usage = Some(usage);
            enriched.row.context_tokens = context_tokens;
            enriched.row.context_window = context_window;
            {
                // Post-ack, so the job is terminal and already retired out of
                // `context.jobs` (#395). The two facts a continuation needs --
                // the session pointer and the observed model -- land in
                // `query_rows` just below, which is where `find_job` reads them
                // back for a retired job.
                let mut context = handler.context.write().await;
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
                        detail.usage.clone_from(&enriched.row.usage);
                        detail.context_tokens = enriched.row.context_tokens;
                        detail.context_window = enriched.row.context_window;
                    }
                }
            }
            handler.emit_post_ack(completed_event(&enriched, &result, evidence));
        });
        let mut tasks = self.post_ack_tasks.borrow_mut();
        tasks.retain(|task| !task.is_finished());
        tasks.push(task);
    }

    pub(super) async fn drain_post_ack_tasks(&self) {
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

    pub(super) fn spawn_execution(&self, job: Job) {
        if job.labor_class == LaborClass::Recovered {
            self.emit_post_ack(execution_event(&job, TallyEvent::Resumed));
        }
        self.emit_post_ack(execution_event(&job, TallyEvent::Dispatched));
        self.emit_post_ack(execution_event(&job, TallyEvent::Started));
        let executor = self.executor.clone();
        let completion = self.completion.clone();
        let handler = self.clone();
        let limits = self.settings.unit_limits;
        let tally_socket = self.tally_socket.clone();
        let brief_root = self.brief_root.clone();
        let git_ai = self.git_ai.clone();
        let exec_attestations = self.exec_attestations;
        let execution_target = job.row.executor.clone();
        let evidence = job.row.evidence.clone();
        let mut shutdown = self.execution_shutdown.clone();
        let mut cancellation = self.execution_cancel.subscribe();
        tokio::task::spawn_local(async move {
            let mut job = job;
            let prepared = match handler.prepare_execution(&mut job).await {
                Ok(Some(prepared)) => prepared,
                Ok(None) => return,
                Err(error) => {
                    eprintln!("tally: execution preparation failed: {error}");
                    let _ = handler.fatal.send(error);
                    return;
                }
            };
            let request = execution_request(
                &executor,
                &job,
                limits,
                (&tally_socket, prepared.job_token.as_deref()),
                &brief_root,
                &git_ai,
                exec_attestations,
            );
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

    pub(super) async fn prepare_execution(
        &self,
        job: &mut Job,
    ) -> Result<Option<PreparedExecution>, DaemonError> {
        let mut context = self.context.write().await;
        let Some(stored) = context.jobs.get(&job.job_id) else {
            return Ok(None);
        };
        if stored.state != JobState::Running
            || stored.row.attempt != job.row.attempt
            || stored.row.lease_epoch != job.row.lease_epoch
        {
            return Ok(None);
        }
        job.row.clone_from(&stored.row);

        // Remote workers cannot reach this daemon. Adopted local units already
        // carry the token in their fixed systemd environment, so relaunching it
        // here would break identity across a daemon restart.
        if job.row.executor.is_some() || job.adopted {
            return Ok(Some(PreparedExecution { job_token: None }));
        }

        if let Some(existing_hash) = &job.row.job_token_hash {
            if self.job_tokens.borrow().contains_key(existing_hash) {
                return Err(DaemonError::Invalid(format!(
                    "job {} generation {}:{} was prepared more than once",
                    job.stable_key(),
                    job.row.attempt,
                    job.row.lease_epoch
                )));
            }
        }

        let (job_token, job_token_hash) = loop {
            let token = mint_job_token()?;
            let digest = hash_job_token(&token);
            if !self.job_tokens.borrow().contains_key(&digest) {
                break (token, digest);
            }
        };

        if let Some(task_uuid) = job.task_uuid {
            let events_dir = context.paths.events_dir();
            let mut matching_events = read_acknowledged_events(&events_dir)?
                .into_iter()
                .filter(|event| event.row.uuid == task_uuid)
                .collect::<Vec<_>>();
            if matching_events.len() != 1 {
                return Err(DaemonError::Invalid(format!(
                    "job {task_uuid} has {} acknowledged enqueue events while persisting its token hash",
                    matching_events.len()
                )));
            }
            let mut event = matching_events
                .pop()
                .expect("exactly one matching event was checked");
            event.row.job_token_hash = Some(job_token_hash.clone());
            update_enqueue_event_atomic(&events_dir, &event)?;
        }

        let mut updated_row = job.row.clone();
        updated_row.job_token_hash = Some(job_token_hash.clone());
        updated_row.validate()?;
        context
            .jobs
            .get_mut(&job.job_id)
            .expect("prepared job remains installed")
            .row
            .clone_from(&updated_row);
        if let Some(task_uuid) = job.task_uuid {
            context.rows.insert(task_uuid, updated_row.clone());
        }
        job.row = updated_row;
        self.job_tokens
            .borrow_mut()
            .insert(job_token_hash, job.job_id);
        Ok(Some(PreparedExecution {
            job_token: Some(job_token),
        }))
    }

    pub(super) fn emit_post_ack(&self, event: EmitEvent) {
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
        let mut change_payload = json!({
            "taskUuid": fields.task_uuid,
            "attempt": fields.attempt,
            "leaseEpoch": fields.lease_epoch,
            "event": fields.event,
            "lifecycleCursor": lifecycle.cursor,
        });
        if let Some(task_ref) = &fields.task_ref {
            change_payload["taskRef"] = Value::String(task_ref.to_string());
        }
        let mut job_change_payload = json!({
            "taskUuid": fields.task_uuid,
            "attempt": fields.attempt,
            "leaseEpoch": fields.lease_epoch,
            "reason": fields.event,
        });
        if let Some(task_ref) = &fields.task_ref {
            job_change_payload["taskRef"] = Value::String(task_ref.to_string());
        }
        let mut changes = self.changes.borrow_mut();
        let mut append_change = |kind, payload| changes.append_now(kind, payload);
        let changed = append_change(ChangeKind::Lifecycle, change_payload.clone())
            .and_then(|_| append_change(ChangeKind::Job, job_change_payload))
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

/// The identity of one task generation's terminal forge projection.
///
/// Startup re-drive and the live post-ack worker must agree on this string, or
/// the same projection would be recorded as two different orphans.
pub(super) fn gh_completion_id(task_uuid: Uuid, attempt: u32, witness_seq: u64) -> String {
    format!("{task_uuid}:{attempt}:{witness_seq}")
}

/// The command that lists every orphaned projection in one pass.
pub(super) fn orphan_listing_command(state_dir: &Path) -> String {
    format!(
        "tally producer orphaned --state-dir {}",
        state_dir.display()
    )
}

pub(super) const ORPHANED_ATTESTATION_KIND: &str = "projection-orphaned";
pub(super) const ORPHAN_RETRACTED_ATTESTATION_KIND: &str = "projection-orphan-retracted";

/// Witness the terminal outcome of one projection on the advisory chain.
///
/// An orphan is witnessed once per identity and a retraction once per
/// identity, and both are decided by asking the chain rather than by asking
/// the record file. That is deliberate. Deriving "already witnessed" from the
/// record file made two states unreachable: a record written by an observation
/// that then died before appending could never be witnessed afterwards, and a
/// record collected by retention and re-derived on a later start would be
/// witnessed twice. The chain is the thing the claim lives on, so the chain is
/// what is asked.
pub(super) fn witness_projection_outcome(
    attestations: &Arc<std::sync::Mutex<SharedAttestations>>,
    outcome: &GhProjectionOutcome,
    attempt: Option<u32>,
    lease_epoch: Option<u64>,
) {
    let mut attestations = attestations
        .lock()
        .expect("attestation ledger lock poisoned");
    let appended = match outcome {
        GhProjectionOutcome::Orphaned { record } => {
            append_orphan_attestation(&mut attestations, record, attempt, lease_epoch)
        }
        GhProjectionOutcome::Settled {
            retracted: Some(record),
        } => append_orphan_retraction(&mut attestations, record, attempt, lease_epoch),
        GhProjectionOutcome::Settled { retracted: None } => Ok(()),
    };
    if let Err(error) = appended {
        // Not fatal, and not lost either: the next observation of the same
        // projection asks the chain again and appends what is still missing.
        eprintln!("tally: orphaned-projection attestation failed: {error}");
    }
}

/// Append the terminal claim for one orphaned projection, unless the chain
/// already carries it.
pub(super) fn append_orphan_attestation(
    attestations: &mut SharedAttestations,
    record: &OrphanedProjection,
    attempt: Option<u32>,
    lease_epoch: Option<u64>,
) -> Result<(), WitnessError> {
    let mut payload = orphan_attestation_payload(ORPHANED_ATTESTATION_KIND, record);
    payload["retryAuthority"] = Value::from("terminal-no-retry");
    append_orphan_payload(attestations, record, payload, attempt, lease_epoch)
}

/// Append the retraction of a claim this tree has been shown to have made
/// wrongly.
///
/// The chain is append-only, so a false claim cannot be erased; the only
/// honest correction is a later record naming the same identity. This fires
/// when a projection that was recorded as orphaned turns out to have reached
/// the forge after all — which is exactly what an estate carrying records
/// written before the idempotency marker was consulted will find.
pub(super) fn append_orphan_retraction(
    attestations: &mut SharedAttestations,
    record: &OrphanedProjection,
    attempt: Option<u32>,
    lease_epoch: Option<u64>,
) -> Result<(), WitnessError> {
    let mut payload = orphan_attestation_payload(ORPHAN_RETRACTED_ATTESTATION_KIND, record);
    payload["retracts"] = Value::from(ORPHANED_ATTESTATION_KIND);
    payload["reason"] = Value::from("projection-settled");
    append_orphan_payload(attestations, record, payload, attempt, lease_epoch)
}

fn orphan_attestation_payload(kind: &str, record: &OrphanedProjection) -> Value {
    json!({
        "kind": kind,
        "schemaVersion": ORPHANED_PROJECTION_SCHEMA_VERSION,
        "projection": record.kind.as_str(),
        "producer": record.producer,
        "source": record.source,
        "itemId": record.item_id,
        "completionId": record.completion_id,
        "observedAt": record.observed_at,
        "detail": record.detail,
    })
}

fn append_orphan_payload(
    attestations: &mut SharedAttestations,
    record: &OrphanedProjection,
    mut payload: Value,
    attempt: Option<u32>,
    lease_epoch: Option<u64>,
) -> Result<(), WitnessError> {
    if let Some(task_uuid) = &record.task_uuid {
        payload["taskUuid"] = Value::String(task_uuid.clone());
    }
    if let Some(verdict) = record.verdict {
        payload["verdict"] =
            serde_json::to_value(verdict).expect("verdict always serializes to a string");
    }
    if let Some(attempt) = attempt {
        payload["attempt"] = Value::from(attempt);
    }
    if let Some(lease_epoch) = lease_epoch {
        payload["leaseEpoch"] = Value::from(lease_epoch);
    }
    let kind = payload["kind"]
        .as_str()
        .expect("payload kind is set by the caller")
        .to_owned();
    let ledger = attestations.ledger()?;
    if chain_carries_projection_claim(ledger.records()?, &kind, record) {
        return Ok(());
    }
    attestations.ledger()?.append(payload).map(|_| ())
}

/// Whether the chain already carries a claim of this kind for this exact
/// projection identity.
fn chain_carries_projection_claim(
    records: &[crate::witness::AttestationRecord],
    kind: &str,
    record: &OrphanedProjection,
) -> bool {
    records.iter().any(|attested| {
        let payload = &attested.payload;
        payload.get("kind").and_then(Value::as_str) == Some(kind)
            && payload.get("projection").and_then(Value::as_str) == Some(record.kind.as_str())
            && payload.get("producer").and_then(Value::as_str) == Some(record.producer.as_str())
            && payload.get("source").and_then(Value::as_str) == Some(record.source.as_str())
            && payload.get("itemId").and_then(Value::as_str) == Some(record.item_id.as_str())
            && payload.get("completionId").and_then(Value::as_str)
                == Some(record.completion_id.as_str())
    })
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

pub(super) fn execution_request(
    executor: &Executor,
    job: &Job,
    limits: UnitLimits,
    local_environment: (&str, Option<&str>),
    brief_root: &Path,
    git_ai_config: &GitAiConfig,
    exec_attestations: bool,
) -> Result<ExecutionRequest, ExecutorError> {
    let (tally_socket, job_token) = local_environment;
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
            if let Some(task_ref) = orchestration.task_ref() {
                attributes.insert("taskRef".to_owned(), task_ref.to_string());
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
        job_token: job
            .row
            .executor
            .is_none()
            .then_some(job_token)
            .flatten()
            .map(str::to_owned),
        environment: job.invocation.env.clone(),
        gh_origin: job.row.gh_origin.clone(),
        brief_hash: job.row.brief_hash.clone(),
        brief_path,
        brief_document: None,
        cwd: job.row.effective_cwd().map(Path::to_path_buf),
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
        extra_writable_paths: job.invocation.extra_writable_paths.clone(),
        credentials: job.row.credentials.clone(),
        limits,
        runtime_max_sec: job.row.runtime_max_sec,
    })
}

fn mint_job_token() -> Result<String, DaemonError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| DaemonError::Invalid(format!("job token entropy failed: {error}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(super) fn hash_job_token(job_token: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(job_token.as_bytes()))
}

pub(super) fn effective_gate_manifest(
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

pub(super) fn execution_fact_for_termination(termination: &ExecutionTermination) -> ExecutionFact {
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

pub(super) fn enqueued_event(job: &Job) -> EmitEvent {
    let mut event = EmitEvent::enqueued(job.stable_key(), job.row.priority, job.row.source);
    event.task_ref = job.task_ref();
    event.agent = Some(job.row.adapter.clone());
    event.session_ref.clone_from(&job.row.session_ref);
    event.unit = Some(job.identity().unit_name());
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
        task_ref: job.task_ref(),
        class: job.row.priority,
        source: job.row.source,
        message: None,
        agent: Some(job.row.adapter.clone()),
        session_ref: job.row.session_ref.clone(),
        unit: Some(job.identity().unit_name()),
        exit_code: None,
        stderr_tail: None,
        stderr_truncated: None,
        gpu_seconds: None,
        context_tokens: None,
        context_window: None,
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

pub(super) fn canonical_job_model(job: &Job) -> Option<String> {
    job.row.adapter_options.model.clone().or_else(|| {
        if job.model_is_advisory {
            None
        } else {
            job.row.model.clone()
        }
    })
}

/// The witness's resource facts for one completion, from whatever the exit
/// recorder's accounting probe measured.
///
/// `charge` is the generic per-job cost — CPU-seconds, whenever a probe
/// succeeded, regardless of which pool the job ran in. `gpu_seconds` is
/// narrower: it is set only for a job whose pool **explicitly** declared
/// `resource = "vram"` (`gpu_pool_job` must already reflect that — see
/// `LeaseEngine::declared_resource_kind`, never
/// `PoolConfig::resource()`'s defaulted reading, since `vram` is
/// `ResourceKind`'s own default and a pool that declared nothing must not
/// register as a GPU pool). It is the unit's main-process wall-clock runtime
/// (`UnitAccounting::wall_seconds`), not CPU-cgroup time and not exact pool
/// occupancy either — the pool lease is held from admission through
/// completion handling, a window that strictly contains the main process's
/// lifetime, so this is a lower bound on occupancy. It is still the right
/// quantity to prefer over CPU-cgroup time, which would understate a
/// GPU-bound job that mostly waits on the device by a much larger margin —
/// wrong in exactly the reassuring direction. Neither field is ever a
/// fabricated value: an unmeasured or non-GPU-pool input yields `None`,
/// never `Some(0.0)`.
pub(super) fn accounting_witness_fields(
    accounting: Option<UnitAccounting>,
    gpu_pool_job: bool,
) -> (Option<Charge>, Option<f64>) {
    let charge = accounting
        .and_then(UnitAccounting::cpu_seconds)
        .map(|amount| Charge {
            unit: "cpu-second".to_owned(),
            amount,
            class_name: "measured".to_owned(),
        });
    let gpu_seconds = gpu_pool_job
        .then(|| accounting.and_then(UnitAccounting::wall_seconds))
        .flatten();
    (charge, gpu_seconds)
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

pub(super) fn lock_gcroot_registration(paths: &DaemonPaths) -> Result<GcRootsLock, WitnessError> {
    acquire_registration_lock(&paths.gcroots_dir()).map_err(|source| WitnessError::Io {
        path: gcroots_lock_path(&paths.gcroots_dir()),
        source,
    })
}

pub(super) fn append_daemon_witness(
    ledger: &mut WitnessLedger,
    paths: &DaemonPaths,
    body: WitnessBody,
) -> Result<WitnessRecord, WitnessError> {
    let _lock = lock_gcroot_registration(paths)?;
    let record = ledger.append(body)?;
    log_gcroot_registration_failures(&record, paths);
    Ok(record)
}

pub(super) fn append_context_witness(
    context: &mut Context,
    body: WitnessBody,
) -> Result<WitnessRecord, WitnessError> {
    let _lock = lock_gcroot_registration(&context.paths)?;
    let record = context.witness.append(body)?;
    log_gcroot_registration_failures(&record, &context.paths);
    Ok(record)
}

pub(super) fn forced_witness(job: &Job, verdict: Verdict, host_id: Option<String>) -> WitnessBody {
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
        error: None,
        result_revision: None,
        authorship: None,
        authorship_sessions: None,
    }
}

pub(super) fn substituted_witness(row: &RowSeed, drv: Derivation) -> WitnessBody {
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
        error: None,
        result_revision: None,
        authorship: None,
        authorship_sessions: None,
    }
}

pub(super) fn release_child_charge(context: &mut Context, job: &Job) -> Result<(), DaemonError> {
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

pub(super) fn finalize_forced_locked(
    context: &mut Context,
    job_id: Uuid,
    verdict: Verdict,
    release_lease: bool,
    scrape_capture: bool,
) -> Result<Option<TerminalWork>, DaemonError> {
    let Some(job) = context.jobs.get(&job_id).cloned() else {
        // A job that already reached a terminal disposition is retired out of
        // the live map (#395), so its absence there is the same fact the
        // `Completed` check below reports: nothing left to force. An id the
        // daemon has never heard of is still an error, which is why this asks
        // the maps that do keep terminal jobs rather than answering `Ok(None)`
        // for anything at all.
        return if context.rows.contains_key(&job_id) || context.query_rows.contains_key(&job_id) {
            Ok(None)
        } else {
            Err(DaemonError::Invalid(format!(
                "unknown forced-terminal job {job_id}"
            )))
        };
    };
    if job.state == JobState::Completed {
        return Ok(None);
    }
    let host_id = (job.state == JobState::Running && job.row.executor.is_none())
        .then(|| context.host_id.clone());
    let record = append_context_witness(context, forced_witness(&job, verdict, host_id))?;
    let result = JobResult {
        gpu_seconds: None,
        task_uuid: job.task_uuid.map(|uuid| uuid.to_string()),
        task_ref: job.task_ref(),
        job_id: job.job_id.to_string(),
        verdict,
        exit_code: if verdict == Verdict::Cancelled { 0 } else { 1 },
        artifact_content_hash: None,
        attempt: job.row.attempt,
        lease_epoch: job.row.lease_epoch,
        witness_seq: record.seq,
        model: record.model.clone(),
        completion: None,
        error: None,
        stderr_excerpt: None,
    };
    context
        .barriers
        .complete_job(&job.stable_key(), result.value());
    // Terminal: the job leaves the live map (#395). `release_lease` used to
    // clear the stored lease id; retiring the whole entry subsumes it, and the
    // lease itself is still released below from the `job` clone.
    retire_job(context, job_id);
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
        task_ref: job.task_ref(),
        class: job.row.priority,
        source: job.row.source,
        message: Some(check.reason.clone()),
        agent: Some(job.row.adapter.clone()),
        session_ref: job.row.session_ref.clone(),
        unit: Some(job.identity().unit_name()),
        exit_code: None,
        stderr_tail: None,
        stderr_truncated: None,
        gpu_seconds: None,
        context_tokens: None,
        context_window: None,
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

pub(super) fn completed_event(job: &Job, result: &JobResult, evidence: String) -> EmitEvent {
    EmitEvent {
        event: terminal_lifecycle_event(result.verdict, result.artifact_content_hash.is_some()),
        task_uuid: job.stable_key(),
        task_ref: job.task_ref(),
        class: job.row.priority,
        source: job.row.source,
        message: None,
        agent: Some(job.row.adapter.clone()),
        session_ref: job.row.session_ref.clone(),
        unit: Some(job.identity().unit_name()),
        exit_code: Some(result.exit_code),
        stderr_tail: result
            .stderr_excerpt
            .as_ref()
            .map(|excerpt| excerpt.text.clone()),
        stderr_truncated: result
            .stderr_excerpt
            .as_ref()
            .map(|excerpt| excerpt.truncated),
        gpu_seconds: result.gpu_seconds,
        context_tokens: job.row.context_tokens,
        context_window: job.row.context_window.as_ref().map(|window| window.tokens),
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

pub(super) fn canonical_verdict(
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
