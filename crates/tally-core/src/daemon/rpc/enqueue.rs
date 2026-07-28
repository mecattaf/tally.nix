use super::super::*;

impl DaemonHandler {
    /// Submit as an operator, the class a connection presenting no capability
    /// token falls into. Tests that exercise job-originated admission call
    /// [`DaemonHandler::enqueue`] with a `CallerIdentity::Job` instead.
    #[cfg(test)]
    pub(crate) async fn enqueue_as_client(
        &self,
        params: Option<Value>,
    ) -> Result<Value, WireError> {
        self.enqueue(params, CallerIdentity::Client).await
    }

    #[cfg(test)]
    pub(crate) async fn continue_job_as_client(
        &self,
        params: Option<Value>,
    ) -> Result<Value, WireError> {
        self.continue_job(params, CallerIdentity::Client).await
    }

    pub(crate) async fn enqueue(
        &self,
        params: Option<Value>,
        caller: CallerIdentity,
    ) -> Result<Value, WireError> {
        let payload: EnqueuePayload = decode_params(params)?;
        self.enqueue_payload(payload, None, caller).await
    }

    pub(crate) async fn continue_job(
        &self,
        params: Option<Value>,
        caller: CallerIdentity,
    ) -> Result<Value, WireError> {
        let payload: EnqueuePayload = decode_params(params)?;
        if payload.resume_from.is_none() {
            return Err(WireError::invalid(
                "queue.continue requires a resumeFrom task UUID",
            ));
        }
        self.enqueue_payload(payload, None, caller).await
    }

    pub(crate) async fn retry_job(&self, params: Option<Value>) -> Result<Value, WireError> {
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
        row.job_token_hash = None;

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
        event.row.job_token_hash = None;
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

    pub(crate) async fn enqueue_payload(
        &self,
        mut payload: EnqueuePayload,
        ingress_id: Option<String>,
        caller: CallerIdentity,
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
        payload.caller_job_token = None;
        let mut context = self.context.write().await;
        if let Some(token_job_id) = caller.job() {
            let resolved = stable_parent_key(&context, &token_job_id.to_string()).ok_or_else(
                || {
                    WireError::invalid(
                        "callerJobToken is not a live job capability; it was never minted or has been revoked",
                    )
                },
            )?;
            // The token, not the request, decides who the caller is. A caller
            // that also names itself must name the identity the token already
            // resolved to; a mismatch is a sibling-impersonation attempt.
            if payload.caller_job_id.as_deref().is_some_and(|presented| {
                stable_parent_key(&context, presented).as_deref() != Some(resolved.as_str())
            }) {
                return Err(WireError::invalid(
                    "callerJobId is not accepted as authorization; identity derives from TALLY_JOB_TOKEN",
                ));
            }
            payload.caller_job_id = Some(resolved);
        }
        let caller_job_id = payload.caller_job_id.clone();
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
            job_token_hash: None,
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
}

/// Canonical guardrail key for a presented job reference, without registering it.
///
/// `ensure_guardrail_parent` performs the same alias/UUID resolution but mutates
/// the guardrail table as a side effect. Comparing a presented `callerJobId`
/// against a token-resolved identity has to happen before that registration, so
/// that a rejected impersonation attempt leaves no parent entry behind.
fn stable_parent_key(context: &Context, presented: &str) -> Option<String> {
    if let Some(info) = context.guardrails.parent(presented) {
        return Some(info.parent_uuid.clone());
    }
    let job_id = context
        .aliases
        .get(presented)
        .copied()
        .or_else(|| Uuid::parse_str(presented).ok())?;
    context
        .jobs
        .get(&job_id)
        .map(|job| &job.row)
        .or_else(|| context.rows.get(&job_id))
        .map(|row| row.uuid.to_string())
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
