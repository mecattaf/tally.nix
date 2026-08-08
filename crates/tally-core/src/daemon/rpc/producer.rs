use super::super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct PoolLossIntent {
    schema_version: u32,
    pub(crate) row: RowSeed,
    labor_class: LaborClass,
    adopted_invocation_id: Option<String>,
    model_is_advisory: bool,
}

pub(crate) type PoolTransitionTask = JoinHandle<Result<(), WireError>>;

impl DaemonHandler {
    pub(crate) async fn producer_runtime_observed(
        &self,
        params: Option<Value>,
    ) -> Result<Value, WireError> {
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

    pub(crate) async fn pool_transition(&self, params: Option<Value>) -> Result<Value, WireError> {
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

    pub(crate) async fn pool_transition_inner(
        &self,
        params: Option<Value>,
    ) -> Result<Value, WireError> {
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
                &context.paths.data_dir,
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

    pub(crate) async fn apply_pool_loss(&self, pool: &str) -> Result<usize, WireError> {
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

    pub(crate) async fn apply_pool_return(&self, pool: &str) -> Result<usize, WireError> {
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
            // No progress sink: this is the runtime pool-return path, where no
            // start timeout exists to renew. (Its corpus-scale cost is #431's
            // territory, not #428's.)
            let units = collect_local_unit_facts(&self.executor, &durable, || {})
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
                &mut self
                    .attestations
                    .lock()
                    .expect("attestation ledger lock poisoned"),
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
            &mut self
                .attestations
                .lock()
                .expect("attestation ledger lock poisoned"),
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
        let mut launches = install_recovery_jobs(
            &mut context,
            &plan,
            &self.executor,
            &mut self
                .attestations
                .lock()
                .expect("attestation ledger lock poisoned"),
        )
        .map_err(|error| self.fail_stop(error))?;
        // Two kinds of member leave the set here. A live paused job whose
        // pools are all reachable again is collected to resume. A uuid whose
        // job is absent from `context.jobs` was retired at a terminal
        // disposition (#395); it can never be resumed, so any sweep of the
        // set drops it. Before #420 that second kind never left: this GC read
        // only the live map, which no longer retains terminal jobs, so every
        // pool-loss-paused job that then completed or was cancelled pinned
        // its uuid here for the daemon's lifetime.
        let mut paused = Vec::new();
        let mut retired = Vec::new();
        for job_id in &context.unreachable_paused_jobs {
            match context.jobs.get(job_id) {
                Some(job) => {
                    if job.row.pools.iter().any(|name| name == pool)
                        && !job
                            .row
                            .pools
                            .iter()
                            .any(|name| context.unreachable_pools.contains(name))
                    {
                        paused.push(*job_id);
                    }
                }
                None => retired.push(*job_id),
            }
        }
        for job_id in paused.iter().chain(&retired) {
            context.unreachable_paused_jobs.remove(job_id);
        }
        launches.extend(resume_paused_jobs_locked(
            &mut context,
            &self.executor,
            paused,
        ));
        drop(context);

        for job in launches {
            self.spawn_execution(job);
        }
        Ok(represented_rows.len())
    }

    pub(crate) async fn drain_pool_transition_tasks(&self) -> Result<(), DaemonError> {
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
}

const MAX_POOL_LOSS_INTENT_BYTES: u64 = 1024 * 1024;

pub(crate) fn pool_loss_intent_directory(state_dir: &Path) -> PathBuf {
    state_dir.join("producers/pool-loss-intents")
}

pub(crate) fn write_pool_loss_intent(state_dir: &Path, job: &Job) -> Result<PathBuf, DaemonError> {
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

pub(crate) fn read_pool_loss_intent(path: &Path) -> Result<PoolLossIntent, DaemonError> {
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

pub(crate) async fn reconcile_pool_loss_intents(
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
            task_ref: intent
                .row
                .orchestration
                .as_ref()
                .and_then(Orchestration::task_ref),
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
                extra_writable_paths: Vec::new(),
                yield_hook: None,
                usage_accounting: UsageAccountingMode::Fresh,
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
