use super::super::*;

impl DaemonHandler {
    pub(crate) async fn await_job(&self, params: Option<Value>) -> Result<Value, WireError> {
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
                    Some(reconstruct_job_result(
                        &mut context,
                        &self.executor,
                        &stable,
                        resolved_attempt,
                    )),
                )
            }
        };
        if let Some(registration) = registration {
            return await_registration(registration).await;
        }
        witness_lookup.expect("terminal witness lookup was selected above")
    }

    pub(crate) async fn await_barrier(&self, params: Option<Value>) -> Result<Value, WireError> {
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
                    Some(reconstruct_job_result(
                        &mut context,
                        &self.executor,
                        &stable,
                        Some(attempt),
                    )),
                    stable.clone(),
                )
            }
        };
        let result = if let Some(registration) = registration {
            await_registration(registration).await?
        } else {
            witness_lookup.expect("terminal barrier lookup was selected above")?
        };
        Ok(single_job_barrier_value(&params.barrier, &stable, result))
    }

    pub(crate) async fn drain(&self, params: Option<Value>) -> Result<Value, WireError> {
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
                .enqueue_payload(
                    payload,
                    Some(claim.ingress_id.clone()),
                    CallerIdentity::Client,
                )
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

    pub(crate) async fn pause(&self, params: Option<Value>) -> Result<Value, WireError> {
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

    pub(crate) async fn resume(&self, params: Option<Value>) -> Result<Value, WireError> {
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

    pub(crate) async fn cancel(&self, params: Option<Value>) -> Result<Value, WireError> {
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

    pub(crate) async fn cancel_flow(&self, flow_run_id: &str) -> Result<Value, WireError> {
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

    pub(crate) async fn cancel_one(
        &self,
        task_uuid: &str,
        force: bool,
    ) -> Result<Value, WireError> {
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

    pub(crate) async fn acquire(&self, params: Option<Value>) -> Result<Value, WireError> {
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

    pub(crate) async fn release(&self, params: Option<Value>) -> Result<Value, WireError> {
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

    pub(crate) async fn lease_status(&self, params: Option<Value>) -> Result<Value, WireError> {
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
}

pub(crate) fn lease_wire(error: LeaseError) -> WireError {
    match error {
        LeaseError::UnknownPool(_)
        | LeaseError::InvalidRequest(_)
        | LeaseError::StaleEpoch { .. } => WireError::invalid(error.to_string()),
        LeaseError::NotFound(_) => WireError::not_found(error.to_string()),
        other => internal_wire(other),
    }
}

fn reconstruct_job_result(
    context: &mut Context,
    executor: &Executor,
    stable: &str,
    attempt: Option<u32>,
) -> Result<Value, WireError> {
    let record = context
        .witness_view
        .latest_for_task(stable, attempt)
        .map_err(internal_wire)?
        .ok_or_else(|| {
            let suffix = attempt.map_or_else(String::new, |attempt| format!(" attempt {attempt}"));
            WireError::not_found(format!("job {stable}{suffix} has no terminal witness"))
        })?;
    Ok(job_result_from_witness(&record, executor).value())
}

fn job_result_from_witness(record: &WitnessRecord, executor: &Executor) -> JobResult {
    let stable = record
        .task_uuid
        .clone()
        .expect("durable wait reconstruction selected a task witness");
    let stderr_excerpt = if is_adapter_smoke(record.evidence_class.as_ref())
        && !matches!(
            record.verdict,
            Verdict::Pass | Verdict::Reused | Verdict::Substituted
        ) {
        Uuid::parse_str(&stable).ok().and_then(|uuid| {
            let identity = ExecutionIdentity {
                job_id: uuid,
                task_uuid: Some(uuid),
            };
            executor
                .retained_capture_paths(&identity, record.attempt, record.lease_epoch)
                .ok()
                .flatten()
                .and_then(|paths| crate::executor::read_capture_excerpt(&paths.stderr).ok())
        })
    } else {
        None
    };
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
        stderr_excerpt,
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

pub(crate) fn find_job<'a>(context: &'a Context, presented: &str) -> Result<&'a Job, WireError> {
    context
        .aliases
        .get(presented)
        .and_then(|job_id| context.jobs.get(job_id))
        .ok_or_else(|| WireError::not_found(format!("job {presented} was not found")))
}

pub(crate) fn lease_request(job: &Job, unit: String) -> LeaseRequest {
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

pub(crate) fn state_name(state: JobState) -> &'static str {
    match state {
        JobState::Paused => "paused",
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Completed => "completed",
    }
}
